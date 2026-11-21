//! The import and export screen: native sessions found on this machine, a path field for any
//! session file, and export of the active session as a native file or as debugging XML.

use loa_util::time::ago;
use std::path::{Path, PathBuf};

use iced::widget::{column, row, scrollable, text, text_input};
use iced::{Alignment, Element, Fill, Length};
use loa_harness::native::{self, NativeSession};
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, width};
use loa_ui_kit::widgets::{badge, card, label, section_header, text_button};

use super::State as Sessions;
use crate::area::{Area, Ctx, Request};
use loa_runtime::session::new_session_id;

/// Discovered sessions listed at once; older ones are picked by path.
const LISTED: usize = 200;

#[derive(Clone, Debug)]
pub enum Message {
    /// The import path field changed.
    Path(String),
    /// Import the file in the path field.
    ImportPath,
    /// Import this discovered file.
    Import(PathBuf),
    /// Look for native sessions again.
    Refresh,
    ExportPath(String),
    /// Export the active session: as XML, or in its harness's own format.
    Export { xml: bool },
    Close,
}

#[derive(Default)]
pub struct State {
    found: Vec<NativeSession>,
    path: String,
    /// Where the next export goes; the default path when empty.
    export_path: String,
    /// What the last import or export did.
    status: Option<String>,
}

impl Sessions {
    /// Shows the import screen over the native sessions found now.
    pub(super) fn show_import(&mut self, ctx: &mut Ctx) {
        self.transfer.found = native::discover(&self.home);
        self.transfer.status = None;
        ctx.show_screen(Self::ID);
    }

    pub(super) fn transfer(&mut self, ctx: &mut Ctx, message: Message) {
        match message {
            Message::Path(path) => self.transfer.path = path,
            Message::ExportPath(path) => self.transfer.export_path = path,
            Message::Refresh => self.transfer.found = native::discover(&self.home),
            Message::Close => ctx.close_screen(),
            Message::ImportPath => {
                let path = PathBuf::from(self.transfer.path.trim());
                self.import(ctx, &path);
            }
            Message::Import(path) => self.import(ctx, &path),
            Message::Export { xml } => {
                let path = Some(self.transfer.export_path.trim()).filter(|path| !path.is_empty()).map(PathBuf::from);
                self.export(ctx, xml, path);
            }
        }
    }

    /// Reads the native session file at `path` into a new session and opens it.
    fn import(&mut self, ctx: &mut Ctx, path: &Path) {
        let (services, runtime) = (&ctx.services, &ctx.runtime);
        let result = native::harness_of(path)
            .ok_or_else(|| format!("{} isn't a session file of a known harness", path.display()))
            .and_then(|harness| runtime.harnesses().registry.get(harness).filter(|adapter| adapter.capabilities().import).ok_or_else(|| format!("This app can't import {harness} sessions")))
            .and_then(|adapter| {
                let id = new_session_id();
                runtime.library().import(adapter.as_ref(), path, &id, &services.node.key).map(|_| id).map_err(|error| error.to_string())
            });
        match result {
            Ok(id) => {
                self.transfer.status = Some(format!("Imported {}", path.display()));
                ctx.request(Request::OpenSession(id));
            }
            Err(error) => {
                self.transfer.status = Some(format!("Can't import: {error}"));
                ctx.notify(format!("Can't import: {error}"));
            }
        }
    }

    /// Writes the active session to `path` (default: the exports folder) natively or as XML.
    pub(super) fn export(&mut self, ctx: &mut Ctx, xml: bool, path: Option<PathBuf>) {
        let Some(session) = ctx.runtime.sessions().active() else {
            ctx.notify("Open a session to export it");
            return;
        };
        let (id, harness) = (session.id().to_string(), session.harness_id().unwrap_or_default().to_owned());
        let extension = if xml { "xml" } else if harness == native::OPENCODE { "json" } else { "jsonl" };
        let (services, runtime) = (&ctx.services, &ctx.runtime);
        let path = path.unwrap_or_else(|| services.storage.dirs.data.join("exports").join(format!("{id}.{extension}")));
        let library = runtime.library();
        let result = path.parent().map_or(Ok(()), |dir| std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))).and_then(|()| {
            if xml {
                return library.export_xml(&id, &path).map_err(|error| error.to_string());
            }
            let adapter = runtime.harnesses().registry.get(&harness).filter(|adapter| adapter.capabilities().export);
            let adapter = adapter.ok_or_else(|| format!("{} sessions can't be exported", runtime.harnesses().label(&harness)))?;
            library.export(adapter.as_ref(), &id, &path).map_err(|error| error.to_string())
        });
        let status = match result {
            Ok(()) => format!("Exported to {}", path.display()),
            Err(error) => format!("Can't export: {error}"),
        };
        self.transfer.status = Some(status.clone());
        ctx.notify(status);
    }
}

impl State {
    /// `active` is the active session's id and harness, when there is one.
    pub fn view<'a>(&'a self, active: Option<(&'a str, &'a str)>, now: i64) -> Element<'a, Message> {
        let heading = |s| label(s, TextRole::Heading, Tone::Normal);
        let hint = |s: String| label(s, TextRole::Caption, Tone::Muted);
        let input = |placeholder, value, on: fn(String) -> Message| text_input(placeholder, value).on_input(on).padding(sp(SpaceRole::S)).style(style::input);
        let header = section_header("Import and export sessions", [text_button("Rescan", ButtonKind::Secondary, Some(Message::Refresh)), text_button("Done", ButtonKind::Secondary, Some(Message::Close))]);
        let import = text_button("Import", ButtonKind::Primary, (!self.path.trim().is_empty()).then_some(Message::ImportPath));
        let picker = column![
            heading("Import a session file"),
            hint("An omp, Claude Code or Codex JSONL session, or a file `opencode export <id>` wrote.".to_owned()),
            row![input("/path/to/session.jsonl", &self.path, Message::Path).on_submit(Message::ImportPath), import].spacing(sp(SpaceRole::S)).align_y(Alignment::Center),
        ];
        let found_title = match self.found.len() {
            0 => "No native sessions found in ~/.omp, ~/.claude or ~/.codex".to_owned(),
            n => format!("Sessions on this machine ({n})"),
        };
        let list = column(self.found.iter().take(LISTED).map(|session| found_row(session, now))).spacing(sp(SpaceRole::Xs));
        let more = self.found.len().saturating_sub(LISTED);
        let more = (more > 0).then(|| hint(format!("{more} older sessions are not listed; import them by path.")));
        let found = column![label(found_title, TextRole::Heading, Tone::Normal), scrollable(list).height(Length::Fixed(width(LayoutRole::ListWidth)))].push(more);
        let export: Element<'a, Message> = match active {
            Some((id, harness)) => {
                let buttons = [text_button(format!("Export {harness} file"), ButtonKind::Secondary, Some(Message::Export { xml: false })), text_button("Export XML (debug)", ButtonKind::Secondary, Some(Message::Export { xml: true }))];
                let fields = row![input("Export path", &self.export_path, Message::ExportPath)].extend(buttons).spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
                column![heading("Export this session"), hint(format!("{id} on {harness}. Leave the path empty to write into the app's exports folder.")), fields].spacing(sp(SpaceRole::S)).into()
            }
            None => label("Open a session to export it.", TextRole::Label, Tone::Muted).into(),
        };
        let cards = [picker.spacing(sp(SpaceRole::S)).into(), found.spacing(sp(SpaceRole::S)).into(), export].map(|body: Element<'a, Message>| card(body).width(Fill).into());
        let status = self.status.as_deref().map(|status| hint(status.to_owned()));
        let body = column![header].extend(cards).push(status).spacing(sp(SpaceRole::L)).max_width(width(LayoutRole::ContentWidth));
        scrollable(iced::widget::container(body).center_x(Fill).padding(sp(SpaceRole::Xl))).height(Fill).into()
    }
}

fn found_row(session: &NativeSession, now: i64) -> Element<'_, Message> {
    let name = session.title.clone().or_else(|| session.path.file_name().map(|name| name.to_string_lossy().into_owned())).unwrap_or_default();
    let detail = match &session.project {
        Some(project) => format!("{project} · {}", ago(now, session.modified)),
        None => ago(now, session.modified),
    };
    let harness = match session.harness {
        native::OMP => "omp",
        native::CLAUDE => "Claude Code",
        native::CODEX => "Codex",
        native::OPENCODE => "OpenCode",
        other => other,
    };
    let text = column![label(name, TextRole::Body, Tone::Normal).wrapping(text::Wrapping::None), label(detail, TextRole::Caption, Tone::Muted).wrapping(text::Wrapping::None)];
    row![badge(harness, Tone::Normal), text.width(Fill), text_button("Import", ButtonKind::Secondary, Some(Message::Import(session.path.clone())))]
        .spacing(sp(SpaceRole::S))
        .padding(sp(SpaceRole::Xs))
        .align_y(Alignment::Center)
        .into()
}
