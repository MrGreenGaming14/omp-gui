//! The top bar: project / session breadcrumb, a waiting badge, the node the
//! session runs on with its online state, and the right panel toggle.

use crate::data::NodeStatus;
use crate::theme::style;
use iced::widget::{button, container, row, space, text};
use iced::{Alignment, Element, Fill, Task};

/// Container id of the waiting badge.
pub const WAITING_BADGE_ID: &str = "loa-topbar-waiting";

/// Container id of the node indicator.
pub const NODE_INDICATOR_ID: &str = "loa-topbar-node";

/// Label of the panel toggle while the panel is closed.
pub const SHOW_PANEL: &str = "Show panel";

/// Label of the panel toggle while the panel is open.
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

    /// Tells the top bar whether the right panel is open, for the toggle label.
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
        let mut bar = row![].spacing(8).align_y(Alignment::Center);

        match &self.crumb {
            None => bar = bar.push(text("No session").style(style::muted)),
            Some(crumb) => {
                if let Some(project) = &crumb.project {
                    bar = bar.push(
                        button(text(project.as_str()).style(style::muted))
                            .on_press(Message::OpenProject(project.clone()))
                            .style(style::row)
                            .padding([2, 6]),
                    );
                    bar = bar.push(text("/").style(style::muted));
                }
                bar = bar.push(text(crumb.title.as_str()).wrapping(text::Wrapping::None));
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
            let state = match (node.local, node.online) {
                (true, _) => "this device",
                (false, true) => "online",
                (false, false) => "offline",
            };
            let online = node.local || node.online;
            bar = bar.push(
                container(
                    row![
                        container(space()).width(8).height(8).style(style::status_dot(online)),
                        text(node.name.as_str()).size(13),
                        text(state).size(12).style(style::muted),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                )
                .id(NODE_INDICATOR_ID)
                .style(style::badge)
                .padding([3, 10]),
            );
        }

        bar = bar.push(
            button(text(if self.panel_open { HIDE_PANEL } else { SHOW_PANEL }).size(13))
                .on_press(Message::TogglePanel)
                .style(style::row)
                .padding([4, 8]),
        );

        container(bar.padding([6, 12])).style(style::chrome).width(Fill).into()
    }
}
