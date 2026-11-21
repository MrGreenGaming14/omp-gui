//! Row rendering through a [`RowRenderer`] registry keyed by [`ItemKind`], and the summaries
//! shown on collapsed rows ("Read 3 files 12s ›").

use super::items::{Item, ItemKind};
use super::prose::{self, LINE_HEIGHT, Look, Streamed};
use super::{LINE, Message, PADDING, ROW};
use iced::alignment::Vertical;
use iced::widget::{Space, button, column, container, row, rule, text};
use iced::{Element, Fill, Font, Padding};
use loa_core::{CallStatus, Dom, ElementKind, JobKind, JobStatus, MessageStatus, Node, NodeId, TodoState, attr};
use loa_ui_kit::icons;
use loa_ui_kit::style::{self as kit, ButtonKind, Surface, Tone};
use loa_ui_kit::text::{format_bytes, format_duration};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, pad_x, sp, text_size as ts};
use loa_view::theme::Icon;

/// Lines and bytes of a call input, result or job output shown when expanded.
const MAX_LINES: usize = 40;
const MAX_BYTES: usize = 6_000;
const CHARS_PER_LINE: usize = 90;

/// What a renderer sees of its row.
pub struct RowCtx<'a> {
    pub dom: &'a Dom,
    pub node: &'a Node,
    pub expanded: bool,
    pub streamed: Option<&'a Streamed>,
    pub look: Look,
}

/// Draws and sizes one kind of row.
pub trait RowRenderer: Sync {
    fn view<'a>(&self, item: &'a Item, cx: &RowCtx<'a>) -> Element<'a, Message>;
    /// A constant-time height guess before layout, without padding.
    fn estimate(&self, item: &Item, cx: &RowCtx<'_>) -> f32 { if cx.expanded { ROW + item.calls.len().max(1) as f32 * 12.0 * LINE } else { ROW } }
}

/// The renderer of `kind`.
pub fn renderer(kind: ItemKind) -> &'static dyn RowRenderer {
    static ROWS: [&dyn RowRenderer; 11] = [&User, &Assistant, &THINKING, &System, &Calls, &Slot, &Job, &Todo, &SurfaceRow, &COMPACTION, &Ext];
    ROWS[kind as usize]
}

fn text_lines(node: &Node) -> f32 { (node.text.len() / CHARS_PER_LINE + 1) as f32 }

/// Past-tense verb and nouns per tool, Claude Code style: "Read 3 files".
const VERBS: &[(&[&str], &str, &str, &str)] = &[
    (&["read", "read_file", "view"], "Read", "file", "files"),
    (&["edit", "multiedit", "apply_patch", "ast_edit"], "Edited", "file", "files"),
    (&["write", "write_file"], "Wrote", "file", "files"),
    (&["bash", "shell", "exec", "exec_command"], "Ran", "command", "commands"),
    (&["grep", "search"], "Searched", "pattern", "patterns"),
    (&["glob", "find", "ls"], "Listed", "pattern", "patterns"),
    (&["fetch", "webfetch", "web_fetch"], "Fetched", "page", "pages"),
    (&["websearch", "web_search"], "Searched the web for", "query", "queries"),
    (&["task", "agent"], "Ran", "agent", "agents"),
];

fn tool_words(tool: &str) -> (&str, &'static str, &'static str) {
    VERBS.iter().find(|(names, ..)| names.iter().any(|n| tool.eq_ignore_ascii_case(n))).map_or((tool, "call", "calls"), |&(_, v, one, many)| (v, one, many))
}

/// How a call group or job is doing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Status { Running, Done, Error, Cancelled }

impl Status {
    fn word(self) -> &'static str { ["running…", "done", "failed", "cancelled"][self as usize] }
}

/// What a collapsed call row shows; the duration runs from the first start to the last end.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CallSummary {
    pub label: String,
    pub status: Status,
    pub duration_ms: Option<i64>,
    pub failed: usize,
}

fn is_failed(dom: &Dom, call: &Node) -> bool {
    call.str_attr(attr::STATUS).and_then(CallStatus::parse) == Some(CallStatus::Error) || dom.children_of_kind(call.id, ElementKind::Result).any(|r| r.bool_attr(attr::IS_ERROR) == Some(true))
}

pub(crate) fn summarize(dom: &Dom, calls: &[NodeId]) -> CallSummary {
    let nodes: Vec<&Node> = calls.iter().filter_map(|id| dom.get(*id)).collect();
    let tool = nodes.first().and_then(|n| n.str_attr(attr::TOOL)).unwrap_or_default();
    let (verb, one, many) = tool_words(tool);
    let label = match nodes.first().and_then(|n| n.str_attr(attr::TITLE)) {
        Some(title) if nodes.len() == 1 => format!("{verb} {title}"),
        _ if nodes.len() == 1 && verb == tool => tool.to_owned(),
        _ => format!("{verb} {} {}", nodes.len(), if nodes.len() == 1 { one } else { many }),
    };
    let (mut running, mut cancelled, mut failed, mut span, mut timed) = (false, 0, 0, (i64::MAX, i64::MIN), true);
    for call in &nodes {
        match call.str_attr(attr::STATUS).and_then(CallStatus::parse) {
            Some(CallStatus::Pending | CallStatus::Running) | None => running = true,
            Some(CallStatus::Cancelled) => cancelled += 1,
            Some(CallStatus::Done | CallStatus::Error) => {}
        }
        failed += usize::from(is_failed(dom, call));
        match (call.int_attr(attr::STARTED), call.int_attr(attr::ENDED)) {
            (Some(s), Some(e)) => span = (span.0.min(s), span.1.max(e)),
            _ => timed = false,
        }
    }
    let status = match () {
        _ if running => Status::Running,
        _ if failed > 0 => Status::Error,
        _ if cancelled > 0 && cancelled == nodes.len() => Status::Cancelled,
        _ => Status::Done,
    };
    let duration_ms = (timed && !running && !nodes.is_empty()).then(|| (span.1 - span.0).max(0));
    CallSummary { label, status, duration_ms, failed }
}

/// The start of `text`, at most [`MAX_LINES`] lines and [`MAX_BYTES`] bytes, and the bytes left out.
pub(crate) fn leading(text: &str) -> (&str, usize) {
    let mut end = text.floor_char_boundary(MAX_BYTES);
    if let Some((i, _)) = text[..end].match_indices('\n').nth(MAX_LINES - 1) {
        end = i;
    }
    (&text[..end], text.len() - end)
}

/// The end of `text`, bounded like [`leading`], and the bytes left out.
pub(crate) fn trailing(text: &str) -> (&str, usize) {
    let mut start = text.ceil_char_boundary(text.len().saturating_sub(MAX_BYTES));
    if let Some((i, _)) = text[start..].trim_end_matches('\n').rmatch_indices('\n').nth(MAX_LINES - 1) {
        start += i + 1;
    }
    (&text[start..], start)
}

fn muted<'a>(content: impl text::IntoFragment<'a>) -> text::Text<'a> { text(content).size(ts(T::Body)).style(kit::text(Tone::Muted)) }
fn danger<'a>(content: impl text::IntoFragment<'a>) -> Element<'a, Message> { text(content).size(ts(T::Body)).style(text::danger).into() }
fn glyph<'a>(icon: Icon, failed: bool) -> Element<'a, Message> { icons::tinted(icon, T::Body, if failed { Tone::Danger } else { Tone::Muted }).into() }

/// A header that toggles the row: `parts` in a line, then a chevron. Expanded, `body` follows
/// behind a thin guide.
fn foldable<'a>(parts: Vec<Element<'a, Message>>, key: NodeId, body: Option<Element<'a, Message>>) -> Element<'a, Message> {
    let chevron = icons::muted(if body.is_some() { Icon::ChevronDown } else { Icon::ChevronRight }, ts(T::Caption));
    let line = row(parts).push(chevron).spacing(sp(Sp::S)).align_y(Vertical::Center);
    let head = button(line).style(kit::button(ButtonKind::Row { selected: false })).padding([sp(Sp::Xs), sp(Sp::M)]).on_press(Message::Toggle(key));
    let Some(body) = body else { return head.into() };
    let body = row![rule::vertical(1).style(kit::rule), body].spacing(sp(Sp::L)).height(iced::Shrink);
    column![head, container(body).padding(Padding::ZERO.left(sp(Sp::L) + 1.0).bottom(sp(Sp::Xs)))].spacing(sp(Sp::Xs)).into()
}

/// A monospace well of `body` with the bytes left out noted below.
fn block(body: &str, left_out: usize, error: bool) -> Element<'_, Message> {
    let body = text(body).font(Font::MONOSPACE).size(ts(T::Mono));
    let mut col = column![if error { body.style(text::danger) } else { body }].spacing(sp(Sp::Xs));
    if left_out > 0 {
        col = col.push(muted(format!("… {} more", format_bytes(left_out as u64))).size(ts(T::Label)));
    }
    container(col).padding([sp(Sp::S), sp(Sp::M)]).width(Fill).style(kit::container(Surface::Well)).into()
}

fn inline<'a>(parts: impl IntoIterator<Item = Element<'a, Message>>) -> Element<'a, Message> { row(parts).spacing(sp(Sp::S)).padding(pad_x(Sp::M)).align_y(Vertical::Center).into() }

struct User;
impl RowRenderer for User {
    fn view<'a>(&self, _: &'a Item, cx: &RowCtx<'a>) -> Element<'a, Message> {
        let body = text(cx.node.text.as_str()).size(ts(T::Body)).line_height(LINE_HEIGHT);
        let bubble = button(body).style(kit::button(ButtonKind::Row { selected: true })).padding([sp(Sp::S), sp(Sp::M)]).on_press(Message::Rewind(cx.node.id));
        container(container(bubble).max_width(ts(T::Body) * 40.0)).align_right(Fill).into()
    }
    fn estimate(&self, _: &Item, cx: &RowCtx<'_>) -> f32 { LINE * text_lines(cx.node) + PADDING }
}

struct System;
impl RowRenderer for System {
    fn view<'a>(&self, _: &'a Item, cx: &RowCtx<'a>) -> Element<'a, Message> { muted(cx.node.text.as_str()).size(ts(T::Label)).into() }
    fn estimate(&self, _: &Item, cx: &RowCtx<'_>) -> f32 { LINE * text_lines(cx.node) + PADDING }
}

struct Assistant;
impl RowRenderer for Assistant {
    fn view<'a>(&self, _: &'a Item, cx: &RowCtx<'a>) -> Element<'a, Message> {
        let status = cx.node.str_attr(attr::STATUS).and_then(MessageStatus::parse);
        let marker = (status == Some(MessageStatus::Cancelled)).then_some("[interrupted]");
        let (body, placed) = match cx.streamed {
            Some(streamed) => prose::view(streamed.items(), cx.look, marker),
            None => (text(cx.node.text.as_str()).size(ts(T::Body)).line_height(LINE_HEIGHT).into(), false),
        };
        let col = column![body].spacing(sp(Sp::Xs)).push(marker.filter(|_| !placed).map(muted));
        col.push((status == Some(MessageStatus::Error)).then(|| text("Failed").size(ts(T::Label)).style(text::danger))).into()
    }
    fn estimate(&self, _: &Item, cx: &RowCtx<'_>) -> f32 {
        cx.streamed.map_or(LINE * text_lines(cx.node), |s| LINE * (s.lines + s.fed / CHARS_PER_LINE + 1) as f32)
    }
}

/// A collapsed note whose text shows when expanded: thinking and compactions.
struct Note {
    label: fn(&Node) -> String,
    italic: bool,
}

const THINKING: Note = Note {
    label: |n| (if n.str_attr(attr::STATUS).and_then(MessageStatus::parse) == Some(MessageStatus::Streaming) { "Thinking…" } else { "Thought" }).into(),
    italic: true,
};
const COMPACTION: Note = Note {
    label: |n| match n.attr(attr::COVERS).and_then(|v| v.as_list()).map_or(0, <[_]>::len) {
        0 => "Conversation compacted".into(),
        1 => "Compacted 1 turn".into(),
        n => format!("Compacted {n} turns"),
    },
    italic: false,
};

impl RowRenderer for Note {
    fn view<'a>(&self, _: &'a Item, cx: &RowCtx<'a>) -> Element<'a, Message> {
        let font = Font { style: if self.italic { iced::font::Style::Italic } else { iced::font::Style::Normal }, ..Font::DEFAULT };
        let body = cx.expanded.then(|| muted(cx.node.text.as_str()).size(ts(T::Label)).line_height(LINE_HEIGHT).font(font).into());
        foldable(vec![muted((self.label)(cx.node)).font(font).into()], cx.node.id, body)
    }
    fn estimate(&self, _: &Item, cx: &RowCtx<'_>) -> f32 { if cx.expanded { ROW + LINE * text_lines(cx.node) } else { ROW } }
}

struct Calls;
impl RowRenderer for Calls {
    fn view<'a>(&self, item: &'a Item, cx: &RowCtx<'a>) -> Element<'a, Message> {
        let s = summarize(cx.dom, &item.calls);
        let mut parts = vec![glyph(Icon::FileText, s.status == Status::Error), muted(s.label).into()];
        match (s.status, s.duration_ms) {
            (Status::Running, _) => parts.push(muted(Status::Running.word()).into()),
            (_, Some(ms)) => parts.push(muted(format_duration(ms)).into()),
            _ => {}
        }
        if s.failed > 0 {
            parts.push(danger(format!("{} failed", s.failed)));
        }
        if s.status == Status::Cancelled {
            parts.push(muted("cancelled").into());
        }
        let body = cx.expanded.then(|| column(item.calls.iter().filter_map(|id| cx.dom.get(*id)).map(|c| call_details(cx.dom, c))).spacing(sp(Sp::S)).into());
        foldable(parts, item.key, body)
    }
}

fn call_details<'a>(dom: &'a Dom, call: &'a Node) -> Element<'a, Message> {
    let tool = call.str_attr(attr::TOOL).unwrap_or_default();
    let line = call.str_attr(attr::TITLE).map_or_else(|| tool.to_owned(), |title| format!("{} {title}", tool_words(tool).0));
    let mut col = column![text(line).size(ts(T::Label))].spacing(sp(Sp::Xs));
    for part in dom.children(call.id).filter(|n| matches!(n.kind, ElementKind::Input | ElementKind::Result)) {
        let error = part.bool_attr(attr::IS_ERROR) == Some(true);
        let label = match part.kind {
            ElementKind::Input => muted("Input"),
            _ if error => text("Error").style(text::danger),
            _ => muted("Result"),
        };
        let (shown, left_out) = leading(&part.text);
        col = col.push(label.size(ts(T::Caption))).push(block(shown, left_out, error));
    }
    let silent = call.str_attr(attr::STATUS).and_then(CallStatus::parse) == Some(CallStatus::Error) && dom.children_of_kind(call.id, ElementKind::Result).next().is_none();
    col.push(silent.then(|| text("Failed").size(ts(T::Label)).style(text::danger))).into()
}

struct Job;
impl RowRenderer for Job {
    fn view<'a>(&self, _: &'a Item, cx: &RowCtx<'a>) -> Element<'a, Message> {
        let node = cx.node;
        let (noun, icon) = match node.str_attr(attr::KIND).and_then(JobKind::parse) {
            Some(JobKind::Subagent) => ("Agent", Icon::Cpu),
            _ => ("Shell", Icon::Terminal),
        };
        let name = [attr::TITLE, attr::COMMAND, attr::AGENT].into_iter().find_map(|a| node.str_attr(a)).unwrap_or_default();
        let status = match node.str_attr(attr::STATUS).and_then(JobStatus::parse) {
            Some(JobStatus::Running) | None => Status::Running,
            Some(JobStatus::Done) => Status::Done,
            Some(JobStatus::Error) => Status::Error,
            Some(JobStatus::Cancelled) => Status::Cancelled,
        };
        let failed = status == Status::Error;
        let mut parts = vec![glyph(icon, failed), muted(noun).into(), text(name).font(Font::MONOSPACE).size(ts(T::Label)).into(), muted(status.word()).into()];
        if let Some(code) = node.int_attr(attr::EXIT_CODE).filter(|c| *c != 0) {
            parts.push(danger(format!("exit {code}")));
        }
        let body = (cx.expanded && !node.text.is_empty()).then(|| {
            let (shown, left_out) = trailing(&node.text);
            let note = (left_out > 0).then(|| muted(format!("… {} more", format_bytes(left_out as u64))).size(ts(T::Label)));
            column![].push(note).push(block(shown, 0, failed)).spacing(sp(Sp::Xs)).into()
        });
        foldable(parts, node.id, body)
    }
}

struct Todo;
impl RowRenderer for Todo {
    fn view<'a>(&self, _: &'a Item, cx: &RowCtx<'a>) -> Element<'a, Message> {
        let (mark, done) = match cx.node.str_attr(attr::STATE).and_then(TodoState::parse).unwrap_or(TodoState::Pending) {
            TodoState::Pending => ("☐", false),
            TodoState::InProgress => ("◐", false),
            TodoState::Done => ("☑", true),
            TodoState::Cancelled => ("☒", true),
        };
        let tone = kit::text(if done { Tone::Muted } else { Tone::Normal });
        inline([text(mark).size(ts(T::Body)).style(tone).into(), text(cx.node.text.as_str()).size(ts(T::Body)).style(kit::text(if done { Tone::Muted } else { Tone::Normal })).into()])
    }
    fn estimate(&self, _: &Item, _: &RowCtx<'_>) -> f32 { LINE }
}

struct SurfaceRow;
impl RowRenderer for SurfaceRow {
    fn view<'a>(&self, _: &'a Item, cx: &RowCtx<'a>) -> Element<'a, Message> {
        let name = cx.node.str_attr(attr::NAME).or_else(|| cx.node.str_attr(attr::URI)).unwrap_or_default();
        inline([muted("Surface").into(), text(name).font(Font::MONOSPACE).size(ts(T::Label)).into(), muted(cx.node.str_attr(attr::MODE).unwrap_or_default()).into()])
    }
    fn estimate(&self, _: &Item, _: &RowCtx<'_>) -> f32 { LINE }
}

struct Ext;
impl RowRenderer for Ext {
    fn view<'a>(&self, _: &'a Item, cx: &RowCtx<'a>) -> Element<'a, Message> {
        let node = cx.node;
        let parts = vec![glyph(Icon::Wrench, false), muted(node.str_attr(attr::HARNESS).unwrap_or_default()).into(), muted(node.str_attr(attr::TYPE).unwrap_or("event")).into()];
        let raw = if node.text.is_empty() { node.str_attr(attr::RAW).unwrap_or_default() } else { node.text.as_str() };
        foldable(parts, node.id, cx.expanded.then(|| {
            let (shown, left_out) = leading(raw);
            block(shown, left_out, false)
        }))
    }
}

/// A decision: the app fills the slot with its card, so this is never drawn.
struct Slot;
impl RowRenderer for Slot {
    fn view<'a>(&self, _: &'a Item, _: &RowCtx<'a>) -> Element<'a, Message> { Space::new().into() }
    fn estimate(&self, _: &Item, _: &RowCtx<'_>) -> f32 { 6.0 * LINE }
}
