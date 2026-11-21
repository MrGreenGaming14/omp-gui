//! The MCP servers section: the add form, the import list and a card per server.

use super::{BOLD, card, clipped, form_card, grid, note_row, primary, row_button, row_pad, status_dot};
use crate::{action, ghost};
use crate::tools::catalog::{Catalog, ServerView};
use crate::tools::{FormId, ImportOutcome, Message, ServerAction, ServerKind, ServerStatus, State, TransportKind};
use iced::widget::{button, checkbox, column, container, row, space, text};
use iced::{Alignment, Element, Fill, Font, Padding};
use loa_contracts::harness::McpTransport;
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, pad_y, sp, text_size as ts};
use loa_ui_kit::widgets::{form_view, label};

impl State {
    pub(super) fn servers_view<'a>(&'a self, catalog: Catalog<'a>, wide: bool) -> Element<'a, Message> {
        let actions = row![ghost("Add server", Some(Icon::Plus)).on_press(Message::FormOpen(FormId::Server)), action("Import", Message::ImportScan)];
        let header = row![label("Servers", T::Caption, Tone::Muted), label(catalog.servers.len().to_string(), T::Caption, Tone::Faint), space().width(Fill), actions.spacing(sp(Sp::Xs) / 2.0)];
        let empty = catalog.servers.is_empty().then(|| card([note_row(if self.filter.is_empty() { "No MCP servers" } else { "No server matches the filter" })]));
        let add = self.add_server.as_ref().map(|f| {
            let kind = |label: &'static str, k: TransportKind| button(text(label).size(ts(T::Caption))).padding([sp(Sp::Xs), sp(Sp::S)]).style(style::button(ButtonKind::Tab { active: f.spec.kind == k })).on_press(Message::TransportPicked(k));
            let kinds = row![kind("stdio", TransportKind::Stdio), kind("HTTP", TransportKind::Http)].spacing(sp(Sp::Xs) / 2.0);
            form_card("Add server", Some(kinds.into()), form_view(&f.spec, &f.errors, |k, v| Message::FormEdit(FormId::Server, k, v), None), "Add", FormId::Server)
        });
        let servers = grid(catalog.servers.into_iter().map(|s| self.server_view(s)).collect(), wide);
        column![header.spacing(sp(Sp::Xs)).align_y(Alignment::Center), add, self.import_view(), empty, servers].spacing(sp(Sp::S)).into()
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
        let kind = ["", "surface", "app"][s.kind as usize];
        let chevron = (!filtering).then(|| tinted(if open { Icon::ChevronUp } else { Icon::ChevronDown }, T::Caption, Tone::Muted));
        let title = row![status_dot(tone), text(name).size(ts(T::Label)).font(BOLD).wrapping(text::Wrapping::None), label(kind, T::Caption, Tone::Faint), clipped(label(status, T::Caption, tone).wrapping(text::Wrapping::None).width(Fill).align_x(Alignment::End)), label(s.tools.len().to_string(), T::Caption, Tone::Faint), chevron];
        let title = row_button(title.spacing(sp(Sp::S)).align_y(Alignment::Center).into(), (!filtering).then(|| Message::BlockOpen(key.clone(), !open)));
        let running = matches!(s.status, ServerStatus::Running | ServerStatus::Starting);
        let act = |l: &'a str, a: ServerAction, on: bool| ghost(l, None).on_press_maybe(on.then(|| Message::Server(name.to_owned(), a)));
        let toggle = ghost(format!("Tools: {}", view.toggle.label()), None).on_press_maybe(self.project.is_some().then(|| Message::ServerToggled(name.to_owned())));
        // App hosted servers run with the app; only their tools toggle.
        let hosted = s.kind == ServerKind::AppHosted;
        let controls = (!hosted).then(|| row![act("Start", ServerAction::Start, !running), act("Stop", ServerAction::Stop, running), act("Restart", ServerAction::Restart, running), act("Remove", ServerAction::Remove, true).style(style::button(ButtonKind::Danger))]);
        let transport = match &s.transport {
            McpTransport::Stdio { command, args, .. } => std::iter::once(command).chain(args).cloned().collect::<Vec<_>>().join(" "),
            McpTransport::Http { url, .. } => url.clone(),
        };
        let transport = text(transport).size(ts(T::Caption)).style(style::text(Tone::Faint)).font(Font::MONOSPACE).wrapping(text::Wrapping::WordOrGlyph);
        let below = column![transport, row![toggle, controls].spacing(sp(Sp::Xs) / 2.0)].spacing(sp(Sp::Xs) / 2.0);
        let head = column![title, container(below).padding(Padding::ZERO.right(sp(Sp::Xs)).bottom(sp(Sp::Xs)).left(sp(Sp::M))).width(Fill)];
        let tools = match (open, view.tools.is_empty()) {
            (false, _) => vec![],
            (true, true) => vec![note_row("No tools declared")],
            (true, false) => self.tool_rows(&key, view.tools),
        };
        card(std::iter::once(head.into()).chain(tools))
    }
    fn import_view(&self) -> Option<Element<'_, Message>> {
        let entries = self.imports.as_ref()?;
        let header = row![text("Import from .claude, .cursor, .codex").size(ts(T::Label)).font(BOLD), space().width(Fill), ghost("Close", Some(Icon::X)).on_press(Message::ImportClose)].align_y(Alignment::Center);
        let note = match (self.import_scanning, entries.is_empty()) {
            (true, _) => Some(note_row("Scanning...")),
            (false, true) => Some(note_row("No server configs found")),
            _ => None,
        };
        let rows = entries.iter().enumerate().map(|(i, e)| {
            let status = match &e.outcome {
                None => e.exists.then(|| label("already registered".to_owned(), T::Caption, Tone::Warn)),
                Some(ImportOutcome::Imported) => Some(label("imported".to_owned(), T::Caption, Tone::Ok)),
                Some(ImportOutcome::Skipped(why)) => Some(label(format!("skipped: {why}"), T::Caption, Tone::Muted)),
                Some(ImportOutcome::Failed(why)) => Some(label(format!("failed: {why}"), T::Caption, Tone::Danger)),
            };
            let source = clipped(label(format!("{} {}", e.candidate.source.label(), e.candidate.path), T::Caption, Tone::Faint).wrapping(text::Wrapping::None));
            let line = row![checkbox(e.selected).size(ts(T::Body)).style(style::check).on_toggle(move |on| Message::ImportSelect(i, on)), label(e.candidate.server.name.as_str(), T::Label, Tone::Normal).wrapping(text::Wrapping::None), source, status];
            container(line.spacing(sp(Sp::S)).align_y(Alignment::Center)).padding(row_pad()).width(Fill).into()
        });
        let submit = primary("Import selected", entries.iter().any(|e| e.selected).then_some(Message::ImportSubmit));
        let head = container(header).padding(pad_y(Sp::Xs).right(sp(Sp::Xs) / 2.0).left(sp(Sp::M))).into();
        Some(card(std::iter::once(head).chain(note).chain(rows).chain([container(submit).padding(row_pad()).into()])))
    }
}
