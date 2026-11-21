//! The top bar: the project / session breadcrumb, a needs-you label, the session status, the
//! panel toggle and the chat full screen button. Its clicks are frame [`Message`]s.

use crate::data::NodeStatus;
use crate::layout::{Message, Pane};
use iced::widget::{button, container, row, space, text};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, pad_x, sp, tokens};
use loa_ui_kit::widgets::{icon_button, label};

/// Height of the top bar, shared with the panel tab bar so their borders line up.
pub const HEIGHT: f32 = 52.0;

/// The session the top bar describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breadcrumb {
    pub project: Option<String>,
    pub title: String,
    /// A decision waits for an answer.
    pub waiting: bool,
    pub node: NodeStatus,
}

#[derive(Debug, Clone, Default)]
pub struct State {
    pub(crate) crumb: Option<Breadcrumb>,
    /// A project page fills the center: shown instead of the session crumb.
    pub(crate) project_page: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) panel_open: bool,
    pub(crate) full: bool,
}

impl State {
    pub fn breadcrumb(&self) -> Option<&Breadcrumb> { self.crumb.as_ref() }

    /// The short session status shown, e.g. "Running".
    pub fn status(&self) -> Option<&str> { self.status.as_deref() }

    pub fn view<'a>(&'a self) -> Element<'a, Message> {
        let mut bar = row![].spacing(sp(Sp::S)).align_y(Alignment::Center);
        let chevron = || tinted(Icon::ChevronRight, T::Label, Tone::Muted);
        let strong = |s: &'a str| label(s, T::Body, Tone::Normal).font(iced::Font { weight: iced::font::Weight::Bold, ..tokens().font(T::Body) }).wrapping(text::Wrapping::None);
        match (&self.project_page, &self.crumb) {
            (Some(project), _) => bar = bar.push(label("Projects", T::Body, Tone::Muted)).push(chevron()).push(strong(project)),
            (None, None) => bar = bar.push(label("New session", T::Body, Tone::Muted)),
            (None, Some(crumb)) => {
                if let Some(project) = &crumb.project {
                    let place = if crumb.node.local { project.clone() } else { format!("{project} on {}", crumb.node.name) };
                    let open = button(label(place, T::Body, Tone::Muted)).on_press(Message::OpenProjectTab(project.clone()));
                    bar = bar.push(open.style(style::button(ButtonKind::Ghost)).padding([sp(Sp::Xs) / 2.0, sp(Sp::Xs)])).push(chevron());
                } else if !crumb.node.local {
                    bar = bar.push(label(format!("on {}", crumb.node.name), T::Body, Tone::Muted)).push(chevron());
                }
                bar = bar.push(strong(&crumb.title)).push(crumb.waiting.then(|| label("· Needs you", T::Label, Tone::Warn)));
            }
        }
        let status = self.status.as_deref().map(|s| container(label(s, T::Caption, Tone::Faint)).padding(pad_x(Sp::Xs)));
        let toggle = icon_button(Icon::PanelRight, if self.panel_open { "Hide panel" } else { "Show panel" }, Some(Message::TogglePanel));
        let (icon, tip) = if self.full { (Icon::Minimize, "Exit full screen") } else { (Icon::Maximize, "Full screen chat") };
        let bar = bar.push(space().width(Fill)).push(status).push(toggle).push(icon_button(icon, tip, Some(Message::ToggleFull(Pane::Center))));
        container(bar.padding(pad_x(Sp::M))).style(style::container(Surface::Ground)).width(Fill).center_y(HEIGHT).into()
    }
}
