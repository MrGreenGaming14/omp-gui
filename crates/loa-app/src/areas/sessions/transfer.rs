//! The import and export screen: native sessions found on this machine (omp,
//! Claude Code, Codex), a path field for any session file (OpenCode exports
//! included), and export of the active session as a native file or as
//! debugging XML.

use loa_util::time::ago;
use std::path::{Path, PathBuf};

use iced::widget::{button, column, container, row, scrollable, space, text, text_input};
use iced::{Alignment, Element, Fill};
use loa_features::native::{self, NativeSession};
use loa_ui_shell::theme::style;

/// Discovered sessions listed at once; older ones are picked by path.
const LISTED: usize = 200;
const WIDTH: f32 = 760.0;

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
    /// The export path field changed.
    ExportPath(String),
    /// Export the active session: `true` as XML, `false` in its harness's
    /// own format.
    Export { xml: bool },
    Close,
}

#[derive(Default)]
pub struct State {
    pub found: Vec<NativeSession>,
    pub path: String,
    /// Where the next export goes; the default path when empty.
    pub export_path: String,
    /// What the last import or export did.
    pub status: Option<String>,
}

impl State {
    pub fn refresh(&mut self, home: &Path) {
        self.found = native::discover(home);
    }

    /// `active` is the active session's id and harness, when there is one.
    pub fn view<'a>(&'a self, active: Option<(&'a str, &'a str)>, now: i64) -> Element<'a, Message> {
        let header = row![
            text("Import and export sessions").size(20).width(Fill),
            button(text("Rescan").size(13)).style(button::secondary).on_press(Message::Refresh),
            button(text("Done").size(13)).style(button::secondary).on_press(Message::Close),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let picker = column![
            text("Import a session file").size(15),
            text("An omp, Claude Code or Codex JSONL session, or a file `opencode export <id>` wrote.")
                .size(12)
                .style(style::muted),
            row![
                text_input("/path/to/session.jsonl", &self.path)
                    .on_input(Message::Path)
                    .on_submit(Message::ImportPath)
                    .size(14)
                    .style(style::input),
                button(text("Import").size(13))
                    .style(button::primary)
                    .on_press_maybe((!self.path.trim().is_empty()).then_some(Message::ImportPath)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(8);

        let mut list = column![].spacing(2);
        for session in self.found.iter().take(LISTED) {
            list = list.push(found_row(session, now));
        }
        let more = self.found.len().saturating_sub(LISTED);
        let found_title = match self.found.len() {
            0 => "No native sessions found in ~/.omp, ~/.claude or ~/.codex".to_owned(),
            n => format!("Sessions on this machine ({n})"),
        };
        let mut found = column![text(found_title).size(15), scrollable(list).height(360)].spacing(8);
        if more > 0 {
            found = found.push(text(format!("{more} older sessions are not listed; import them by path.")).size(12).style(style::muted));
        }

        let export: Element<'a, Message> = match active {
            Some((id, harness)) => column![
                text("Export this session").size(15),
                text(format!("{id} on {harness}. Leave the path empty to write into the app's exports folder."))
                    .size(12)
                    .style(style::muted),
                row![
                    text_input("Export path", &self.export_path).on_input(Message::ExportPath).size(14).style(style::input),
                    button(text(format!("Export {harness} file")).size(13))
                        .style(button::secondary)
                        .on_press(Message::Export { xml: false }),
                    button(text("Export XML (debug)").size(13)).style(button::secondary).on_press(Message::Export { xml: true }),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ]
            .spacing(8)
            .into(),
            None => text("Open a session to export it.").size(13).style(style::muted).into(),
        };

        let mut body = column![
            header,
            container(picker).style(style::card).padding(16).width(Fill),
            container(found).style(style::card).padding(16).width(Fill),
            container(export).style(style::card).padding(16).width(Fill),
        ]
        .spacing(16)
        .max_width(WIDTH);
        if let Some(status) = &self.status {
            body = body.push(text(status.as_str()).size(12).style(style::muted));
        }
        scrollable(container(column![space().height(24), body, space().height(24)]).center_x(Fill).padding([0, 24]))
            .height(Fill)
            .into()
    }
}

fn found_row(session: &NativeSession, now: i64) -> Element<'_, Message> {
    let name = session
        .title
        .clone()
        .or_else(|| session.path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let detail = match &session.project {
        Some(project) => format!("{project} · {}", ago(now, session.modified)),
        None => ago(now, session.modified),
    };
    let harness = container(text(harness_label(session.harness)).size(12)).style(style::badge).padding([1, 5]);
    row![
        harness,
        column![
            text(name).size(14).wrapping(text::Wrapping::None),
            text(detail).size(12).style(style::muted).wrapping(text::Wrapping::None),
        ]
        .width(Fill),
        button(text("Import").size(12)).style(button::secondary).on_press(Message::Import(session.path.clone())),
    ]
    .spacing(10)
    .padding([4, 4])
    .align_y(Alignment::Center)
    .into()
}

fn harness_label(harness: &str) -> &str {
    match harness {
        native::OMP => "omp",
        native::CLAUDE => "Claude Code",
        native::CODEX => "Codex",
        native::OPENCODE => "OpenCode",
        other => other,
    }
}

/// File extension of a native session of `harness`.
pub fn extension(harness: &str) -> &'static str {
    if harness == native::OPENCODE { "json" } else { "jsonl" }
}
