//! Settings-style screens the app composes: sign in, the model picker and model settings
//! ([`models`]), a project ([`projects`]), peers and grants ([`peers`]) and updates
//! ([`updates`]). Each is a kit [`Screen`](loa_ui_kit::widgets::Screen) over
//! `Msg<Local, Event>`: the view emits events the app acts on directly, guarded against the
//! current state, and local messages go through the screen's reducer. Screens show data the app
//! hands them and never load or store anything themselves.

pub mod models;
pub mod peers;
pub mod projects;
pub mod updates;

use iced::widget::{Space, button, checkbox, column, container, pick_list, row, scrollable, text, text_input};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{LayoutRole, RadiusRole, SpaceRole, TextRole, sp, text_size, tokens, width};
use loa_ui_kit::widgets::{label, section_header};
use std::borrow::Borrow;

/// A text button in `kind`, disabled without `on`.
pub(crate) fn action<'a, M: Clone + 'a>(s: impl text::IntoFragment<'a>, kind: ButtonKind, on: Option<M>) -> button::Button<'a, M> {
    button(text(s).size(text_size(TextRole::Label))).padding([sp(SpaceRole::Xs), sp(SpaceRole::S)]).style(style::button(kind)).on_press_maybe(on)
}

/// A small round status light in `tone`.
pub(crate) fn dot<'a, M: 'a>(tone: Tone) -> Element<'a, M> {
    let d = sp(SpaceRole::Xs) + 2.0;
    container(Space::new()).width(d).height(d).style(style::fill(tone.role(), tokens().radius(RadiusRole::Dot))).into()
}

/// A text input in the kit look.
pub(crate) fn field<'a, M: Clone + 'a>(placeholder: &str, value: &str, on_input: impl Fn(String) -> M + 'a) -> text_input::TextInput<'a, M> {
    text_input(placeholder, value).on_input(on_input).size(text_size(TextRole::Body)).padding(sp(SpaceRole::Xs)).style(style::input)
}

/// `content` after a muted label in the label column, as kit forms lay out fields.
pub(crate) fn labeled<'a, M: 'a>(name: impl text::IntoFragment<'a>, content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    let name = container(label(name, TextRole::Label, Tone::Muted)).width(width(LayoutRole::LabelColumn));
    row![name, content.into()].spacing(sp(SpaceRole::M)).align_y(Alignment::Center).into()
}

/// A pick list in the kit look.
pub(crate) fn pick<'a, T, L, M>(options: L, selected: Option<T>, placeholder: &str, on: impl Fn(T) -> M + 'a) -> Element<'a, M>
where
    T: ToString + PartialEq + Clone + 'a,
    L: Borrow<[T]> + 'a,
    M: Clone + 'a,
{
    pick_list(options, selected, on).placeholder(placeholder).text_size(text_size(TextRole::Label)).padding([sp(SpaceRole::Xs), sp(SpaceRole::S)]).style(style::pick).into()
}

pub(crate) fn check<'a, M: Clone + 'a>(on: bool, s: impl text::IntoFragment<'a>, toggle: impl Fn(bool) -> M + 'a) -> Element<'a, M> {
    checkbox(on).label(s).size(text_size(TextRole::Body)).text_size(text_size(TextRole::Label)).style(style::check).on_toggle(toggle).into()
}

/// A headed section of a screen.
pub(crate) fn section<'a, M: 'a>(title: impl text::IntoFragment<'a>, body: impl Into<Element<'a, M>>) -> Element<'a, M> {
    column![section_header(title, []), body.into()].spacing(sp(SpaceRole::S)).into()
}

/// Problems in a danger box, one per line.
pub(crate) fn errors<'a, M: 'a>(lines: impl IntoIterator<Item = String>) -> Element<'a, M> {
    let lines = column(lines.into_iter().map(|l| label(l, TextRole::Label, Tone::Danger).into())).spacing(sp(SpaceRole::Xs));
    container(lines).padding(sp(SpaceRole::S)).width(Fill).style(style::container(Surface::ErrorBox)).into()
}

/// A screen body in a scroller.
pub(crate) fn scroll<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    scrollable(container(content).padding(sp(SpaceRole::M)).width(Fill)).style(style::scrollbar(false)).height(Fill).into()
}
