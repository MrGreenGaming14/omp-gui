//! The MCP servers section: the add form, the import list and a card per server.

use super::{BOLD, card, clipped, flat, form_buttons, form_card, grid, note_row, row_pad, status_dot};
use crate::tools::catalog::{Catalog, ServerView};
use crate::tools::{FormId, ImportOutcome, Message, ServerAction, ServerKind, ServerStatus, State, TransportKind, transport_label};
use iced::widget::{button, checkbox, column, container, row, space, text};
use iced::{Alignment, Element, Fill, Font, Padding};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, pad_y, sp, text_size as ts};
use loa_ui_kit::widgets::{form_view, label};

impl State {
    pub(super) fn servers_view<'a>(&'a self, catalog: Catalog<'a>, wide: bool) -> Element<'a, Message> {
        let actions = row![flat("Add server", Some(Icon::Plus)).on_press(Message::FormOpen(FormId::Server)), flat("Import", None).on_press(Message::ImportScan)];
        let header = row![label("Servers", T::Caption, Tone::Muted), label(catalog.servers.len().to_string(), T::Caption, Tone::Faint), space().width(Fill), actions.spacing(sp(Sp::Xs) / 2.0)];
        let empty = catalog.servers.is_empty().then(|| card([note_row(if self.filter.is_empty() { "No MCP servers" } else { "No server matches the filter" })]));
        let servers = grid(catalog.servers.into_iter().map(|s| self.server_view(s)).collect(), wide);
        column![header.spacing(sp(Sp::Xs)).align_y(Alignment::Center), self.add_server_form(), self.import_view(), empty, servers].spacing(sp(Sp::S)).into()
    }

    /// Name and status (which open and close the tools), transport and controls, then tools.
    fn server_view<'a>(&'a self, view: ServerView<'a>) -> Element<'a, Message> {
        let s = view.server;
        let (name, key) = (s.name.as_str(), format!("server:{}", s.name));
        let (open, filtering) = (self.block_open(&key), !self.filter.is_empty());
        let (tone, status) = match &s.status {
            ServerStatus::Running => (Tone::Ok, "running".to_owned()),
            ServerStatus::Starting => (Tone::Muted, "starting".to_owned()),
            ServerStatus::Stopped => (Tone::Muted, "stopped".to_owned()),
            ServerStatus::Error(e) => (Tone::Danger, format!("error: {e}")),
        };
        let kind = match s.kind {
            ServerKind::Surface => "surface",
            ServerKind::AppHosted => "app",
            ServerKind::External => "",
        };
        let chevron = (!filtering).then(|| tinted(if open { Icon::ChevronUp } else { Icon::ChevronDown }, T::Caption, Tone::Muted));
        let title = row![status_dot(tone), text(name).size(ts(T::Label)).font(BOLD).wrapping(text::Wrapping::None), label(kind, T::Caption, Tone::Faint), clipped(label(status, T::Caption, tone).wrapping(text::Wrapping::None).width(Fill).align_x(Alignment::End)), label(s.tools.len().to_string(), T::Caption, Tone::Faint), chevron];
        let title = button(title.spacing(sp(Sp::S)).align_y(Alignment::Center)).padding(row_pad()).width(Fill).style(style::button(ButtonKind::Row { selected: false })).on_press_maybe((!filtering).then(|| Message::BlockOpen(key.clone(), !open)));
        let running = matches!(s.status, ServerStatus::Running | ServerStatus::Starting);
        let act = |l: &'a str, a: ServerAction, on: bool| flat(l, None).on_press_maybe(on.then(|| Message::Server(name.to_owned(), a)));
        let toggle = button(text(format!("Tools: {}", view.toggle.label())).size(ts(T::Caption))).padding([sp(Sp::Xs), sp(Sp::S)]).style(style::button(ButtonKind::Ghost)).on_press_maybe(self.project.is_some().then(|| Message::ServerToggled(name.to_owned())));
        // App hosted servers run with the app; only their tools toggle.
        let controls = (s.kind != ServerKind::AppHosted).then(|| row![act("Start", ServerAction::Start, !running), act("Stop", ServerAction::Stop, running), act("Restart", ServerAction::Restart, running)]);
        let remove = s.can_remove().then(|| flat("Remove", None).style(style::button(ButtonKind::Danger)).on_press(Message::Server(name.to_owned(), ServerAction::Remove)));
        let transport = text(transport_label(&s.transport)).size(ts(T::Caption)).style(style::text(Tone::Faint)).font(Font::MONOSPACE).wrapping(text::Wrapping::WordOrGlyph);
        let below = column![transport, row![toggle, controls, remove].spacing(sp(Sp::Xs) / 2.0)].spacing(sp(Sp::Xs) / 2.0);
        let head = column![title, container(below).padding(Padding::ZERO.right(sp(Sp::Xs)).bottom(sp(Sp::Xs)).left(sp(Sp::M))).width(Fill)];
        let tools = match (open, view.tools.is_empty()) {
            (false, _) => vec![],
            (true, true) => vec![note_row("No tools declared")],
            (true, false) => self.tool_rows(&key, view.tools),
        };
        card(std::iter::once(head.into()).chain(tools))
    }

    fn add_server_form(&self) -> Option<Element<'_, Message>> {
        let f = self.add_server.as_ref()?;
        let kind = |label: &'static str, k: TransportKind| button(text(label).size(ts(T::Caption))).padding([sp(Sp::Xs), sp(Sp::S)]).style(style::button(ButtonKind::Tab { active: f.spec.kind == k })).on_press(Message::TransportPicked(k));
        let head = row![text("Add server").size(ts(T::Label)).font(BOLD), space().width(Fill), row![kind("stdio", TransportKind::Stdio), kind("HTTP", TransportKind::Http)].spacing(sp(Sp::Xs) / 2.0)].align_y(Alignment::Center);
        let fields = form_view(&f.spec, &f.errors, |k, v| Message::FormEdit(FormId::Server, k, v));
        Some(form_card(column![head, fields, form_buttons("Add", FormId::Server)].spacing(sp(Sp::S))))
    }

    fn import_view(&self) -> Option<Element<'_, Message>> {
        let entries = self.imports.as_ref()?;
        let header = row![text("Import from .claude, .cursor, .codex").size(ts(T::Label)).font(BOLD), space().width(Fill), flat("Close", Some(Icon::X)).on_press(Message::ImportClose)].align_y(Alignment::Center);
        let note = match (self.import_scanning, entries.is_empty()) {
            (true, _) => Some(note_row("Scanning...")),
            (false, true) => Some(note_row("No server configs found")),
            _ => None,
        };
        let rows = entries.iter().enumerate().map(|(i, e)| {
            let status = match &e.outcome {
                None if e.exists => Some(label("already registered".to_owned(), T::Caption, Tone::Warn)),
                None => None,
                Some(ImportOutcome::Imported) => Some(label("imported".to_owned(), T::Caption, Tone::Ok)),
                Some(ImportOutcome::Skipped(why)) => Some(label(format!("skipped: {why}"), T::Caption, Tone::Muted)),
                Some(ImportOutcome::Failed(why)) => Some(label(format!("failed: {why}"), T::Caption, Tone::Danger)),
            };
            let source = clipped(label(format!("{} {}", e.candidate.source.label(), e.candidate.path), T::Caption, Tone::Faint).wrapping(text::Wrapping::None));
            let line = row![checkbox(e.selected).size(ts(T::Body)).style(style::check).on_toggle(move |on| Message::ImportSelect(i, on)), label(e.candidate.server.name.as_str(), T::Label, Tone::Normal).wrapping(text::Wrapping::None), source, status];
            container(line.spacing(sp(Sp::S)).align_y(Alignment::Center)).padding(row_pad()).width(Fill).into()
        });
        let submit = button(text("Import selected").size(ts(T::Caption))).padding([sp(Sp::Xs), sp(Sp::M)]).style(style::button(ButtonKind::Primary)).on_press_maybe(entries.iter().any(|e| e.selected).then_some(Message::ImportSubmit));
        let mut items = vec![container(header).padding(pad_y(Sp::Xs).right(sp(Sp::Xs) / 2.0).left(sp(Sp::M))).into()];
        items.extend(note.into_iter().chain(rows));
        items.push(container(submit).padding(row_pad()).into());
        Some(card(items))
    }
}
