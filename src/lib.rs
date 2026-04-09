// SPDX-License-Identifier: GPL-3.0-only

mod wayland;

use std::sync::LazyLock;

use cosmic::{
    Element, app,
    applet::cosmic_panel_config::PanelAnchor,
    desktop::{
        DesktopEntryCache, DesktopLookupContext, DesktopResolveOptions, IconSourceExt, fde,
        resolve_desktop_entry,
    },
    iced::{
        self, Alignment, Length, Subscription,
        widget::{row, space},
    },
    widget::{self, autosize, container},
};

use wayland::{ActiveWindow, WaylandUpdate, active_window_subscription};

const APP_ID: &str = "io.github.tkilian.CosmicAppletAppTitle";
const HORIZONTAL_MAX_CHARS: usize = 56;
const VERTICAL_MAX_CHARS: usize = 18;
const EMPTY_TITLE: &str = "Desktop";

static AUTOSIZE_MAIN_ID: LazyLock<widget::Id> = LazyLock::new(|| widget::Id::new("autosize-main"));

pub fn run() -> cosmic::iced::Result {
    cosmic::applet::run::<Applet>(())
}

pub struct Applet {
    core: cosmic::app::Core,
    desktop_cache: DesktopEntryCache,
    active_window: Option<ActiveWindow>,
    active_icon: Option<widget::icon::Handle>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Wayland(WaylandUpdate),
}

impl Applet {
    fn max_chars(&self) -> usize {
        match self.core.applet.anchor {
            PanelAnchor::Left | PanelAnchor::Right => VERTICAL_MAX_CHARS,
            PanelAnchor::Top | PanelAnchor::Bottom => HORIZONTAL_MAX_CHARS,
        }
    }

    fn display_title(&self) -> String {
        truncate_title(
            self.active_window
                .as_ref()
                .map(|window| window.title.as_str())
                .unwrap_or(EMPTY_TITLE),
            self.max_chars(),
        )
    }

    fn resolve_icon(&mut self, active_window: &ActiveWindow) -> Option<widget::icon::Handle> {
        let app_id = active_window
            .app_id
            .as_deref()
            .or(active_window.identifier.as_deref())?;

        let mut lookup = DesktopLookupContext::new(app_id).with_title(active_window.title.as_str());
        if let Some(identifier) = active_window.identifier.as_deref() {
            lookup = lookup.with_identifier(identifier);
        }

        let entry = resolve_desktop_entry(
            &mut self.desktop_cache,
            &lookup,
            &DesktopResolveOptions::default(),
        );
        let icon = fde::IconSource::from_unknown(entry.icon().unwrap_or(&entry.appid));
        Some(icon.as_cosmic_icon())
    }
}

impl cosmic::Application for Applet {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();

    const APP_ID: &'static str = APP_ID;

    fn init(core: cosmic::app::Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        (
            Self {
                core,
                desktop_cache: DesktopEntryCache::new(fde::get_languages_from_env()),
                active_window: None,
                active_icon: None,
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
            Message::Wayland(update) => match update {
                WaylandUpdate::ActiveWindow(active_window) => {
                    self.active_icon = active_window
                        .as_ref()
                        .and_then(|active_window| self.resolve_icon(active_window));
                    self.active_window = active_window;
                }
                WaylandUpdate::Finished => {
                    tracing::error!("Wayland subscription ended");
                }
            },
        }

        app::Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        active_window_subscription().map(Message::Wayland)
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let height = (self.core.applet.suggested_size(true).1
            + 2 * self.core.applet.suggested_padding(true).1) as f32;
        let icon_size = self.core.applet.suggested_size(true).0 as f32;
        let mut content = row![].align_y(Alignment::Center).spacing(6);

        if let Some(icon) = self.active_icon.clone() {
            content = content.push(
                widget::icon(icon)
                    .width(Length::Fixed(icon_size))
                    .height(Length::Fixed(icon_size)),
            );
        }

        content = content
            .push(self.core.applet.text(self.display_title()))
            .push(space::vertical().height(Length::Fixed(height)));

        let content = container(content).padding([0, self.core.applet.suggested_padding(true).0]);

        autosize::autosize(content, AUTOSIZE_MAIN_ID.clone()).into()
    }
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
