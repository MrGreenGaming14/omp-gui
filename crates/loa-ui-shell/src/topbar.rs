//! The top bar: project / session breadcrumb, a waiting badge, the node the
//! session runs on with its online state, the session status and the right
//! panel toggle.

use crate::data::NodeStatus;
use crate::theme::style;
use iced::widget::{button, container, row, space, text, tooltip};
use iced::{Alignment, Element, Fill, Task};
use loa_ui_icons::{Icon, icon};

/// Height of the top bar, shared with the panel tab bar so their bottom
/// borders line up.
pub const HEIGHT: f32 = 40.0;

/// Container id of the waiting badge.
pub const WAITING_BADGE_ID: &str = "loa-topbar-waiting";

/// Container id of the node indicator.
pub const NODE_INDICATOR_ID: &str = "loa-topbar-node";

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
}

/// Internal messages.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The project part of the breadcrumb was clicked.
    OpenProject(String),
    /// The panel toggle was clicked.
    TogglePanel,
}

/// What the app must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Show this project.
    OpenProject(String),
    /// Open or close the right panel.
    TogglePanel,
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

    /// Handles a message.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::OpenProject(project) => Event::OpenProject(project),
            Message::TogglePanel => Event::TogglePanel,
        };
        (Task::none(), Some(event))
    }

    /// Draws the top bar.
    pub fn view(&self) -> Element<'_, Message> {
        let mut bar = row![].spacing(6).align_y(Alignment::Center);

        match &self.crumb {
            None => bar = bar.push(text("No session").size(14).style(style::muted)),
            Some(crumb) => {
                if let Some(project) = &crumb.project {
                    bar = bar.push(
                        button(text(project.as_str()).size(14).style(style::muted))
                            .on_press(Message::OpenProject(project.clone()))
                            .style(style::row)
                            .padding([2, 2]),
                    );
                    bar = bar.push(text("/").size(14).style(style::muted));
                }
                bar = bar.push(text(crumb.title.as_str()).size(14).wrapping(text::Wrapping::None));
                if crumb.waiting {
                    bar = bar.push(
                        container(text("Waiting for you").size(12))
                            .id(WAITING_BADGE_ID)
                            .style(style::waiting_badge)
                            .padding([2, 8]),
                    );
                }
            }
        }

        bar = bar.push(space().width(Fill));

        if let Some(crumb) = &self.crumb {
            let node = &crumb.node;
            let online = node.local || node.online;
            bar = bar.push(
                container(
                    row![
                        container(space()).width(7).height(7).style(style::status_dot(online)),
                        row![
                            text(node.name.as_str()).size(14),
                            text(if online { "online" } else { "offline" }).size(14),
                        ]
                        .spacing(4),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .id(NODE_INDICATOR_ID)
                .style(style::outlined)
                .padding([3, 10]),
            );
        }

        if let Some(status) = &self.status {
            bar = bar.push(container(text(status.as_str()).size(12).style(style::muted)).padding([0, 6]));
        }

        let toggle = button(icon(Icon::PanelRight, 15.0)).on_press(Message::TogglePanel).style(style::row).padding(5);
        let label = container(text(if self.panel_open { HIDE_PANEL } else { SHOW_PANEL }).size(12))
            .style(style::tooltip)
            .padding([3, 8]);
        bar = bar.push(container(tooltip(toggle, label, tooltip::Position::Bottom)).id(PANEL_TOGGLE_ID));

        container(bar.padding([0, 14])).style(style::chrome).width(Fill).center_y(HEIGHT).into()
    }
}
