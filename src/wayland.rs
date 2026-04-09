// SPDX-License-Identifier: GPL-3.0-only

use std::{
    collections::HashSet,
    os::{
        fd::{FromRawFd, RawFd},
        unix::net::UnixStream,
    },
    sync::{LazyLock, Mutex},
};

use cosmic::{
    cctk::{
        self,
        cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1,
        cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
        sctk::{
            self,
            output::{OutputHandler, OutputState},
            reexports::{calloop, calloop_wayland_source::WaylandSource},
            registry::{ProvidesRegistryState, RegistryState},
            seat::{SeatHandler, SeatState},
        },
        toplevel_info::{ToplevelInfo, ToplevelInfoHandler, ToplevelInfoState},
        toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
        wayland_client::{
            Connection, QueueHandle, WEnum,
            globals::registry_queue_init,
            protocol::{wl_output::WlOutput, wl_seat::WlSeat},
        },
        wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        wayland_protocols::ext::workspace::v1::client::ext_workspace_handle_v1,
        workspace::{WorkspaceHandler, WorkspaceState},
    },
    iced::Subscription,
};
use futures::{SinkExt, channel::mpsc, executor::block_on};
use iced_futures::stream;

static WAYLAND_REQUEST_TX: LazyLock<Mutex<Option<calloop::channel::Sender<WaylandRequest>>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceWindow {
    pub handle: ExtForeignToplevelHandleV1,
    pub title: String,
    pub app_id: Option<String>,
    pub identifier: Option<String>,
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub enum WaylandRequest {
    Activate(ExtForeignToplevelHandleV1),
}

#[derive(Debug, Clone)]
pub enum WaylandUpdate {
    WorkspaceWindows(Vec<WorkspaceWindow>),
    Finished,
}

pub fn focus_window(handle: ExtForeignToplevelHandleV1) {
    let sender = WAYLAND_REQUEST_TX
        .lock()
        .ok()
        .and_then(|guard| guard.clone());

    if let Some(sender) = sender {
        let _ = sender.send(WaylandRequest::Activate(handle));
    }
}

pub fn workspace_windows_subscription() -> Subscription<WaylandUpdate> {
    Subscription::run_with(std::any::TypeId::of::<WaylandUpdate>(), |_| {
        let (request_tx, request_rx) = calloop::channel::channel();
        if let Ok(mut guard) = WAYLAND_REQUEST_TX.lock() {
            *guard = Some(request_tx);
        }

        stream::channel(1, move |output: mpsc::Sender<WaylandUpdate>| async move {
            let _ = std::thread::Builder::new()
                .name("workspace-window-list-wayland".into())
                .spawn(move || {
                    wayland_event_loop(output.clone(), request_rx);
                });

            futures::future::pending::<()>().await;
        })
    })
}

enum OutputScope<'a> {
    Any,
    Pending,
    Specific(&'a WlOutput),
}

struct AppData {
    tx: mpsc::Sender<WaylandUpdate>,
    registry_state: RegistryState,
    // Keep outputs before workspaces so workspace state always receives output updates.
    output_state: OutputState,
    workspace_state: WorkspaceState,
    toplevel_info_state: ToplevelInfoState,
    toplevel_manager_state: Option<ToplevelManagerState>,
    seat_state: SeatState,
    configured_output: Option<String>,
    expected_output: Option<WlOutput>,
    workspaces_ready: bool,
    last_windows: Vec<WorkspaceWindow>,
}

impl AppData {
    fn trimmed(value: &str) -> Option<String> {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    }

    fn window_for(info: &ToplevelInfo) -> Option<WorkspaceWindow> {
        let app_id = Self::trimmed(&info.app_id);
        let title = Self::trimmed(&info.title).or_else(|| app_id.clone())?;
        let identifier = Self::trimmed(&info.identifier);

        Some(WorkspaceWindow {
            handle: info.foreign_toplevel.clone(),
            title,
            app_id,
            identifier,
            is_active: info
                .state
                .contains(&zcosmic_toplevel_handle_v1::State::Activated),
        })
    }

    fn cosmic_toplevel(
        &self,
        handle: &ExtForeignToplevelHandleV1,
    ) -> Option<cctk::cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1>{
        self.toplevel_info_state
            .info(handle)?
            .cosmic_toplevel
            .clone()
    }

    fn activate_toplevel(&self, handle: &ExtForeignToplevelHandleV1) {
        let Some(manager_state) = self.toplevel_manager_state.as_ref() else {
            return;
        };
        let Some(cosmic_toplevel) = self.cosmic_toplevel(handle) else {
            return;
        };
        let Some(seat) = self.seat_state.seats().next() else {
            return;
        };

        manager_state.manager.activate(&cosmic_toplevel, &seat);
    }

    fn output_scope(&self) -> OutputScope<'_> {
        match (
            self.configured_output.as_ref(),
            self.expected_output.as_ref(),
        ) {
            (Some(_), Some(output)) => OutputScope::Specific(output),
            (Some(_), None) => OutputScope::Pending,
            (None, _) => OutputScope::Any,
        }
    }

    fn matches_output(info: &ToplevelInfo, scope: &OutputScope<'_>) -> bool {
        match scope {
            OutputScope::Any => true,
            OutputScope::Pending => false,
            OutputScope::Specific(output) => info.output.contains(*output),
        }
    }

    fn active_workspaces(
        &self,
        scope: &OutputScope<'_>,
    ) -> HashSet<ext_workspace_handle_v1::ExtWorkspaceHandleV1> {
        if !self.workspaces_ready {
            return HashSet::new();
        }

        self.workspace_state
            .workspace_groups()
            .filter(|group| match scope {
                OutputScope::Any => true,
                OutputScope::Pending => false,
                OutputScope::Specific(output) => group
                    .outputs
                    .iter()
                    .any(|group_output| group_output == *output),
            })
            .flat_map(|group| group.workspaces.iter())
            .filter_map(|handle| self.workspace_state.workspace_info(handle))
            .filter(|workspace| {
                workspace
                    .state
                    .contains(ext_workspace_handle_v1::State::Active)
            })
            .map(|workspace| workspace.handle.clone())
            .collect()
    }

    fn matches_workspace(
        info: &ToplevelInfo,
        active_workspaces: &HashSet<ext_workspace_handle_v1::ExtWorkspaceHandleV1>,
    ) -> bool {
        if info
            .state
            .contains(&zcosmic_toplevel_handle_v1::State::Sticky)
        {
            return true;
        }

        if active_workspaces.is_empty() {
            return false;
        }

        active_workspaces
            .iter()
            .any(|workspace| info.workspace.contains(workspace))
    }

    fn current_windows(&self) -> Vec<WorkspaceWindow> {
        let output_scope = self.output_scope();
        if matches!(output_scope, OutputScope::Pending) || !self.workspaces_ready {
            return Vec::new();
        }

        let active_workspaces = self.active_workspaces(&output_scope);

        self.toplevel_info_state
            .toplevels()
            .filter(|info| Self::matches_output(info, &output_scope))
            .filter(|info| Self::matches_workspace(info, &active_workspaces))
            .filter_map(Self::window_for)
            .collect()
    }

    fn publish_windows(&mut self) {
        let windows = self.current_windows();
        if windows == self.last_windows {
            return;
        }

        self.last_windows = windows.clone();
        let _ = block_on(self.tx.send(WaylandUpdate::WorkspaceWindows(windows)));
    }

    fn sync_expected_output(&mut self, output: &WlOutput) -> bool {
        let Some(configured_output) = self.configured_output.as_deref() else {
            return false;
        };

        let matches_output = self
            .output_state
            .info(output)
            .and_then(|info| info.name.clone())
            .as_deref()
            .is_some_and(|name| name == configured_output);

        if matches_output {
            let changed = match self.expected_output.as_ref() {
                Some(current) => current != output,
                None => true,
            };
            if changed {
                self.expected_output = Some(output.clone());
            }
            return changed;
        }

        let was_expected = self
            .expected_output
            .as_ref()
            .is_some_and(|current| current == output);
        if was_expected {
            self.expected_output = None;
        }
        was_expected
    }
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    sctk::registry_handlers![OutputState,];
}

impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: WlOutput) {
        if self.sync_expected_output(&output) {
            self.publish_windows();
        }
    }

    fn update_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: WlOutput) {
        if self.sync_expected_output(&output) {
            self.publish_windows();
        }
    }

    fn output_destroyed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: WlOutput) {
        let was_expected = self
            .expected_output
            .as_ref()
            .is_some_and(|current| current == &output);
        if was_expected {
            self.expected_output = None;
            self.publish_windows();
        }
    }
}

impl WorkspaceHandler for AppData {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.workspace_state
    }

    fn done(&mut self) {
        self.workspaces_ready = true;
        self.publish_windows();
    }
}

impl SeatHandler for AppData {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: WlSeat,
        _capability: sctk::seat::Capability,
    ) {
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: WlSeat,
        _capability: sctk::seat::Capability,
    ) {
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: WlSeat) {}
}

impl ToplevelManagerHandler for AppData {
    fn toplevel_manager_state(&mut self) -> &mut ToplevelManagerState {
        self.toplevel_manager_state
            .as_mut()
            .expect("toplevel manager not initialized")
    }

    fn capabilities(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _capabilities: Vec<
            WEnum<zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1>,
        >,
    ) {
    }
}

impl ToplevelInfoHandler for AppData {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }

    fn new_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _toplevel: &cctk::wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        self.publish_windows();
    }

    fn update_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _toplevel: &cctk::wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        self.publish_windows();
    }

    fn toplevel_closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _toplevel: &cctk::wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        self.publish_windows();
    }

    fn info_done(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>) {
        self.publish_windows();
    }
}

fn wayland_event_loop(
    tx: mpsc::Sender<WaylandUpdate>,
    requests: calloop::channel::Channel<WaylandRequest>,
) {
    let finished_tx = tx.clone();

    let socket = std::env::var("X_PRIVILEGED_WAYLAND_SOCKET")
        .ok()
        .and_then(|fd| {
            fd.parse::<RawFd>()
                .ok()
                .map(|fd| unsafe { UnixStream::from_raw_fd(fd) })
        });

    let conn = match socket {
        Some(socket) => Connection::from_socket(socket),
        None => Connection::connect_to_env(),
    };

    let conn = match conn {
        Ok(conn) => conn,
        Err(err) => {
            tracing::error!("Failed to connect to Wayland: {err}");
            let _ = block_on(finished_tx.clone().send(WaylandUpdate::Finished));
            return;
        }
    };

    let (globals, event_queue) = match registry_queue_init(&conn) {
        Ok(parts) => parts,
        Err(err) => {
            tracing::error!("Failed to initialize the Wayland registry: {err}");
            let _ = block_on(finished_tx.clone().send(WaylandUpdate::Finished));
            return;
        }
    };

    let mut event_loop = match calloop::EventLoop::<AppData>::try_new() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            tracing::error!("Failed to create the Wayland event loop: {err}");
            let _ = block_on(finished_tx.clone().send(WaylandUpdate::Finished));
            return;
        }
    };

    let qh = event_queue.handle();
    if WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .is_err()
    {
        tracing::error!("Failed to register the Wayland source");
        let _ = block_on(finished_tx.clone().send(WaylandUpdate::Finished));
        return;
    }

    if event_loop
        .handle()
        .insert_source(requests, |event, (), state| match event {
            calloop::channel::Event::Msg(WaylandRequest::Activate(handle)) => {
                state.activate_toplevel(&handle);
            }
            calloop::channel::Event::Closed => {}
        })
        .is_err()
    {
        tracing::error!("Failed to register the applet request channel");
        let _ = block_on(finished_tx.clone().send(WaylandUpdate::Finished));
        return;
    }

    let registry_state = RegistryState::new(&globals);
    let output_state = OutputState::new(&globals, &qh);
    let workspace_state = WorkspaceState::new(&registry_state, &qh);
    let Some(toplevel_info_state) = ToplevelInfoState::try_new(&registry_state, &qh) else {
        tracing::error!("The compositor does not expose the toplevel info protocol");
        let _ = block_on(finished_tx.clone().send(WaylandUpdate::Finished));
        return;
    };
    let toplevel_manager_state = ToplevelManagerState::try_new(&registry_state, &qh);

    let mut app_data = AppData {
        tx,
        registry_state,
        output_state,
        workspace_state,
        toplevel_info_state,
        toplevel_manager_state,
        seat_state: SeatState::new(&globals, &qh),
        configured_output: std::env::var("COSMIC_PANEL_OUTPUT")
            .ok()
            .filter(|value| !value.is_empty()),
        expected_output: None,
        workspaces_ready: false,
        last_windows: Vec::new(),
    };

    loop {
        if let Err(err) = event_loop.dispatch(None, &mut app_data) {
            tracing::error!("Wayland event loop exited: {err}");
            break;
        }
    }

    let _ = block_on(finished_tx.clone().send(WaylandUpdate::Finished));
}

sctk::delegate_seat!(AppData);
sctk::delegate_output!(AppData);
sctk::delegate_registry!(AppData);
cctk::delegate_workspace!(AppData);
cctk::delegate_toplevel_info!(AppData);
cctk::delegate_toplevel_manager!(AppData);
