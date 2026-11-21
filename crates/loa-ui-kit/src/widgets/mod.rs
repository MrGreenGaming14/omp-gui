//! Shared widgets, all styled through [`crate::style`] recipes and sized by token roles, plus
//! the component ports screens implement.

mod form;
pub mod popover;
pub mod virtual_list;

pub use form::{Field, Form, FormSpec, FormState, Picker, PickerItem, PickerMsg, form_view, list_editor};
pub use popover::{Popover, popover};
pub use virtual_list::{Heights, scroll_target};

use crate::icons::{Icon, tinted};
use crate::style::{self, ButtonKind, Surface, Tone};
use crate::theme::{ColorRole, LayoutRole, RadiusRole, SpaceRole, TextRole, tokens};
use iced::widget::{Space, button, column, container, mouse_area, row, rule, text, tooltip};
use iced::{Alignment, Element, Fill, FillPortion, Subscription, Task, mouse};

/// A screen or pane: local state with messages in and events out.
pub trait Component {
    type Message: Clone;
    type Event;
    fn update(&mut self, m: Self::Message) -> (Task<Self::Message>, Option<Self::Event>);
    fn view(&self) -> Element<'_, Self::Message>;
    fn subscription(&self) -> Subscription<Self::Message> { Subscription::none() }
}

/// A right-panel tab the frame lists and draws.
pub trait PanelPane {
    type Message;
    fn id(&self) -> &str;
    fn title(&self) -> &str;
    fn icon(&self) -> Icon;
    fn view(&self) -> Element<'_, Self::Message>;
}

/// The message convention of screen crates: handled locally, or emitted to the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg<L, E> {
    Local(L),
    Emit(E),
}

fn sp(r: SpaceRole) -> f32 { tokens().space(r) }
fn size(r: TextRole) -> f32 { tokens().text(r) }

/// `s` at text role `r` in `tone`.
pub fn label<'a>(s: impl text::IntoFragment<'a>, r: TextRole, tone: Tone) -> text::Text<'a> {
    text(s).size(size(r)).font(tokens().font(r)).style(style::text(tone))
}

pub fn card<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> container::Container<'a, M> {
    container(content).padding(sp(SpaceRole::M)).style(style::container(Surface::Card))
}

pub fn badge<'a, M: 'a>(s: impl text::IntoFragment<'a>, tone: Tone) -> Element<'a, M> {
    container(label(s, TextRole::Caption, tone)).padding([0.0, sp(SpaceRole::Xs)]).style(style::container(Surface::Badge)).into()
}

/// A ghost icon button with a tooltip; disabled without `on`.
pub fn icon_button<'a, M: Clone + 'a>(icon: Icon, tip: &'a str, on: Option<M>) -> Element<'a, M> {
    let b = button(tinted(icon, TextRole::Body, Tone::Muted)).padding(sp(SpaceRole::Xs)).style(style::button(ButtonKind::Ghost)).on_press_maybe(on);
    let tip = container(label(tip, TextRole::Caption, Tone::Normal)).padding([sp(SpaceRole::Xs), sp(SpaceRole::S)]);
    tooltip(b, tip.style(style::container(Surface::Tooltip)), tooltip::Position::Bottom).into()
}

/// A full-width row with an optional icon.
pub fn menu_row<'a, M: Clone + 'a>(icon: Option<Icon>, s: impl text::IntoFragment<'a>, selected: bool, on: M) -> Element<'a, M> {
    let mut r = row![].spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
    if let Some(i) = icon { r = r.push(tinted(i, TextRole::Body, Tone::Muted)) }
    row_button(r.push(label(s, TextRole::Body, Tone::Normal)).into(), selected, on)
}

/// A row with a title over a faint detail line.
pub fn two_line_row<'a, M: Clone + 'a>(title: impl text::IntoFragment<'a>, detail: impl text::IntoFragment<'a>, selected: bool, on: M) -> Element<'a, M> {
    row_button(column![label(title, TextRole::Body, Tone::Normal), label(detail, TextRole::Caption, Tone::Faint)].into(), selected, on)
}

fn row_button<'a, M: Clone + 'a>(content: Element<'a, M>, selected: bool, on: M) -> Element<'a, M> {
    button(content).width(Fill).padding([sp(SpaceRole::Xs), sp(SpaceRole::S)]).style(style::button(ButtonKind::Row { selected })).on_press(on).into()
}

pub fn mono_block<'a, M: 'a>(s: impl text::IntoFragment<'a>) -> Element<'a, M> {
    container(label(s, TextRole::Mono, Tone::Normal)).width(Fill).padding(sp(SpaceRole::S)).style(style::container(Surface::Well)).into()
}

/// A usage bar: accent below half, warn from half, danger from 80%.
pub fn meter<'a, M: 'a>(fraction: f32) -> Element<'a, M> {
    let f = fraction.clamp(0.0, 1.0);
    let role = if f >= 0.8 { ColorRole::Danger } else if f >= 0.5 { ColorRole::Warn } else { ColorRole::Accent };
    let dot = tokens().radius(RadiusRole::Dot);
    let filled = (f * 1000.0) as u16;
    let bar = container(Space::new()).width(FillPortion(filled.max(1))).height(Fill).style(style::fill(role, dot));
    let bar = row![bar, Space::new().width(FillPortion(1000 - filled.min(999)))];
    container(bar).width(Fill).height(sp(SpaceRole::Xs) + 2.0).style(style::fill(ColorRole::LineStrong, dot)).into()
}

pub fn section_header<'a, M: 'a>(title: impl text::IntoFragment<'a>, actions: impl IntoIterator<Item = Element<'a, M>>) -> Element<'a, M> {
    let head = row![label(title, TextRole::Heading, Tone::Normal), Space::new().width(Fill)].align_y(Alignment::Center);
    head.extend(actions).spacing(sp(SpaceRole::S)).into()
}

/// An icon, a label, a toned status word and an optional faint detail.
pub fn status_row<'a, M: 'a>(icon: Icon, name: &'a str, status: &'a str, tone: Tone, detail: Option<&'a str>) -> Element<'a, M> {
    let mut r = row![tinted(icon, TextRole::Body, Tone::Muted), label(name, TextRole::Body, Tone::Normal), Space::new().width(Fill)];
    if let Some(d) = detail { r = r.push(label(d, TextRole::Caption, Tone::Faint)) }
    r.push(label(status, TextRole::Label, tone)).spacing(sp(SpaceRole::S)).align_y(Alignment::Center).into()
}

pub fn empty_state<'a, M: 'a>(icon: Icon, title: &'a str, hint: &'a str) -> Element<'a, M> {
    let body = column![tinted(icon, TextRole::Title, Tone::Faint), label(title, TextRole::Heading, Tone::Normal), label(hint, TextRole::Label, Tone::Muted)];
    container(body.spacing(sp(SpaceRole::S)).align_x(Alignment::Center)).center(Fill).into()
}

/// A chevron header that shows `body` while open.
pub fn collapsible<'a, M: Clone + 'a>(title: impl text::IntoFragment<'a>, open: bool, toggle: M, body: impl FnOnce() -> Element<'a, M>) -> Element<'a, M> {
    let chevron = tinted(if open { Icon::ChevronDown } else { Icon::ChevronRight }, TextRole::Caption, Tone::Muted);
    let head = button(row![chevron, label(title, TextRole::Label, Tone::Muted)].spacing(sp(SpaceRole::Xs)).align_y(Alignment::Center));
    let head = head.padding(0).style(style::button(ButtonKind::Flat)).on_press(toggle);
    if open { column![head, body()].spacing(sp(SpaceRole::Xs)).into() } else { head.into() }
}

/// One tab of a [`tab_strip`].
pub struct Tab<'a, M> {
    pub label: &'a str,
    pub icon: Option<Icon>,
    pub active: bool,
    pub on: M,
}

/// A row of pill tabs.
pub fn tab_strip<'a, M: Clone + 'a>(tabs: impl IntoIterator<Item = Tab<'a, M>>) -> Element<'a, M> {
    let pills = tabs.into_iter().map(|t| {
        let mut r = row![].spacing(sp(SpaceRole::Xs)).align_y(Alignment::Center);
        if let Some(i) = t.icon { r = r.push(tinted(i, TextRole::Label, if t.active { Tone::Normal } else { Tone::Muted })) }
        let r = r.push(text(t.label).size(size(TextRole::Label)));
        button(r).padding([sp(SpaceRole::Xs), sp(SpaceRole::S)]).style(style::button(ButtonKind::Tab { active: t.active })).on_press(t.on).into()
    });
    row(pills).spacing(sp(SpaceRole::Xs)).align_y(Alignment::Center).into()
}

/// A settings-style window body: a nav column of `items` beside `body`.
pub fn side_nav<'a, T: Copy + PartialEq + std::fmt::Display, M: Clone + 'a>(items: &[T], selected: T, on: impl Fn(T) -> M, body: Element<'a, M>) -> Element<'a, M> {
    let nav = column(items.iter().map(|&i| menu_row(None, i.to_string(), i == selected, on(i)))).spacing(sp(SpaceRole::Xs));
    let nav = container(nav).width(tokens().layout(LayoutRole::NavWidth) * 0.75).height(Fill).padding(sp(SpaceRole::S));
    row![nav.style(style::container(Surface::Side)), rule::vertical(1).style(style::rule), container(body).width(Fill).padding(sp(SpaceRole::L))].into()
}

/// A grab area on a pane edge, drawn as the 1px divider.
pub fn splitter_edge<'a, M: Clone + 'a>(on_press: M) -> Element<'a, M> {
    let line = container(rule::vertical(1).style(style::rule)).center_x(sp(SpaceRole::Xs) + 2.0).height(Fill);
    mouse_area(line).on_press(on_press).interaction(mouse::Interaction::ResizingHorizontally).into()
}
