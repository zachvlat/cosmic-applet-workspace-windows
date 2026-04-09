// SPDX-License-Identifier: GPL-3.0-only

use std::os::{
    fd::{FromRawFd, RawFd},
    unix::net::UnixStream,
};

use cosmic::{
    cctk::{
        self,
        cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1,
        sctk::{
            self,
            output::{OutputHandler, OutputState},
            reexports::{calloop, calloop_wayland_source::WaylandSource},
            registry::{ProvidesRegistryState, RegistryState},
        },
        toplevel_info::{ToplevelInfo, ToplevelInfoHandler, ToplevelInfoState},
        wayland_client::{
            Connection, QueueHandle, globals::registry_queue_init, protocol::wl_output::WlOutput,
        },
    },
    iced::Subscription,
};
use futures::{SinkExt, channel::mpsc, executor::block_on};
use iced_futures::stream;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWindow {
    pub title: String,
    pub app_id: Option<String>,
    pub identifier: Option<String>,
}

#[derive(Debug, Clone)]
pub enum WaylandUpdate {
    ActiveWindow(Option<ActiveWindow>),
    Finished,
}

pub fn active_window_subscription() -> Subscription<WaylandUpdate> {
    Subscription::run_with(std::any::TypeId::of::<WaylandUpdate>(), |_| {
        stream::channel(1, move |output: mpsc::Sender<WaylandUpdate>| async move {
            let _ = std::thread::Builder::new()
                .name("active-window-title-wayland".into())
                .spawn(move || {
                    wayland_event_loop(output.clone());
                });

            futures::future::pending::<()>().await;
        })
    })
}

struct AppData {
    tx: mpsc::Sender<WaylandUpdate>,
    registry_state: RegistryState,
    output_state: OutputState,
    toplevel_info_state: ToplevelInfoState,
    configured_output: Option<String>,
    expected_output: Option<WlOutput>,
    last_window: Option<ActiveWindow>,
}

impl AppData {
    fn trimmed(value: &str) -> Option<String> {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    }

    fn active_window_for(info: &ToplevelInfo) -> Option<ActiveWindow> {
        let app_id = Self::trimmed(&info.app_id);
        let title = Self::trimmed(&info.title).or_else(|| app_id.clone())?;
        let identifier = Self::trimmed(&info.identifier);

        Some(ActiveWindow {
            title,
            app_id,
            identifier,
        })
    }

    fn current_window(&self) -> Option<ActiveWindow> {
        let active: Vec<_> = self
            .toplevel_info_state
            .toplevels()
            .filter(|info| {
                info.state
                    .contains(&zcosmic_toplevel_handle_v1::State::Activated)
            })
            .collect();

        if let Some(output) = self.expected_output.as_ref() {
            if let Some(info) = active.iter().find(|info| info.output.contains(output)) {
                return Self::active_window_for(info);
            }
        }

        active.into_iter().find_map(Self::active_window_for)
    }

    fn publish_window(&mut self) {
        let window = self.current_window();
        if window == self.last_window {
            return;
        }

        self.last_window = window.clone();
        let _ = block_on(self.tx.send(WaylandUpdate::ActiveWindow(window)));
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
            self.publish_window();
        }
    }

    fn update_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: WlOutput) {
        if self.sync_expected_output(&output) {
            self.publish_window();
        }
    }

    fn output_destroyed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: WlOutput) {
        let was_expected = self
            .expected_output
            .as_ref()
            .is_some_and(|current| current == &output);
        if was_expected {
            self.expected_output = None;
            self.publish_window();
        }
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
        self.publish_window();
    }

    fn update_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _toplevel: &cctk::wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        self.publish_window();
    }

    fn toplevel_closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _toplevel: &cctk::wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        self.publish_window();
    }

    fn info_done(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>) {
        self.publish_window();
    }
}

fn wayland_event_loop(tx: mpsc::Sender<WaylandUpdate>) {
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

    let registry_state = RegistryState::new(&globals);
    let Some(toplevel_info_state) = ToplevelInfoState::try_new(&registry_state, &qh) else {
        tracing::error!("The compositor does not expose the toplevel info protocol");
        let _ = block_on(finished_tx.clone().send(WaylandUpdate::Finished));
        return;
    };

    let mut app_data = AppData {
        tx,
        output_state: OutputState::new(&globals, &qh),
        toplevel_info_state,
        registry_state,
        configured_output: std::env::var("COSMIC_PANEL_OUTPUT")
            .ok()
            .filter(|value| !value.is_empty()),
        expected_output: None,
        last_window: None,
    };

    loop {
        if let Err(err) = event_loop.dispatch(None, &mut app_data) {
            tracing::error!("Wayland event loop exited: {err}");
            break;
        }
    }

    let _ = block_on(finished_tx.clone().send(WaylandUpdate::Finished));
}

sctk::delegate_output!(AppData);
sctk::delegate_registry!(AppData);
cctk::delegate_toplevel_info!(AppData);
