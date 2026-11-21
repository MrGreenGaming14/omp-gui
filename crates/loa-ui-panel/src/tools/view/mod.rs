//! Rendering of the tools page, one module per section. Every list sits in a card with
//! dividers under a header that names and counts it; long lists show their first rows until
//! opened, and a tool's details open inline under its row.

mod hosts;
mod peers;
mod servers;

use super::catalog::{GroupView, ToolRow};
use super::{BuiltinGroup, DetailState, FieldError, FormId, Message, Section, State, ToolInfo};
use iced::widget::{Column, button, checkbox, column, container, pick_list, responsive, row, rule, scrollable, space, text, text_input};
use iced::{Alignment, Element, Fill, Font, Padding, font};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{LayoutRole, RadiusRole, SpaceRole as Sp, TextRole as T, pad_x, sp, text_size as ts, tokens, width};
use loa_ui_kit::widgets::label;

/// From this width cards sit in two columns.
const TWO_COLUMNS: f32 = 640.0;
/// Lists longer than this show only their first `PREVIEW` rows until opened.
const COLLAPSE_AT: usize = 8;
const PREVIEW: usize = 6;
const BOLD: Font = Font { weight: font::Weight::Bold, ..Font::DEFAULT };

impl State {
    pub fn view(&self) -> Element<'_, Message> { responsive(move |size| self.layout(size.width >= TWO_COLUMNS)).into() }

    fn layout(&self, wide: bool) -> Element<'_, Message> {
        let catalog = self.catalog();
        let hosts = if self.prompts.is_empty() { "Remote hosts".to_owned() } else { format!("Remote hosts ({})", self.prompts.len()) };
        let sections = [
            (Section::Builtin, "Built in".to_owned(), catalog.builtin.iter().map(|g| g.tools.len()).sum()),
            (Section::Servers, "MCP servers".to_owned(), catalog.servers.len()),
            (Section::Hosts, hosts, self.visible_hosts().len()),
            (Section::Peers, "Peers".to_owned(), self.peers.len()),
        ];
        let tabs = row(sections.into_iter().map(|(section, name, count)| {
            let active = section == self.section;
            let tone = if active { Tone::Normal } else if section == Section::Hosts && !self.prompts.is_empty() { Tone::Warn } else { Tone::Muted };
            let count = (count > 0).then(|| label(count.to_string(), T::Caption, Tone::Faint));
            let content = row![label(name, T::Label, tone).wrapping(text::Wrapping::None), count].spacing(sp(Sp::Xs)).align_y(Alignment::Center);
            button(content).padding([sp(Sp::Xs), sp(Sp::S)]).style(style::button(ButtonKind::Tab { active })).on_press(Message::SectionSelected(section)).into()
        }));
        let thin = sp(Sp::Xs) / 2.0;
        let tabs = scrollable(tabs.spacing(sp(Sp::L))).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(thin).scroller_width(thin))).style(style::scrollbar(false));
        let toggles = matches!(self.section, Section::Builtin | Section::Servers);
        let projects = toggles.then(|| {
            let selected = self.project.as_ref().and_then(|id| self.projects.iter().find(|p| &p.id == id));
            pick_list(self.projects.as_slice(), selected, Message::ProjectSelected).placeholder("Choose a project").text_size(ts(T::Caption)).padding([sp(Sp::Xs), sp(Sp::S)]).width(width(LayoutRole::LabelColumn)).style(style::pick)
        });
        let search = input("Filter tools, servers and hosts", &self.filter_text, Message::FilterChanged).width(Fill);
        let hint = (toggles && self.project.is_none()).then(|| label("Choose a project to switch tools on or off", T::Caption, Tone::Faint));
        let header = column![tabs, row![search, projects].spacing(sp(Sp::S)).align_y(Alignment::Center), hint].spacing(sp(Sp::S));
        let body = match self.section {
            Section::Builtin => {
                let groups: Vec<_> = catalog.builtin.into_iter().map(|g| self.group_view(g)).collect();
                if groups.is_empty() { label("No built in tool matches the filter", T::Label, Tone::Muted).into() } else { grid(groups, wide) }
            }
            Section::Servers => self.servers_view(catalog, wide),
            Section::Hosts => self.hosts_view(wide),
            Section::Peers => self.peers_view(),
        };
        let w = sp(Sp::Xs);
        let body = scrollable(container(body).padding(Padding::ZERO.top(w).bottom(sp(Sp::M)).right(sp(Sp::S)))).direction(scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(w).scroller_width(w).spacing(w)));
        column![container(header).padding(Padding::ZERO.right(sp(Sp::S))), body.style(style::scrollbar(false)).height(Fill)].spacing(sp(Sp::M)).padding(Padding::ZERO.top(sp(Sp::S)).right(thin).left(sp(Sp::M))).into()
    }

    /// Whether the card under header `key` shows; every card shows while filtering.
    fn block_open(&self, key: &str) -> bool { !self.filter.is_empty() || self.open_blocks.get(key).copied().unwrap_or(true) }

    /// A header that opens and closes its card (not while filtering), with `trailing` actions.
    fn block_header<'a>(&self, key: &str, title: String, count: usize, trailing: Option<Element<'a, Message>>) -> Element<'a, Message> {
        let (open, filtering) = (self.block_open(key), !self.filter.is_empty());
        let chevron = (!filtering).then(|| tinted(if open { Icon::ChevronDown } else { Icon::ChevronRight }, T::Caption, Tone::Muted));
        let name = row![chevron, label(title, T::Caption, Tone::Normal).wrapping(text::Wrapping::None), label(count.to_string(), T::Caption, Tone::Faint)].spacing(sp(Sp::Xs)).align_y(Alignment::Center);
        let toggle = button(name).padding([sp(Sp::Xs), 0.0]).style(style::button(ButtonKind::Flat)).on_press_maybe((!filtering).then(|| Message::BlockOpen(key.to_owned(), !open)));
        row![toggle, space().width(Fill), trailing].spacing(sp(Sp::S)).align_y(Alignment::Center).into()
    }

    fn group_view<'a>(&'a self, group: GroupView<'a>) -> Element<'a, Message> {
        let (key, title) = match group.group {
            BuiltinGroup::Harness if group.title.is_empty() => ("harness", "Harness tools".to_owned()),
            BuiltinGroup::Harness => ("harness", format!("Harness tools · {}", group.title)),
            BuiltinGroup::App => ("app", group.title.to_owned()),
        };
        let header = self.block_header(key, title, group.tools.len(), None);
        if !self.block_open(key) { return header }
        let rows = match self.builtin.harness_note.as_deref().filter(|_| group.group == BuiltinGroup::Harness) {
            Some(text) => vec![note_row(text)],
            None if group.tools.is_empty() => vec![note_row("No tools")],
            None => self.tool_rows(key, group.tools),
        };
        column![header, card(rows)].spacing(sp(Sp::Xs)).into()
    }

    /// The rows of list `key`, cut to a preview while long, with a show all / fewer row.
    fn tool_rows<'a>(&'a self, key: &str, tools: Vec<ToolRow<'a>>) -> Vec<Element<'a, Message>> {
        let (total, all_key) = (tools.len(), format!("{key}:all"));
        let all = !self.filter.is_empty() || total <= COLLAPSE_AT || self.open_blocks.get(&all_key).copied().unwrap_or(false);
        let mut rows: Vec<_> = tools.into_iter().take(if all { total } else { PREVIEW }).map(|t| self.tool_row(t)).collect();
        if total > COLLAPSE_AT && self.filter.is_empty() {
            let more = if all { "Show fewer".to_owned() } else { format!("Show all {total}") };
            rows.push(button(label(more, T::Caption, Tone::Muted)).padding(row_pad()).width(Fill).style(style::button(ButtonKind::Row { selected: false })).on_press(Message::BlockOpen(all_key, !all)).into());
        }
        rows
    }

    /// Checkbox, name, clipped description and detail dots; a click opens the details.
    fn tool_row<'a>(&'a self, ToolRow { id, info, enabled }: ToolRow<'a>) -> Element<'a, Message> {
        let expanded = self.expanded_tool.as_ref() == Some(&id);
        let toggle = self.project.is_some().then(|| { let id = id.clone(); move |on| Message::ToolToggled(id.clone(), on) });
        let dots = (!info.details.is_empty()).then(|| row(info.details.iter().map(|d| status_dot(detail_tone(d.state)))).spacing(sp(Sp::Xs)).align_y(Alignment::Center));
        let line = row![
            checkbox(enabled).size(ts(T::Body)).style(style::check).on_toggle_maybe(toggle),
            label(info.name.as_str(), T::Label, Tone::Normal).wrapping(text::Wrapping::None),
            clipped(label(info.description.as_str(), T::Caption, Tone::Muted).wrapping(text::Wrapping::None)),
            dots,
            tinted(if expanded { Icon::ChevronUp } else { Icon::ChevronDown }, T::Caption, Tone::Muted),
        ];
        let head = button(line.spacing(sp(Sp::S)).align_y(Alignment::Center)).padding(row_pad()).width(Fill).style(style::button(ButtonKind::Row { selected: false })).on_press(Message::ToolExpanded(id));
        column![head, expanded.then(|| tool_details(info))].into()
    }
}

/// An open tool: its whole description and its details.
fn tool_details(info: &ToolInfo) -> Element<'_, Message> {
    let details = info.details.iter().map(|d| {
        let name = label(d.label.as_str(), T::Caption, Tone::Normal).width(width(LayoutRole::LabelColumn) * 0.7);
        row![status_dot(detail_tone(d.state)), name, label(d.text.as_str(), T::Caption, Tone::Muted)].spacing(sp(Sp::S)).align_y(Alignment::Center).into()
    });
    let content = Column::new().push(label(info.description.as_str(), T::Caption, Tone::Muted)).extend(details).spacing(sp(Sp::Xs));
    container(content).padding(Padding::ZERO.right(sp(Sp::M)).bottom(sp(Sp::M)).left(sp(Sp::Xl) + sp(Sp::S))).width(Fill).into()
}

fn detail_tone(state: DetailState) -> Tone { [Tone::Ok, Tone::Muted, Tone::Danger][state as usize] }
fn row_pad() -> Padding { Padding::from([sp(Sp::S), sp(Sp::M)]) }

/// Rows in a card with a divider between each two.
fn card<'a>(rows: impl IntoIterator<Item = Element<'a, Message>>) -> Element<'a, Message> {
    let mut content = column![];
    for (i, item) in rows.into_iter().enumerate() {
        content = content.push((i > 0).then(|| container(rule::horizontal(1).style(style::rule)).padding(pad_x(Sp::Xs)))).push(item);
    }
    container(content).padding(sp(Sp::Xs)).width(Fill).style(style::container(Surface::Card)).into()
}

/// A form or an editor in a card.
fn form_card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> { container(content).padding(sp(Sp::M)).width(Fill).style(style::container(Surface::Card)).into() }
/// An editor or log opened under a row.
fn inset<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> { container(container(content).padding(sp(Sp::M)).width(Fill).style(style::container(Surface::Well))).padding(Padding::ZERO.bottom(sp(Sp::Xs))).into() }

/// Cards in two columns when `wide`, else stacked.
fn grid(items: Vec<Element<'_, Message>>, wide: bool) -> Element<'_, Message> {
    if !wide { return column(items).spacing(sp(Sp::L)).into() }
    let mut items = items.into_iter();
    let mut rows = column![].spacing(sp(Sp::L));
    while let Some(first) = items.next() {
        let second = items.next().unwrap_or_else(|| space().width(Fill).into());
        rows = rows.push(row![container(first).width(Fill), container(second).width(Fill)].spacing(sp(Sp::L)).align_y(Alignment::Start));
    }
    rows.into()
}

/// A muted line in a card, for empty states and progress.
fn note_row(s: &str) -> Element<'_, Message> { container(label(s, T::Label, Tone::Muted)).padding(row_pad()).width(Fill).into() }
/// Content taking the rest of a line, cut at its edge.
fn clipped<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> { container(content).width(Fill).clip(true).into() }

fn status_dot<'a>(tone: Tone) -> Element<'a, Message> {
    let d = sp(Sp::Xs) * 1.5;
    container(space().width(d).height(d)).style(style::fill(tone.role(), tokens().radius(RadiusRole::Dot))).into()
}

/// A small ghost button with caption text, or an icon and text.
fn flat<'a>(s: &'a str, icon: Option<Icon>) -> button::Button<'a, Message> {
    let content = row![icon.map(|i| tinted(i, T::Caption, Tone::Normal)), text(s).size(ts(T::Caption))].spacing(sp(Sp::Xs)).align_y(Alignment::Center);
    button(content).padding([sp(Sp::Xs), sp(Sp::S)]).style(style::button(ButtonKind::Ghost))
}

/// Submit and Cancel of a form.
fn form_buttons(submit: &str, form: FormId) -> Element<'_, Message> {
    let primary = button(text(submit).size(ts(T::Caption))).padding([sp(Sp::Xs), sp(Sp::M)]).style(style::button(ButtonKind::Primary)).on_press(Message::FormSubmit(form));
    row![primary, flat("Cancel", None).on_press(Message::FormCancel(form))].spacing(sp(Sp::S)).into()
}

fn input<'a>(placeholder: &'a str, value: &'a str, on: impl Fn(String) -> Message + 'a) -> text_input::TextInput<'a, Message> {
    text_input(placeholder, value).on_input(on).size(ts(T::Label)).style(style::input).padding([sp(Sp::Xs), sp(Sp::S)])
}

fn field_error(e: Option<&FieldError>) -> Option<text::Text<'static>> { e.map(|e| label(e.to_string(), T::Caption, Tone::Danger)) }
