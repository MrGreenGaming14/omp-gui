//! The top bar: breadcrumb, needs-you label, session status, panel toggle and chat full screen; clicks are frame [`Message`]s.

use crate::layout::{Message, Pane, State, open_project};
use iced::widget::{button, container, row, space, text};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::icons::tinted;
use loa_view::theme::Icon;
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, pad_x, sp, tokens};
use loa_ui_kit::widgets::{icon_button, label};

/// Height of the top bar, shared with the panel tab bar so their borders line up.
pub const HEIGHT: f32 = 52.0;

pub(crate) fn view(frame: &State) -> Element<'_, Message> {
    let mut bar = row![].spacing(sp(Sp::S)).align_y(Alignment::Center);
    let chevron = || tinted(Icon::ChevronRight, T::Label, Tone::Muted);
    let strong = |s: &str| label(s.to_owned(), T::Body, Tone::Normal).font(iced::Font { weight: iced::font::Weight::Bold, ..tokens().font(T::Body) }).wrapping(text::Wrapping::None);
    match (&frame.sidebar().shown_project, frame.breadcrumb()) {
        (Some(project), _) => bar = bar.push(label("Projects", T::Body, Tone::Muted)).push(chevron()).push(strong(project)),
        (None, None) => bar = bar.push(label("New session", T::Body, Tone::Muted)),
        (None, Some((session, node))) => {
            if let Some(project) = &session.project {
                let place = if node.local { project.clone() } else { format!("{project} on {}", node.name) };
                let open = button(label(place, T::Body, Tone::Muted)).on_press(open_project(project));
                bar = bar.push(open.style(style::button(ButtonKind::Ghost)).padding([sp(Sp::Xs) / 2.0, sp(Sp::Xs)])).push(chevron());
            } else if !node.local {
                bar = bar.push(label(format!("on {}", node.name), T::Body, Tone::Muted)).push(chevron());
            }
            bar = bar.push(strong(&session.title)).push(session.waiting.then(|| label("· Needs you", T::Label, Tone::Warn)));
        }
    }
    let status = frame.status.as_deref().map(|s| container(label(s, T::Caption, Tone::Faint)).padding(pad_x(Sp::Xs)));
    let toggle = icon_button(Icon::PanelRight, if frame.panel_open() { "Hide panel" } else { "Show panel" }, Some(Message::TogglePanel));
    let (icon, tip) = if frame.full() == Some(Pane::Center) { (Icon::Minimize, "Exit full screen") } else { (Icon::Maximize, "Full screen chat") };
    let bar = bar.push(space().width(Fill)).push(status).push(toggle).push(icon_button(icon, tip, Some(Message::ToggleFull(Pane::Center))));
    container(bar.padding(pad_x(Sp::M))).style(style::container(Surface::Ground)).width(Fill).center_y(HEIGHT).into()
}
