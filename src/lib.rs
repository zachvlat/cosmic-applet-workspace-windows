// SPDX-License-Identifier: GPL-3.0-only

mod config;
mod wayland;

use std::{
    cmp::{Ordering, Reverse},
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

use config::{AppletConfig, MAX_TITLE_CHARS, MIN_TITLE_CHARS};
use cosmic::{
    Apply, Element, app,
    cctk::{
        wayland_client::Proxy,
        wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    },
    desktop::{
        DesktopEntryCache, DesktopLookupContext, DesktopResolveOptions, IconSourceExt, fde,
        resolve_desktop_entry, spawn_desktop_exec,
    },
    iced::{
        self, Alignment, Length, Subscription, event, mouse,
        widget::{row, space},
        window,
    },
    surface::action::{app_popup, destroy_popup},
    theme,
    widget::{self, autosize, container, menu},
};

use wayland::{
    WaylandUpdate, WindowGeometry, WorkspaceWindow, close_window, focus_window, minimize_window,
    set_window_maximized, workspace_windows_subscription,
};

const APP_ID: &str = "io.github.tkilian.CosmicAppletAppTitle";
const EMPTY_TITLE: &str = "Desktop";
const CONTEXT_MENU_WIDTH: f32 = 320.0;
const DRAG_THRESHOLD: f32 = 8.0;
const EDGE_ALIGNMENT_TOLERANCE: i32 = 16;
const FLOATING_OVERLAP_RATIO: f32 = 0.18;
const FLOATING_SMALL_AREA_RATIO: f32 = 0.72;
const SETTINGS_POPUP_WIDTH: f32 = 360.0;

static AUTOSIZE_MAIN_ID: LazyLock<widget::Id> = LazyLock::new(|| widget::Id::new("autosize-main"));

type WindowId = u32;

pub fn run() -> cosmic::iced::Result {
    cosmic::applet::run::<Applet>(())
}

#[derive(Debug, Clone)]
struct WindowMenuAction {
    app_id: Option<String>,
    exec: String,
    name: String,
    terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StackAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone)]
struct DisplayWindow {
    app_name: String,
    geometry: Option<WindowGeometry>,
    menu_actions: Vec<WindowMenuAction>,
    handle: ExtForeignToplevelHandleV1,
    title: String,
    icon: Option<widget::icon::Handle>,
    is_active: bool,
    is_maximized: bool,
    is_sticky: bool,
}

#[derive(Debug, Clone)]
enum WindowControlAction {
    Close(ExtForeignToplevelHandleV1),
    Minimize(ExtForeignToplevelHandleV1),
    SetMaximized(ExtForeignToplevelHandleV1, bool),
}

#[derive(Debug, Clone)]
enum DeferredMenuAction {
    LaunchDesktopAction(WindowMenuAction),
    OpenSettings,
    WindowControl(WindowControlAction),
}

struct PressedWindow {
    handle: ExtForeignToplevelHandleV1,
    origin: Option<iced::Point>,
}

struct Applet {
    config: AppletConfig,
    config_dirty: bool,
    context_menu_popup: Option<window::Id>,
    context_menu_window: Option<ExtForeignToplevelHandleV1>,
    core: cosmic::app::Core,
    cursor_in_applet: Option<iced::Point>,
    desktop_cache: DesktopEntryCache,
    dragging_window: Option<WindowId>,
    hovered_window: Option<ExtForeignToplevelHandleV1>,
    last_drag_target: Option<WindowId>,
    ordered_window_ids: Vec<WindowId>,
    pending_context_menu_window: Option<ExtForeignToplevelHandleV1>,
    pending_menu_action: Option<DeferredMenuAction>,
    pressed_window: Option<PressedWindow>,
    settings_popup: Option<window::Id>,
    source_windows: Vec<WorkspaceWindow>,
    windows: Vec<DisplayWindow>,
    workspace_tiling_enabled: bool,
}

#[derive(Debug, Clone)]
enum Message {
    ClearHoveredWindow(ExtForeignToplevelHandleV1),
    ClearHoveredWindowGlobal,
    CloseWindow(ExtForeignToplevelHandleV1),
    DesktopActionFinished,
    HoverWindow(ExtForeignToplevelHandleV1),
    MinimizeWindow(ExtForeignToplevelHandleV1),
    OpenWindowContextMenu(ExtForeignToplevelHandleV1),
    OpenSettingsPopup,
    PopupClosed(window::Id),
    PressWindow(ExtForeignToplevelHandleV1),
    ReleasePointer,
    RunWindowAction(WindowMenuAction),
    SetMaxTitleChars(usize),
    SetMiddleClickCloses(bool),
    SetShowAppIcons(bool),
    SetWindowMaximized(ExtForeignToplevelHandleV1, bool),
    UpdateAppletCursor(iced::Point),
    Wayland(WaylandUpdate),
}

impl Applet {
    fn window_id(handle: &ExtForeignToplevelHandleV1) -> WindowId {
        handle.id().protocol_id()
    }

    fn persist_config_if_dirty(&mut self) {
        if self.config_dirty {
            self.config.save();
            self.config_dirty = false;
        }
    }

    fn max_chars(&self) -> usize {
        self.config.max_title_chars
    }

    fn resolve_desktop_entry(&mut self, window: &WorkspaceWindow) -> Option<fde::DesktopEntry> {
        let app_id = window.app_id.as_deref().or(window.identifier.as_deref())?;

        let mut lookup = DesktopLookupContext::new(app_id).with_title(window.title.as_str());
        if let Some(identifier) = window.identifier.as_deref() {
            lookup = lookup.with_identifier(identifier);
        }

        let entry = resolve_desktop_entry(
            &mut self.desktop_cache,
            &lookup,
            &DesktopResolveOptions::default(),
        );

        Some(entry)
    }

    fn resolve_window(&mut self, window: &WorkspaceWindow) -> DisplayWindow {
        let mut app_name = window
            .app_id
            .clone()
            .or(window.identifier.clone())
            .unwrap_or_else(|| window.title.clone());
        let mut menu_actions = Vec::new();
        let mut icon = None;

        if let Some(entry) = self.resolve_desktop_entry(window) {
            let locales = self.desktop_cache.locales();
            let app_id = Some(entry.appid.to_string());
            let terminal = entry.terminal();
            let has_new_window_action = entry
                .actions()
                .unwrap_or_default()
                .into_iter()
                .any(is_new_window_action_id);

            app_name = entry
                .name(locales)
                .unwrap_or_else(|| std::borrow::Cow::Borrowed(&entry.appid))
                .to_string();
            if self.config.show_app_icons {
                let icon_source =
                    fde::IconSource::from_unknown(entry.icon().unwrap_or(&entry.appid));
                icon = Some(icon_source.as_cosmic_icon());
            }

            if !has_new_window_action {
                if let Some(exec) = entry.exec() {
                    menu_actions.push(WindowMenuAction {
                        app_id: app_id.clone(),
                        exec: exec.to_string(),
                        name: String::from("New Window"),
                        terminal,
                    });
                }
            }

            menu_actions.extend(entry.actions().unwrap_or_default().into_iter().filter_map(
                |action_id| {
                    let name = entry.action_entry_localized(action_id, "Name", locales)?;
                    let exec = entry.action_entry(action_id, "Exec")?;

                    Some(WindowMenuAction {
                        app_id: app_id.clone(),
                        exec: exec.to_string(),
                        name: name.to_string(),
                        terminal,
                    })
                },
            ));
        }

        DisplayWindow {
            app_name,
            geometry: window.geometry,
            menu_actions,
            handle: window.handle.clone(),
            title: window.title.clone(),
            icon,
            is_active: window.is_active,
            is_maximized: window.is_maximized,
            is_sticky: window.is_sticky,
        }
    }

    fn selected_context_window(&self) -> Option<&DisplayWindow> {
        let handle = self.context_menu_window.as_ref()?;
        self.windows.iter().find(|window| &window.handle == handle)
    }

    fn context_menu_label(label: impl ToString) -> Element<'static, Message> {
        widget::text(label.to_string())
            .wrapping(iced::widget::text::Wrapping::None)
            .width(Length::Shrink)
            .into()
    }

    fn perform_window_control(action: WindowControlAction) {
        match action {
            WindowControlAction::Close(handle) => close_window(handle),
            WindowControlAction::Minimize(handle) => minimize_window(handle),
            WindowControlAction::SetMaximized(handle, maximized) => {
                set_window_maximized(handle, maximized);
            }
        }
    }

    fn run_deferred_menu_action(&mut self, action: DeferredMenuAction) -> app::Task<Message> {
        match action {
            DeferredMenuAction::LaunchDesktopAction(action) => {
                Self::launch_window_action_task(action)
            }
            DeferredMenuAction::OpenSettings => self.open_settings_task(),
            DeferredMenuAction::WindowControl(action) => {
                Self::perform_window_control(action);
                app::Task::none()
            }
        }
    }

    fn queue_or_run_menu_action(&mut self, action: DeferredMenuAction) -> app::Task<Message> {
        if let Some(menu_id) = self.context_menu_popup {
            self.pending_context_menu_window = None;
            self.pending_menu_action = Some(action);
            surface_task(destroy_popup(menu_id))
        } else {
            self.run_deferred_menu_action(action)
        }
    }

    fn context_menu_window_control(
        icon_name: &'static str,
        message: Message,
        is_active: bool,
        padding: impl Into<iced::Padding>,
    ) -> Element<'static, Message> {
        widget::icon::from_name(icon_name)
            .apply(widget::button::icon)
            .padding(padding)
            .class(theme::Button::HeaderBar)
            .selected(is_active)
            .icon_size(16)
            .on_press(message)
            .into()
    }

    fn context_menu_window_controls(window: &DisplayWindow) -> Element<'static, Message> {
        let minimize_handle = window.handle.clone();
        let maximize_handle = window.handle.clone();
        let close_handle = window.handle.clone();

        row![
            Self::context_menu_window_control(
                "window-minimize-symbolic",
                Message::MinimizeWindow(minimize_handle),
                window.is_active,
                8,
            ),
            Self::context_menu_window_control(
                if window.is_maximized {
                    "window-restore-symbolic"
                } else {
                    "window-maximize-symbolic"
                },
                Message::SetWindowMaximized(maximize_handle, !window.is_maximized),
                window.is_active,
                8,
            ),
            Self::context_menu_window_control(
                "window-close-symbolic",
                Message::CloseWindow(close_handle),
                window.is_active,
                [8, 4, 8, 8],
            ),
        ]
        .spacing(4)
        .into()
    }

    fn apply_window_order(&mut self) {
        let order = self
            .layout_window_order()
            .unwrap_or_else(|| self.ordered_window_ids.clone());
        let positions = order
            .into_iter()
            .enumerate()
            .map(|(index, id)| (id, index))
            .collect::<HashMap<_, _>>();

        self.windows.sort_by_key(|window| {
            positions
                .get(&Self::window_id(&window.handle))
                .copied()
                .unwrap_or(usize::MAX)
        });
    }

    fn manual_window_order_index(&self, handle: &ExtForeignToplevelHandleV1) -> usize {
        let window_id = Self::window_id(handle);
        self.ordered_window_ids
            .iter()
            .position(|id| *id == window_id)
            .unwrap_or(usize::MAX)
    }

    fn window_area(geometry: WindowGeometry) -> i64 {
        i64::from(geometry.width.max(0)) * i64::from(geometry.height.max(0))
    }

    fn window_right(geometry: WindowGeometry) -> i32 {
        geometry.x.saturating_add(geometry.width)
    }

    fn window_bottom(geometry: WindowGeometry) -> i32 {
        geometry.y.saturating_add(geometry.height)
    }

    fn overlap_area(left: WindowGeometry, right: WindowGeometry) -> i64 {
        let x_overlap =
            (Self::window_right(left).min(Self::window_right(right)) - left.x.max(right.x)).max(0);
        let y_overlap = (Self::window_bottom(left).min(Self::window_bottom(right))
            - left.y.max(right.y))
        .max(0);

        i64::from(x_overlap) * i64::from(y_overlap)
    }

    fn windows_align(left: WindowGeometry, right: WindowGeometry) -> bool {
        let tolerance = EDGE_ALIGNMENT_TOLERANCE as u32;

        left.x.abs_diff(right.x) <= tolerance
            || left.y.abs_diff(right.y) <= tolerance
            || Self::window_right(left).abs_diff(Self::window_right(right)) <= tolerance
            || Self::window_bottom(left).abs_diff(Self::window_bottom(right)) <= tolerance
    }

    fn floating_window_ids(&self, windows: &[&DisplayWindow]) -> HashSet<WindowId> {
        let largest_area = windows
            .iter()
            .filter_map(|window| window.geometry)
            .map(Self::window_area)
            .max()
            .unwrap_or(0);

        windows
            .iter()
            .filter_map(|window| {
                let window_id = Self::window_id(&window.handle);
                let geometry = match window.geometry {
                    Some(geometry) => geometry,
                    None => return Some(window_id),
                };

                if window.is_sticky {
                    return Some(window_id);
                }

                let area = Self::window_area(geometry);
                let has_large_overlap = windows.iter().any(|other| {
                    if window.handle == other.handle {
                        return false;
                    }

                    let Some(other_geometry) = other.geometry else {
                        return false;
                    };
                    let other_area = Self::window_area(other_geometry);
                    if other_area < area {
                        return false;
                    }

                    let overlap = Self::overlap_area(geometry, other_geometry);
                    overlap > 0 && (overlap as f32 / area.max(1) as f32) >= FLOATING_OVERLAP_RATIO
                });

                let shares_edge = windows.iter().any(|other| {
                    if window.handle == other.handle {
                        return false;
                    }

                    other
                        .geometry
                        .is_some_and(|other_geometry| Self::windows_align(geometry, other_geometry))
                });

                let is_small = largest_area > 0
                    && (area as f32) <= (largest_area as f32 * FLOATING_SMALL_AREA_RATIO);

                (has_large_overlap || (is_small && !shares_edge)).then_some(window_id)
            })
            .collect()
    }

    fn tiled_master_id(windows: &[&DisplayWindow]) -> Option<WindowId> {
        windows
            .iter()
            .filter_map(|window| {
                window.geometry.map(|geometry| {
                    (
                        window,
                        (
                            Reverse(Self::window_area(geometry)),
                            geometry.x,
                            geometry.y,
                            Reverse(Self::window_id(&window.handle)),
                        ),
                    )
                })
            })
            .min_by_key(|(_, key)| *key)
            .map(|(window, _)| Self::window_id(&window.handle))
    }

    fn tiled_stack_axis(windows: &[&DisplayWindow], master_id: Option<WindowId>) -> StackAxis {
        let geometries = windows
            .iter()
            .filter(|window| Some(Self::window_id(&window.handle)) != master_id)
            .filter_map(|window| window.geometry)
            .collect::<Vec<_>>();

        let Some(first) = geometries.first().copied() else {
            return StackAxis::Vertical;
        };

        let (mut min_x, mut max_x) = (first.x, first.x);
        let (mut min_y, mut max_y) = (first.y, first.y);
        for geometry in geometries.iter().copied().skip(1) {
            min_x = min_x.min(geometry.x);
            max_x = max_x.max(geometry.x);
            min_y = min_y.min(geometry.y);
            max_y = max_y.max(geometry.y);
        }

        if max_y.abs_diff(min_y) >= max_x.abs_diff(min_x) {
            StackAxis::Vertical
        } else {
            StackAxis::Horizontal
        }
    }

    fn compare_floating_windows(&self, left: &DisplayWindow, right: &DisplayWindow) -> Ordering {
        self.manual_window_order_index(&left.handle)
            .cmp(&self.manual_window_order_index(&right.handle))
            .then_with(|| match (left.geometry, right.geometry) {
                (Some(left_geometry), Some(right_geometry)) => left_geometry
                    .y
                    .cmp(&right_geometry.y)
                    .then_with(|| left_geometry.x.cmp(&right_geometry.x)),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            })
            .then_with(|| left.title.cmp(&right.title))
    }

    fn compare_tiled_windows(
        &self,
        left: &DisplayWindow,
        right: &DisplayWindow,
        master_id: Option<WindowId>,
        stack_axis: StackAxis,
    ) -> Ordering {
        let left_id = Self::window_id(&left.handle);
        let right_id = Self::window_id(&right.handle);

        match (Some(left_id) == master_id, Some(right_id) == master_id) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }

        match (left.geometry, right.geometry) {
            (Some(left_geometry), Some(right_geometry)) => {
                let geometry_order = match stack_axis {
                    StackAxis::Horizontal => left_geometry
                        .x
                        .cmp(&right_geometry.x)
                        .then_with(|| left_geometry.y.cmp(&right_geometry.y)),
                    StackAxis::Vertical => left_geometry
                        .y
                        .cmp(&right_geometry.y)
                        .then_with(|| left_geometry.x.cmp(&right_geometry.x)),
                };

                if geometry_order != Ordering::Equal {
                    return geometry_order;
                }
            }
            (Some(_), None) => return Ordering::Less,
            (None, Some(_)) => return Ordering::Greater,
            (None, None) => {}
        }

        self.manual_window_order_index(&left.handle)
            .cmp(&self.manual_window_order_index(&right.handle))
            .then_with(|| left.title.cmp(&right.title))
    }

    fn layout_window_order(&self) -> Option<Vec<WindowId>> {
        if !self.workspace_tiling_enabled {
            return None;
        }

        let windows = self.windows.iter().collect::<Vec<_>>();
        if windows.is_empty() {
            return Some(Vec::new());
        }

        let floating_ids = self.floating_window_ids(&windows);
        let mut tiled_windows = windows
            .iter()
            .copied()
            .filter(|window| !floating_ids.contains(&Self::window_id(&window.handle)))
            .collect::<Vec<_>>();
        let mut floating_windows = windows
            .iter()
            .copied()
            .filter(|window| floating_ids.contains(&Self::window_id(&window.handle)))
            .collect::<Vec<_>>();

        let master_id = Self::tiled_master_id(&tiled_windows);
        let stack_axis = Self::tiled_stack_axis(&tiled_windows, master_id);

        tiled_windows
            .sort_by(|left, right| self.compare_tiled_windows(left, right, master_id, stack_axis));
        floating_windows.sort_by(|left, right| self.compare_floating_windows(left, right));

        Some(
            tiled_windows
                .into_iter()
                .chain(floating_windows)
                .map(|window| Self::window_id(&window.handle))
                .collect(),
        )
    }

    fn clear_pointer_state(&mut self) {
        self.dragging_window = None;
        self.last_drag_target = None;
        self.pressed_window = None;
    }

    fn reorder_window(&mut self, dragged_id: WindowId, target_id: WindowId) -> bool {
        if dragged_id == target_id {
            return false;
        }

        let Some(from) = self
            .ordered_window_ids
            .iter()
            .position(|id| *id == dragged_id)
        else {
            return false;
        };
        let Some(target_position) = self
            .ordered_window_ids
            .iter()
            .position(|id| *id == target_id)
        else {
            return false;
        };

        let dragged = self.ordered_window_ids.remove(from);
        let Some(target_after_removal) = self
            .ordered_window_ids
            .iter()
            .position(|id| *id == target_id)
        else {
            self.ordered_window_ids.insert(from, dragged);
            return false;
        };

        let insert_at = if from < target_position {
            target_after_removal + 1
        } else {
            target_after_removal
        };

        self.ordered_window_ids
            .insert(insert_at.min(self.ordered_window_ids.len()), dragged);
        self.apply_window_order();
        true
    }

    fn sync_window_order(&mut self) {
        let live_ids = self
            .windows
            .iter()
            .map(|window| Self::window_id(&window.handle))
            .collect::<Vec<_>>();

        self.ordered_window_ids
            .retain(|id| live_ids.iter().any(|live_id| live_id == id));

        for live_id in &live_ids {
            if !self.ordered_window_ids.contains(live_id) {
                self.ordered_window_ids.push(*live_id);
            }
        }

        self.apply_window_order();

        if self
            .dragging_window
            .is_some_and(|id| !live_ids.iter().any(|live_id| *live_id == id))
        {
            self.dragging_window = None;
            self.last_drag_target = None;
        }

        if self
            .last_drag_target
            .is_some_and(|id| !live_ids.iter().any(|live_id| *live_id == id))
        {
            self.last_drag_target = None;
        }

        if self.pressed_window.as_ref().is_some_and(|pressed| {
            !live_ids
                .iter()
                .any(|live_id| *live_id == Self::window_id(&pressed.handle))
        }) {
            self.pressed_window = None;
        }
    }

    fn rebuild_windows(&mut self) {
        let source_windows = self.source_windows.clone();
        self.windows = source_windows
            .iter()
            .map(|window| self.resolve_window(window))
            .collect();
        self.sync_window_order();

        if self
            .hovered_window
            .as_ref()
            .is_some_and(|hovered| !self.windows.iter().any(|window| &window.handle == hovered))
        {
            self.hovered_window = None;
        }

        if self
            .context_menu_window
            .as_ref()
            .is_some_and(|target| !self.windows.iter().any(|window| &window.handle == target))
        {
            self.context_menu_window = None;
        }

        if self
            .pending_context_menu_window
            .as_ref()
            .is_some_and(|target| !self.windows.iter().any(|window| &window.handle == target))
        {
            self.pending_context_menu_window = None;
        }

        if self.pressed_window.as_ref().is_some_and(|pressed| {
            !self
                .windows
                .iter()
                .any(|window| window.handle == pressed.handle)
        }) {
            self.pressed_window = None;
        }
    }

    fn settings_panel(&self) -> Element<'_, Message> {
        let content = widget::container(
            widget::settings::view_column(vec![
                widget::text::title4("Workspace Windows").into(),
                widget::text::caption("Changes apply immediately and are saved automatically.")
                    .into(),
                widget::settings::section()
                    .title("Display")
                    .add(
                        widget::settings::item::builder("Show application icons")
                            .description("Display the desktop icon before each window title.")
                            .toggler(self.config.show_app_icons, Message::SetShowAppIcons),
                    )
                    .add(
                        widget::settings::item::builder("Maximum title length")
                            .description("Limit how many characters each window title can use.")
                            .control(widget::spin_button(
                                self.config.max_title_chars.to_string(),
                                self.config.max_title_chars,
                                1,
                                MIN_TITLE_CHARS,
                                MAX_TITLE_CHARS,
                                Message::SetMaxTitleChars,
                            )),
                    )
                    .into(),
                widget::settings::section()
                    .title("Actions")
                    .add(
                        widget::settings::item::builder("Middle-click closes windows")
                            .description("Close a window directly by middle-clicking its tile.")
                            .toggler(
                                self.config.middle_click_closes,
                                Message::SetMiddleClickCloses,
                            ),
                    )
                    .into(),
            ])
            .width(Length::Fill),
        )
        .padding(16)
        .width(Length::Fixed(SETTINGS_POPUP_WIDTH));

        self.core.applet.popup_container(content).into()
    }

    fn context_menu_panel(&self) -> Element<'_, Message> {
        let context_window = self.selected_context_window();
        let mut items: Vec<Element<'_, Message>> = Vec::new();

        let mut title_row = row![]
            .align_y(Alignment::Center)
            .spacing(8)
            .width(Length::Fill);
        if let Some(icon) = context_window.and_then(|window| window.icon.clone()) {
            title_row = title_row.push(
                container(
                    widget::icon(icon)
                        .width(Length::Fixed(16.0))
                        .height(Length::Fixed(16.0)),
                )
                .padding([0, 4]),
            );
        }

        title_row = title_row
            .push(Self::context_menu_label(
                context_window
                    .map(|window| window.app_name.as_str())
                    .unwrap_or("Workspace Windows"),
            ))
            .push(space::horizontal().width(Length::Fill));

        if let Some(window) = context_window {
            title_row = title_row.push(Self::context_menu_window_controls(window));
        }

        items.push(
            container(title_row)
                .padding(
                    iced::Padding::ZERO
                        .top(2.0)
                        .bottom(2.0)
                        .left(8.0)
                        .right(4.0),
                )
                .width(Length::Fill)
                .into(),
        );
        items.push(widget::divider::horizontal::light().into());

        if let Some(window) = context_window {
            if window.menu_actions.is_empty() {
                items.push(
                    menu::menu_button(vec![
                        Self::context_menu_label("No application actions"),
                        space::horizontal().width(Length::Fill).into(),
                    ])
                    .into(),
                );
            } else {
                for action in &window.menu_actions {
                    items.push(
                        menu::menu_button(vec![
                            Self::context_menu_label(action.name.clone()),
                            space::horizontal().width(Length::Fill).into(),
                        ])
                        .on_press(Message::RunWindowAction(action.clone()))
                        .into(),
                    );
                }
            }
        } else {
            items.push(
                menu::menu_button(vec![
                    Self::context_menu_label("Window unavailable"),
                    space::horizontal().width(Length::Fill).into(),
                ])
                .into(),
            );
        }

        items.push(widget::divider::horizontal::light().into());
        let settings = menu::menu_button(vec![
            Self::context_menu_label("Workspace Windows Settings"),
            space::horizontal().width(Length::Fill).into(),
        ])
        .on_press(Message::OpenSettingsPopup);
        items.push(settings.into());

        let content = container(
            widget::column::with_children(items)
                .width(Length::Fill)
                .spacing(2),
        )
        .padding([8, 4])
        .width(Length::Fixed(CONTEXT_MENU_WIDTH));

        self.core.applet.popup_container(content).into()
    }

    fn open_context_menu_task(&self) -> app::Task<Message> {
        surface_task(app_popup::<Applet>(
            |state: &mut Applet| {
                let new_id = window::Id::unique();
                state.context_menu_popup = Some(new_id);

                let mut popup_settings = state.core.applet.get_popup_settings(
                    state
                        .core
                        .main_window_id()
                        .expect("applet main window missing"),
                    new_id,
                    None,
                    None,
                    None,
                );

                if let Some(position) = state.cursor_in_applet {
                    popup_settings.positioner.anchor_rect = iced::Rectangle {
                        x: position.x.round() as i32,
                        y: position.y.round() as i32,
                        width: 1,
                        height: 1,
                    };
                }

                popup_settings
            },
            Some(Box::new(|state: &Applet| {
                state.context_menu_panel().map(cosmic::Action::App)
            })),
        ))
    }

    fn launch_window_action_task(action: WindowMenuAction) -> app::Task<Message> {
        cosmic::task::future(async move {
            spawn_desktop_exec(
                &action.exec,
                Vec::<(String, String)>::new(),
                action.app_id.as_deref(),
                action.terminal,
            )
            .await;

            Message::DesktopActionFinished
        })
    }

    fn open_settings_task(&self) -> app::Task<Message> {
        surface_task(app_popup::<Applet>(
            |state: &mut Applet| {
                let new_id = window::Id::unique();
                state.settings_popup = Some(new_id);

                let mut popup_settings = state.core.applet.get_popup_settings(
                    state
                        .core
                        .main_window_id()
                        .expect("applet main window missing"),
                    new_id,
                    None,
                    None,
                    None,
                );

                if let Some(position) = state.cursor_in_applet {
                    popup_settings.positioner.anchor_rect = iced::Rectangle {
                        x: position.x.round() as i32,
                        y: position.y.round() as i32,
                        width: 1,
                        height: 1,
                    };
                }

                popup_settings
            },
            Some(Box::new(|state: &Applet| {
                state.settings_panel().map(cosmic::Action::App)
            })),
        ))
    }

    fn window_tile(&self, window: &DisplayWindow, icon_size: f32) -> Element<'_, Message> {
        let text = truncate_title(&window.title, self.max_chars());
        let mut content = row![].align_y(Alignment::Center).spacing(4);

        if let Some(icon) = window.icon.clone() {
            content = content.push(
                widget::icon(icon)
                    .width(Length::Fixed(icon_size))
                    .height(Length::Fixed(icon_size)),
            );
        }

        content = content.push(self.core.applet.text(text));

        let is_active = window.is_active;
        let is_dragging = self
            .dragging_window
            .is_some_and(|dragging| dragging == Self::window_id(&window.handle));
        let is_hovered = self
            .hovered_window
            .as_ref()
            .is_some_and(|hovered| hovered == &window.handle);
        let handle = window.handle.clone();
        let press_handle = handle.clone();
        let hover_handle = handle.clone();
        let hover_move_handle = handle.clone();
        let context_handle = handle.clone();
        let preview = container(content)
            .padding([2, 8])
            .class(theme::Container::custom(move |theme| {
                let cosmic = theme.cosmic();
                let highlight = is_hovered || is_dragging;
                let (background, foreground, border_color, border_width) = if is_active {
                    (
                        if highlight {
                            cosmic.accent_button.hover.into()
                        } else {
                            cosmic.accent_button.base.into()
                        },
                        cosmic.accent_button.on.into(),
                        if is_dragging {
                            cosmic.accent.base.into()
                        } else if is_hovered {
                            cosmic.accent.base.into()
                        } else {
                            iced::Color::TRANSPARENT
                        },
                        if highlight { 1.0 } else { 0.0 },
                    )
                } else {
                    (
                        if highlight {
                            cosmic.background.component.hover.into()
                        } else {
                            cosmic.background.component.base.into()
                        },
                        cosmic.background.component.on.into(),
                        if is_dragging {
                            cosmic.accent.base.into()
                        } else if is_hovered {
                            cosmic.bg_divider().into()
                        } else {
                            iced::Color::TRANSPARENT
                        },
                        if highlight { 1.0 } else { 0.0 },
                    )
                };

                container::Style {
                    icon_color: Some(foreground),
                    text_color: Some(foreground),
                    background: Some(iced::Background::Color(background)),
                    border: iced::Border {
                        radius: cosmic.corner_radii.radius_s.into(),
                        color: border_color,
                        width: border_width,
                        ..Default::default()
                    },
                    shadow: Default::default(),
                    snap: true,
                }
            }));

        let tile = widget::mouse_area(preview)
            .interaction(mouse::Interaction::Idle)
            .on_enter(Message::HoverWindow(hover_handle))
            .on_move(move |_| Message::HoverWindow(hover_move_handle.clone()))
            .on_exit(Message::ClearHoveredWindow(handle.clone()))
            .on_press(Message::PressWindow(press_handle))
            .on_right_press(Message::OpenWindowContextMenu(context_handle));

        let tile = if self.config.middle_click_closes {
            tile.on_middle_press(Message::CloseWindow(handle.clone()))
        } else {
            tile
        };

        tile.into()
    }

    fn empty_tile(&self) -> Element<'_, Message> {
        container(self.core.applet.text(EMPTY_TITLE))
            .padding([2, 8])
            .class(theme::Container::custom(move |theme| {
                let cosmic = theme.cosmic();
                let background = cosmic.background.component.base.into();
                let foreground = cosmic.background.component.on.into();

                container::Style {
                    icon_color: Some(foreground),
                    text_color: Some(foreground),
                    background: Some(iced::Background::Color(background)),
                    border: iced::Border {
                        radius: cosmic.corner_radii.radius_s.into(),
                        ..Default::default()
                    },
                    shadow: Default::default(),
                    snap: true,
                }
            }))
            .into()
    }
}

impl cosmic::Application for Applet {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();

    const APP_ID: &'static str = APP_ID;

    fn init(core: cosmic::app::Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        let config = AppletConfig::load();
        (
            Self {
                config,
                config_dirty: false,
                context_menu_popup: None,
                context_menu_window: None,
                core,
                cursor_in_applet: None,
                desktop_cache: DesktopEntryCache::new(fde::get_languages_from_env()),
                dragging_window: None,
                hovered_window: None,
                last_drag_target: None,
                ordered_window_ids: Vec::new(),
                pending_context_menu_window: None,
                pending_menu_action: None,
                pressed_window: None,
                settings_popup: None,
                source_windows: Vec::new(),
                windows: Vec::new(),
                workspace_tiling_enabled: false,
            },
            app::Task::none(),
        )
    }

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn style(&self) -> Option<iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::ClearHoveredWindow(handle) => {
                if self
                    .hovered_window
                    .as_ref()
                    .is_some_and(|hovered| hovered == &handle)
                {
                    self.hovered_window = None;
                }
            }
            Message::ClearHoveredWindowGlobal => {
                self.hovered_window = None;
                self.cursor_in_applet = None;
                self.clear_pointer_state();
            }
            Message::CloseWindow(handle) => {
                return self.queue_or_run_menu_action(DeferredMenuAction::WindowControl(
                    WindowControlAction::Close(handle),
                ));
            }
            Message::DesktopActionFinished => {}
            Message::HoverWindow(handle) => {
                if !self.workspace_tiling_enabled {
                    if let Some(dragged_id) = self.dragging_window {
                        let target_id = Self::window_id(&handle);
                        if dragged_id != target_id && self.last_drag_target != Some(target_id) {
                            let _ = self.reorder_window(dragged_id, target_id);
                            self.last_drag_target = Some(target_id);
                        }
                    }
                }

                self.hovered_window = Some(handle);
            }
            Message::MinimizeWindow(handle) => {
                return self.queue_or_run_menu_action(DeferredMenuAction::WindowControl(
                    WindowControlAction::Minimize(handle),
                ));
            }
            Message::OpenWindowContextMenu(handle) => {
                if self.settings_popup.is_some() {
                    return app::Task::none();
                }

                self.clear_pointer_state();

                if let Some(id) = self.context_menu_popup {
                    self.pending_context_menu_window = Some(handle);
                    self.pending_menu_action = None;
                    return surface_task(destroy_popup(id));
                }

                self.context_menu_window = Some(handle);
                return self.open_context_menu_task();
            }
            Message::OpenSettingsPopup => {
                if self.settings_popup.is_some() {
                    return app::Task::none();
                }

                return self.queue_or_run_menu_action(DeferredMenuAction::OpenSettings);
            }
            Message::RunWindowAction(action) => {
                return self
                    .queue_or_run_menu_action(DeferredMenuAction::LaunchDesktopAction(action));
            }
            Message::PressWindow(handle) => {
                self.hovered_window = Some(handle.clone());
                self.pressed_window = Some(PressedWindow {
                    handle,
                    origin: self.cursor_in_applet,
                });
                self.dragging_window = None;
                self.last_drag_target = None;
            }
            Message::PopupClosed(id) => {
                if self.context_menu_popup == Some(id) {
                    self.context_menu_popup = None;
                    self.context_menu_window = None;

                    if let Some(action) = self.pending_menu_action.take() {
                        return self.run_deferred_menu_action(action);
                    }

                    if let Some(handle) = self.pending_context_menu_window.take() {
                        self.context_menu_window = Some(handle);
                        return self.open_context_menu_task();
                    }
                }
                if self.settings_popup == Some(id) {
                    self.settings_popup = None;
                    self.persist_config_if_dirty();
                }
            }
            Message::ReleasePointer => {
                if self.dragging_window.is_none() {
                    if let Some(pressed) = self.pressed_window.take() {
                        let pressed_id = Self::window_id(&pressed.handle);
                        let released_over_same_window = self
                            .hovered_window
                            .as_ref()
                            .is_some_and(|hovered| Self::window_id(hovered) == pressed_id);

                        if released_over_same_window {
                            focus_window(pressed.handle);
                        }
                    }
                }

                self.dragging_window = None;
                self.last_drag_target = None;
                self.pressed_window = None;
            }
            Message::SetMaxTitleChars(value) => {
                let value = value.clamp(MIN_TITLE_CHARS, MAX_TITLE_CHARS);
                if self.config.max_title_chars != value {
                    self.config.max_title_chars = value;
                    self.config_dirty = true;
                }
            }
            Message::SetMiddleClickCloses(value) => {
                if self.config.middle_click_closes != value {
                    self.config.middle_click_closes = value;
                    self.config_dirty = true;
                }
            }
            Message::SetShowAppIcons(value) => {
                if self.config.show_app_icons != value {
                    self.config.show_app_icons = value;
                    self.config_dirty = true;
                    self.rebuild_windows();
                }
            }
            Message::SetWindowMaximized(handle, maximized) => {
                return self.queue_or_run_menu_action(DeferredMenuAction::WindowControl(
                    WindowControlAction::SetMaximized(handle, maximized),
                ));
            }
            Message::UpdateAppletCursor(position) => {
                self.cursor_in_applet = Some(position);

                if !self.workspace_tiling_enabled {
                    if let Some(pressed) = self.pressed_window.as_mut() {
                        let origin = pressed.origin.get_or_insert(position);

                        if self.dragging_window.is_none()
                            && position.distance(*origin) >= DRAG_THRESHOLD
                        {
                            let dragged_id = Self::window_id(&pressed.handle);
                            self.dragging_window = Some(dragged_id);
                            self.last_drag_target = Some(dragged_id);
                        }
                    }
                }
            }
            Message::Wayland(update) => match update {
                WaylandUpdate::WorkspaceWindows {
                    windows,
                    tiling_enabled,
                } => {
                    self.workspace_tiling_enabled = tiling_enabled;
                    self.source_windows = windows;
                    self.rebuild_windows();
                }
                WaylandUpdate::Finished => {
                    tracing::error!("Wayland subscription ended");
                }
            },
        }

        app::Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch([
            workspace_windows_subscription().map(Message::Wayland),
            event::listen_with(|event, _, _| match event {
                iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    Some(Message::ReleasePointer)
                }
                iced::Event::Mouse(mouse::Event::CursorLeft) => {
                    Some(Message::ClearHoveredWindowGlobal)
                }
                _ => None,
            }),
        ])
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let height = (self.core.applet.suggested_size(true).1
            + 2 * self.core.applet.suggested_padding(true).1) as f32;
        let icon_size = self.core.applet.suggested_size(true).0 as f32;
        let mut content = row![].align_y(Alignment::Center).spacing(6);

        if self.windows.is_empty() {
            content = content.push(self.empty_tile());
        } else {
            for window in &self.windows {
                content = content.push(self.window_tile(window, icon_size));
            }
        }

        content = content.push(space::vertical().height(Length::Fixed(height)));

        let content = container(content).padding([0, self.core.applet.suggested_padding(true).0]);
        widget::mouse_area(autosize::autosize(content, AUTOSIZE_MAIN_ID.clone()))
            .interaction(mouse::Interaction::Idle)
            .on_move(Message::UpdateAppletCursor)
            .into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Self::Message> {
        if self.settings_popup == Some(id) {
            self.settings_panel()
        } else if self.context_menu_popup == Some(id) {
            self.context_menu_panel()
        } else {
            widget::text::body("").into()
        }
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Self::Message> {
        Some(Message::PopupClosed(id))
    }
}

fn surface_task(action: cosmic::surface::Action) -> app::Task<Message> {
    cosmic::task::message(cosmic::Action::Cosmic(cosmic::app::Action::Surface(action)))
}

fn is_new_window_action_id(action_id: &str) -> bool {
    let normalized = action_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect::<String>();

    matches!(
        normalized.as_str(),
        "new" | "newwindow" | "newemptywindow" | "newmainwindow"
    )
}

fn truncate_title(title: &str, max_chars: usize) -> String {
    let char_count = title.chars().count();
    if char_count <= max_chars {
        return title.to_owned();
    }

    let keep = max_chars.saturating_sub(3);
    let mut truncated = title.chars().take(keep).collect::<String>();
    truncated.push_str("...");
    truncated
}
