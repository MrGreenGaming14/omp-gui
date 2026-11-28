//! The top bar: breadcrumb, needs-you label, session status, panel toggle and chat full screen; clicks are frame [`Message`]s.

use crate::layout::{Message, Pane, State, open_project};
use iced::widget::{button, container, row, text, tooltip};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::icons::tinted;
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::text::{EllipsisMode, ellipsize_text, measure_text_width};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, pad_x, sp, text_size as ts, tokens};
use loa_ui_kit::widgets::{icon_button, label, width_responsive};
use loa_view::theme::Icon;

/// Height of the top bar, shared with the panel tab bar so their borders line up.
pub const HEIGHT: f32 = 52.0;

pub(crate) fn view(frame: &State) -> Element<'_, Message> {
    let gap = sp(Sp::S);
    let status = frame.status.as_deref().map(|s| container(label(s, T::Caption, Tone::Faint).wrapping(text::Wrapping::None)).padding(pad_x(Sp::Xs)));
    let toggle = icon_button(Icon::PanelRight, if frame.panel_open() { "Hide panel" } else { "Show panel" }, Some(Message::TogglePanel));
    let (icon, tip) = if frame.full() == Some(Pane::Center) { (Icon::Minimize, "Exit full screen") } else { (Icon::Maximize, "Full screen chat") };
    // The breadcrumb takes what the controls leave and shortens to fit it.
    let crumbs = width_responsive(move |width| breadcrumb(frame, width));
    let bar = row![crumbs].push(status).push(toggle).push(icon_button(icon, tip, Some(Message::ToggleFull(Pane::Center)))).spacing(gap).align_y(Alignment::Center);
    container(bar.padding(pad_x(Sp::M))).style(style::container(Surface::Ground)).width(Fill).center_y(HEIGHT).clip(true).into()
}

/// The breadcrumb, its project and title ellipsized to fit `left_budget`; a shortened piece shows
/// its full text on hover.
fn breadcrumb(frame: &State, left_budget: f32) -> Element<'_, Message> {
    let body_size = ts(T::Body);
    let label_size = ts(T::Label);
    let body_font = tokens().font(T::Body);
    let bold_font = iced::Font { weight: iced::font::Weight::Bold, ..body_font };
    let label_font = tokens().font(T::Label);
    let gap = sp(Sp::S);

    let chevron = || tinted(Icon::ChevronRight, T::Label, Tone::Muted);
    let strong = |s: String| label(s, T::Body, Tone::Normal).font(bold_font).wrapping(text::Wrapping::None);

    let mut left = row![].spacing(gap).align_y(Alignment::Center);
    match (&frame.sidebar().shown_project, frame.breadcrumb()) {
        (Some(project), _) => {
            let prefix_w = measure_text_width("Projects", body_size, body_font) + gap + label_size + gap;
            let title_budget = (left_budget - prefix_w).max(0.0);
            let shown = ellipsize_text(project, title_budget, body_size, bold_font, EllipsisMode::End);
            let title = tip_if_cut(strong(shown.clone()).into(), &shown, project);
            left = left
                .push(label("Projects", T::Body, Tone::Muted).wrapping(text::Wrapping::None))
                .push(chevron())
                .push(title);
        }
        (None, None) => {
            let full = "New session";
            let shown = ellipsize_text(full, left_budget, body_size, body_font, EllipsisMode::End);
            let title = tip_if_cut(label(shown.clone(), T::Body, Tone::Muted).wrapping(text::Wrapping::None).into(), &shown, full);
            left = left.push(title);
        }
        (None, Some((session, node))) => {
            let place = if let Some(project) = &session.project {
                Some((true, project.as_str(), if node.local { project.clone() } else { format!("{project} on {}", node.name) }))
            } else if !node.local {
                Some((false, "", format!("on {}", node.name)))
            } else {
                None
            };
            let crumb_overhead = match &place {
                Some((true, _, _)) => 2.0 * sp(Sp::Xs) + gap + label_size + gap,
                Some((false, _, _)) => gap + label_size + gap,
                None => 0.0,
            };
            let waiting_w = if session.waiting {
                gap + measure_text_width("· Needs you", label_size, label_font)
            } else {
                0.0
            };
            let content_budget = (left_budget - crumb_overhead - waiting_w).max(0.0);
            let title_full_w = measure_text_width(&session.title, body_size, bold_font);

            let title_budget = if let Some((clickable, project, full_place)) = place {
                let crumb_full_w = measure_text_width(&full_place, body_size, body_font);
                let title_reserve = title_full_w.min(content_budget * 0.55);
                let crumb_budget = (content_budget - title_reserve).min(crumb_full_w).max(0.0);
                let shown_crumb = ellipsize_text(&full_place, crumb_budget, body_size, body_font, EllipsisMode::End);
                let actual_crumb_w = measure_text_width(&shown_crumb, body_size, body_font);
                let crumb_widget: Element<'_, Message> = if clickable {
                    let open = button(label(shown_crumb.clone(), T::Body, Tone::Muted).wrapping(text::Wrapping::None))
                        .on_press(open_project(project))
                        .style(style::button(ButtonKind::Ghost))
                        .padding([sp(Sp::Xs) / 2.0, sp(Sp::Xs)]);
                    tip_if_cut(open.into(), &shown_crumb, &full_place)
                } else {
                    tip_if_cut(label(shown_crumb.clone(), T::Body, Tone::Muted).wrapping(text::Wrapping::None).into(), &shown_crumb, &full_place)
                };
                left = left.push(crumb_widget).push(chevron());
                (content_budget - actual_crumb_w).max(0.0)
            } else {
                content_budget
            };

            let shown_title = ellipsize_text(&session.title, title_budget, body_size, bold_font, EllipsisMode::End);
            let title = tip_if_cut(strong(shown_title.clone()).into(), &shown_title, &session.title);
            left = left
                .push(title)
                .push(session.waiting.then(|| label("· Needs you", T::Label, Tone::Warn).wrapping(text::Wrapping::None)));
        }
    }

    container(left).width(Fill).clip(true).into()
}

/// `el`, with `full` on hover when it was shortened to `shown`.
fn tip_if_cut<'a>(el: Element<'a, Message>, shown: &str, full: &str) -> Element<'a, Message> {
    if shown == full {
        return el;
    }
    let tip = container(label(full.to_owned(), T::Caption, Tone::Normal)).padding([sp(Sp::Xs), sp(Sp::S)]).style(style::container(Surface::Tooltip));
    tooltip(el, tip, tooltip::Position::Bottom).into()
}
