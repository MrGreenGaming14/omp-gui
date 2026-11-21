//! The top bar: project / session breadcrumb, a needs-you label, the session
//! status, the right panel toggle and the chat full screen button.
//!
//! The machine a session runs on lives in the sidebar's machine switcher;
//! the breadcrumb names it only for sessions on another machine.

use crate::data::NodeStatus;
use iced::widget::{button, container, row, space, text};
use iced::{Alignment, Element, Fill, Task};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, pad_x, sp, tokens};
use loa_ui_kit::widgets::{icon_button, label};

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
    /// A project page in the center: shown instead of the session crumb.
    project_page: Option<String>,
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

    /// Shows "Projects › name" while a project's page fills the center.
    pub fn set_project_page(&mut self, project: Option<String>) {
        self.project_page = project;
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
    pub fn view<'a>(&'a self) -> Element<'a, Message> {
        let mut bar = row![].spacing(sp(Sp::S)).align_y(Alignment::Center);
        let chevron = || tinted(Icon::ChevronRight, T::Label, Tone::Muted);
        let strong = |s: &'a str| label(s, T::Body, Tone::Normal).font(iced::Font { weight: iced::font::Weight::Bold, ..tokens().font(T::Body) }).wrapping(text::Wrapping::None);

        match (&self.project_page, &self.crumb) {
            (Some(project), _) => {
                bar = bar.push(label("Projects", T::Body, Tone::Muted)).push(chevron()).push(strong(project));
            }
            (None, None) => bar = bar.push(label("New session", T::Body, Tone::Muted)),
            (None, Some(crumb)) => {
                if let Some(project) = &crumb.project {
                    let place = if crumb.node.local { project.clone() } else { format!("{project} on {}", crumb.node.name) };
                    bar = bar.push(
                        button(label(place, T::Body, Tone::Muted))
                            .on_press(Message::OpenProject(project.clone()))
                            .style(style::button(ButtonKind::Ghost))
                            .padding([sp(Sp::Xs) / 2.0, sp(Sp::Xs)]),
                    );
                    bar = bar.push(chevron());
                } else if !crumb.node.local {
                    bar = bar.push(label(format!("on {}", crumb.node.name), T::Body, Tone::Muted)).push(chevron());
                }
                bar = bar.push(strong(&crumb.title));
                if crumb.waiting {
                    bar = bar.push(container(label("· Needs you", T::Label, Tone::Warn)).id(WAITING_BADGE_ID));
                }
            }
        }

        bar = bar.push(space().width(Fill));
        if let Some(status) = &self.status {
            bar = bar.push(container(label(status.as_str(), T::Caption, Tone::Faint)).padding(pad_x(Sp::Xs)));
        }
        let toggle = icon_button(Icon::PanelRight, if self.panel_open { HIDE_PANEL } else { SHOW_PANEL }, Some(Message::TogglePanel));
        bar = bar.push(container(toggle).id(PANEL_TOGGLE_ID));
        let full = if self.full { (Icon::Minimize, "Exit full screen") } else { (Icon::Maximize, "Full screen chat") };
        bar = bar.push(icon_button(full.0, full.1, Some(Message::ToggleFull)));

        container(bar.padding(pad_x(Sp::M))).style(style::container(Surface::Ground)).width(Fill).center_y(HEIGHT).into()
    }
}
