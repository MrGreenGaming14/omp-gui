//! The top bar: project / session breadcrumb, a needs-you label, the session
//! status, the right panel toggle and the chat full screen button.
//!
//! The machine a session runs on lives in the sidebar's machine switcher;
//! the breadcrumb names it only for sessions on another machine.

use crate::data::NodeStatus;
use crate::theme::style;
use iced::widget::{button, container, row, space, text, tooltip};
use iced::{Alignment, Element, Fill, Task};
use loa_ui_icons::{Icon, muted};

/// Height of the top bar, shared with the panel tab bar so their bottom
/// borders line up.
pub const HEIGHT: f32 = 52.0;

/// Container id of the waiting badge.
pub const WAITING_BADGE_ID: &str = "loa-topbar-waiting";

/// Container id of the panel toggle button.
pub const PANEL_TOGGLE_ID: &str = "loa-topbar-panel-toggle";

/// Tooltip of the panel toggle while the panel is closed.
pub const SHOW_PANEL: &str = "Show panel";

/// Tooltip of the panel toggle while the panel is open.
pub const HIDE_PANEL: &str = "Hide panel";

/// The session the top bar describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breadcrumb {
    /// Project of the session, if any.
    pub project: Option<String>,
    /// Session title.
    pub title: String,
    /// Whether a decision waits for an answer.
    pub waiting: bool,
    /// Node the session runs on.
    pub node: NodeStatus,
}

/// Top bar state.
#[derive(Debug, Clone, Default)]
pub struct State {
    crumb: Option<Breadcrumb>,
    status: Option<String>,
    panel_open: bool,
    full: bool,
}

/// Internal messages.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The project part of the breadcrumb was clicked.
    OpenProject(String),
    /// The panel toggle was clicked.
    TogglePanel,
    /// The full screen button was clicked.
    ToggleFull,
}

/// What the app must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Show this project.
    OpenProject(String),
    /// Open or close the right panel.
    TogglePanel,
    /// Enter or leave chat full screen.
    ToggleFull,
}

impl State {
    /// Sets the session shown, or `None` when no session is open.
    pub fn set_breadcrumb(&mut self, crumb: Option<Breadcrumb>) {
        self.crumb = crumb;
    }

    /// The session shown.
    pub fn breadcrumb(&self) -> Option<&Breadcrumb> {
        self.crumb.as_ref()
    }

    /// Sets the short status of the session shown next to the node, e.g.
    /// "Running" or "Interrupted", or `None` to hide it.
    pub fn set_status(&mut self, status: Option<String>) {
        self.status = status;
    }

    /// The session status shown.
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Tells the top bar whether the right panel is open, for the toggle
    /// tooltip.
    pub fn set_panel_open(&mut self, open: bool) {
        self.panel_open = open;
    }

    /// Tells the top bar whether the chat fills the window.
    pub fn set_full(&mut self, full: bool) {
        self.full = full;
    }

    /// Handles a message.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::OpenProject(project) => Event::OpenProject(project),
            Message::TogglePanel => Event::TogglePanel,
            Message::ToggleFull => Event::ToggleFull,
        };
        (Task::none(), Some(event))
    }

    /// Draws the top bar.
    pub fn view(&self) -> Element<'_, Message> {
        let mut bar = row![].spacing(8).align_y(Alignment::Center);

        match &self.crumb {
            None => bar = bar.push(text("New session").size(14).style(style::muted)),
            Some(crumb) => {
                if let Some(project) = &crumb.project {
                    let place = if crumb.node.local { project.clone() } else { format!("{project} on {}", crumb.node.name) };
                    bar = bar.push(
                        button(text(place).size(14).style(style::muted))
                            .on_press(Message::OpenProject(project.clone()))
                            .style(style::ghost_button)
                            .padding([2, 4]),
                    );
                    bar = bar.push(muted(Icon::ChevronRight, 13.0));
                } else if !crumb.node.local {
                    bar = bar.push(text!("on {}", crumb.node.name).size(14).style(style::muted));
                    bar = bar.push(muted(Icon::ChevronRight, 13.0));
                }
                bar = bar.push(text(crumb.title.as_str()).size(14).font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }).wrapping(text::Wrapping::None));
                if crumb.waiting {
                    bar = bar.push(
                        container(text("· Needs you").size(13).style(style::waiting_text)).id(WAITING_BADGE_ID),
                    );
                }
            }
        }

        bar = bar.push(space().width(Fill));

        if let Some(status) = &self.status {
            bar = bar.push(container(text(status.as_str()).size(12).style(style::faint)).padding([0, 6]));
        }

        let toggle = button(muted(Icon::PanelRight, 16.0)).on_press(Message::TogglePanel).style(style::ghost_button).padding(7);
        let label = container(text(if self.panel_open { HIDE_PANEL } else { SHOW_PANEL }).size(12))
            .style(style::tooltip)
            .padding([3, 8]);
        bar = bar.push(container(tooltip(toggle, label, tooltip::Position::Bottom)).id(PANEL_TOGGLE_ID));

        let full = button(muted(if self.full { Icon::Minimize } else { Icon::Maximize }, 15.0))
            .on_press(Message::ToggleFull)
            .style(style::ghost_button)
            .padding(7);
        let full_label = container(text(if self.full { "Exit full screen" } else { "Full screen chat" }).size(12))
            .style(style::tooltip)
            .padding([3, 8]);
        bar = bar.push(tooltip(full, full_label, tooltip::Position::Bottom));

        container(bar.padding([0, 12])).style(style::chrome).width(Fill).center_y(HEIGHT).into()
    }
}
