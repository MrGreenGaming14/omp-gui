//! Rendering of one transcript row, and the summaries shown on collapsed
//! rows ("Read 3 files 12s ›").

use crate::Message;
use crate::items::{Item, ItemKind};
use crate::stream::Streamed;
use iced::widget::{Space, button, column, container, markdown, row, text};
use iced::{Element, Fill, Font};
use loa_core::{
    CallStatus, Dom, ElementKind, JobKind, JobStatus, MessageStatus, Node, NodeId, TodoState, attr,
};

/// Lines of a call input, call result or job output shown when expanded.
const MAX_LINES: usize = 40;
/// Bytes shown of the same, so one huge line can't stall layout.
const MAX_BYTES: usize = 6_000;
const DETAIL_SIZE: u32 = 13;

/// How a group of calls is doing as a whole.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GroupStatus {
    Running,
    Done,
    Error,
    Cancelled,
}

/// What a collapsed call row shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CallSummary {
    pub label: String,
    pub status: GroupStatus,
    /// From the first start to the last end, once every call has ended.
    pub duration_ms: Option<i64>,
    /// Calls that ended in an error.
    pub failed: usize,
}

/// Verb and nouns of a tool, Claude Code style: "Read 3 files".
fn tool_words(tool: &str) -> (&str, &'static str, &'static str) {
    let lower = |name: &str| tool.eq_ignore_ascii_case(name);
    if lower("read") || lower("read_file") || lower("view") {
        ("Read", "file", "files")
    } else if lower("edit") || lower("multiedit") || lower("apply_patch") || lower("ast_edit") {
        ("Edit", "file", "files")
    } else if lower("write") || lower("write_file") {
        ("Write", "file", "files")
    } else if lower("bash") || lower("shell") || lower("exec") || lower("exec_command") {
        ("Ran", "command", "commands")
    } else if lower("grep") || lower("search") {
        ("Searched", "pattern", "patterns")
    } else if lower("glob") || lower("find") || lower("ls") {
        ("Listed", "pattern", "patterns")
    } else if lower("fetch") || lower("webfetch") || lower("web_fetch") {
        ("Fetched", "page", "pages")
    } else if lower("websearch") || lower("web_search") {
        ("Searched the web for", "query", "queries")
    } else if lower("task") || lower("agent") {
        ("Ran", "agent", "agents")
    } else {
        (tool, "call", "calls")
    }
}

fn is_failed(dom: &Dom, call: &Node) -> bool {
    call.str_attr(attr::STATUS).and_then(CallStatus::parse) == Some(CallStatus::Error)
        || dom
            .children_of_kind(call.id, ElementKind::Result)
            .any(|r| r.bool_attr(attr::IS_ERROR) == Some(true))
}

pub(crate) fn summarize(dom: &Dom, calls: &[NodeId]) -> CallSummary {
    let nodes = || calls.iter().filter_map(|id| dom.get(*id));
    let count = nodes().count();
    let tool = nodes()
        .next()
        .and_then(|n| n.str_attr(attr::TOOL))
        .unwrap_or_default();
    let (verb, one, many) = tool_words(tool);
    let label = match nodes().next().and_then(|n| n.str_attr(attr::TITLE)) {
        Some(title) if count == 1 => format!("{verb} {title}"),
        _ if count == 1 && verb == tool => tool.to_owned(),
        _ => format!("{verb} {count} {}", if count == 1 { one } else { many }),
    };

    let mut running = false;
    let mut cancelled = 0;
    let mut failed = 0;
    let mut start = i64::MAX;
    let mut end = i64::MIN;
    let mut timed = true;
    for call in nodes() {
        match call.str_attr(attr::STATUS).and_then(CallStatus::parse) {
            Some(CallStatus::Pending | CallStatus::Running) | None => running = true,
            Some(CallStatus::Cancelled) => cancelled += 1,
            Some(CallStatus::Done | CallStatus::Error) => {}
        }
        if is_failed(dom, call) {
            failed += 1;
        }
        match (call.int_attr(attr::STARTED), call.int_attr(attr::ENDED)) {
            (Some(s), Some(e)) => {
                start = start.min(s);
                end = end.max(e);
            }
            _ => timed = false,
        }
    }
    let status = if running {
        GroupStatus::Running
    } else if failed > 0 {
        GroupStatus::Error
    } else if cancelled > 0 && cancelled == count {
        GroupStatus::Cancelled
    } else {
        GroupStatus::Done
    };
    let duration_ms = (timed && !running && count > 0).then(|| (end - start).max(0));
    CallSummary {
        label,
        status,
        duration_ms,
        failed,
    }
}

/// "0.4s", "12s", "2m 5s".
pub(crate) fn format_duration(ms: i64) -> String {
    if ms < 10_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 60_000 {
        format!("{}s", ms / 1000)
    } else {
        format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// The start of `text`: at most [`MAX_LINES`] lines and [`MAX_BYTES`]
/// bytes, and the number of bytes left out.
pub(crate) fn leading(text: &str) -> (&str, usize) {
    let mut end = text.len().min(MAX_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if let Some((i, _)) = text[..end].match_indices('\n').nth(MAX_LINES - 1) {
        end = i;
    }
    (&text[..end], text.len() - end)
}

/// The end of `text`, bounded like [`leading`], and the number of bytes left out.
pub(crate) fn trailing(text: &str) -> (&str, usize) {
    let mut start = text.len().saturating_sub(MAX_BYTES);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let trimmed = text[start..].trim_end_matches('\n');
    if let Some((i, _)) = trimmed.rmatch_indices('\n').nth(MAX_LINES - 1) {
        start += i + 1;
    }
    (&text[start..], start)
}

fn chevron(expanded: bool) -> &'static str {
    if expanded { "⌄" } else { "›" }
}

/// A row header that toggles the row open and closed.
fn header<'a>(content: impl Into<Element<'a, Message>>, key: NodeId) -> Element<'a, Message> {
    button(content)
        .style(button::text)
        .padding([2, 0])
        .on_press(Message::Toggle(key))
        .into()
}

fn muted<'a>(content: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(content).style(text::secondary)
}

fn omitted<'a>(bytes: usize) -> Element<'a, Message> {
    muted(format!("… {} more", format_bytes(bytes)))
        .size(DETAIL_SIZE)
        .into()
}

fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

/// A monospace block of `body` with the bytes left out noted below.
fn block<'a>(body: &'a str, left_out: usize, error: bool) -> Element<'a, Message> {
    let body = text(body).font(Font::MONOSPACE).size(DETAIL_SIZE);
    let body = if error {
        body.style(text::danger)
    } else {
        body
    };
    let mut col = column![body].spacing(4);
    if left_out > 0 {
        col = col.push(omitted(left_out));
    }
    container(col)
        .padding(8)
        .width(Fill)
        .style(container::rounded_box)
        .into()
}

/// Renders one row.
pub(crate) fn view<'a>(
    dom: &'a Dom,
    item: &'a Item,
    expanded: bool,
    streamed: Option<&'a Streamed>,
    settings: markdown::Settings,
) -> Element<'a, Message> {
    let Some(node) = dom.get(item.key) else {
        return Space::new().into();
    };
    match item.kind {
        ItemKind::User => user(node),
        ItemKind::Assistant => assistant(node, streamed, settings),
        ItemKind::Thinking => thinking(node, expanded),
        ItemKind::System => muted(node.text.as_str()).into(),
        ItemKind::Calls => calls(dom, item, expanded),
        // The app fills decision slots; this row is never asked for them.
        ItemKind::Decision => Space::new().into(),
        ItemKind::Job => job(node, expanded),
        ItemKind::Todo => todo(node),
        ItemKind::Surface => surface(node),
        ItemKind::Compaction => compaction(node, expanded),
        ItemKind::Ext => ext(node, expanded),
    }
}

fn user(node: &Node) -> Element<'_, Message> {
    let body = container(text(node.text.as_str()))
        .padding([8, 12])
        .width(Fill)
        .style(container::rounded_box);
    button(body)
        .style(button::text)
        .padding(0)
        .width(Fill)
        .on_press(Message::Rewind(node.id))
        .into()
}

fn assistant<'a>(
    node: &'a Node,
    streamed: Option<&'a Streamed>,
    settings: markdown::Settings,
) -> Element<'a, Message> {
    let body: Element<'a, Message> = match streamed {
        Some(streamed) => markdown::view(streamed.items(), settings).map(Message::Link),
        None => text(node.text.as_str()).into(),
    };
    let status = node.str_attr(attr::STATUS).and_then(MessageStatus::parse);
    match status {
        Some(MessageStatus::Cancelled) => column![body, muted("Interrupted").size(DETAIL_SIZE)]
            .spacing(4)
            .into(),
        Some(MessageStatus::Error) => {
            column![body, text("Failed").size(DETAIL_SIZE).style(text::danger)]
                .spacing(4)
                .into()
        }
        _ => body,
    }
}

fn thinking(node: &Node, expanded: bool) -> Element<'_, Message> {
    let streaming = node.str_attr(attr::STATUS).and_then(MessageStatus::parse)
        == Some(MessageStatus::Streaming);
    let label = if streaming { "Thinking…" } else { "Thought" };
    let head = header(
        row![muted(label).font(italic()), muted(chevron(expanded))].spacing(6),
        node.id,
    );
    if !expanded {
        return head;
    }
    column![head, muted(node.text.as_str()).font(italic())]
        .spacing(4)
        .into()
}

fn italic() -> Font {
    Font {
        style: iced::font::Style::Italic,
        ..Font::DEFAULT
    }
}

fn status_dot<'a>(status: GroupStatus) -> text::Text<'a> {
    let dot = text("●");
    match status {
        GroupStatus::Running => dot.style(text::primary),
        GroupStatus::Done => dot.style(text::success),
        GroupStatus::Error => dot.style(text::danger),
        GroupStatus::Cancelled => dot.style(text::secondary),
    }
}

fn calls<'a>(dom: &'a Dom, item: &'a Item, expanded: bool) -> Element<'a, Message> {
    let summary = summarize(dom, &item.calls);
    let mut line = row![status_dot(summary.status), text(summary.label)].spacing(8);
    match (summary.status, summary.duration_ms) {
        (GroupStatus::Running, _) => line = line.push(muted("running…")),
        (_, Some(ms)) => line = line.push(muted(format_duration(ms))),
        (_, None) => {}
    }
    if summary.failed > 0 {
        line = line.push(text(format!("{} failed", summary.failed)).style(text::danger));
    }
    if summary.status == GroupStatus::Cancelled {
        line = line.push(muted("cancelled"));
    }
    let head = header(line.push(muted(chevron(expanded))), item.key);
    if !expanded {
        return head;
    }
    let details = item
        .calls
        .iter()
        .filter_map(|id| dom.get(*id))
        .map(|call| call_details(dom, call));
    column![head, column(details).spacing(8).padding([0, 16])]
        .spacing(6)
        .into()
}

fn call_details<'a>(dom: &'a Dom, call: &'a Node) -> Element<'a, Message> {
    let tool = call.str_attr(attr::TOOL).unwrap_or_default();
    let title = call.str_attr(attr::TITLE).unwrap_or(tool);
    let mut col = column![text(title).font(Font::MONOSPACE).size(DETAIL_SIZE)].spacing(4);
    for input in dom.children_of_kind(call.id, ElementKind::Input) {
        let (shown, left_out) = leading(&input.text);
        col = col
            .push(muted("Input").size(DETAIL_SIZE))
            .push(block(shown, left_out, false));
    }
    for result in dom.children_of_kind(call.id, ElementKind::Result) {
        let error = result.bool_attr(attr::IS_ERROR) == Some(true);
        let (shown, left_out) = leading(&result.text);
        let label = if error {
            text("Error").style(text::danger)
        } else {
            muted("Result")
        };
        col = col
            .push(label.size(DETAIL_SIZE))
            .push(block(shown, left_out, error));
    }
    if call.str_attr(attr::STATUS).and_then(CallStatus::parse) == Some(CallStatus::Error)
        && dom
            .children_of_kind(call.id, ElementKind::Result)
            .next()
            .is_none()
    {
        col = col.push(text("Failed").size(DETAIL_SIZE).style(text::danger));
    }
    col.into()
}

fn job(node: &Node, expanded: bool) -> Element<'_, Message> {
    let kind = node.str_attr(attr::KIND).and_then(JobKind::parse);
    let noun = match kind {
        Some(JobKind::Subagent) => "Agent",
        _ => "Shell",
    };
    let name = node
        .str_attr(attr::TITLE)
        .or_else(|| node.str_attr(attr::COMMAND))
        .or_else(|| node.str_attr(attr::AGENT))
        .unwrap_or_default();
    let status = match node.str_attr(attr::STATUS).and_then(JobStatus::parse) {
        Some(JobStatus::Running) | None => GroupStatus::Running,
        Some(JobStatus::Done) => GroupStatus::Done,
        Some(JobStatus::Error) => GroupStatus::Error,
        Some(JobStatus::Cancelled) => GroupStatus::Cancelled,
    };
    let state = match status {
        GroupStatus::Running => "running…",
        GroupStatus::Done => "done",
        GroupStatus::Error => "failed",
        GroupStatus::Cancelled => "cancelled",
    };
    let mut line = row![
        status_dot(status),
        text(noun),
        text(name).font(Font::MONOSPACE),
        muted(state)
    ]
    .spacing(8);
    if let Some(code) = node.int_attr(attr::EXIT_CODE).filter(|c| *c != 0) {
        line = line.push(text(format!("exit {code}")).style(text::danger));
    }
    let head = header(line.push(muted(chevron(expanded))), node.id);
    if !expanded || node.text.is_empty() {
        return head;
    }
    let (shown, left_out) = trailing(&node.text);
    let mut col = column![head].spacing(6);
    if left_out > 0 {
        col = col.push(omitted(left_out));
    }
    col.push(block(shown, 0, status == GroupStatus::Error))
        .into()
}

fn todo(node: &Node) -> Element<'_, Message> {
    let state = node
        .str_attr(attr::STATE)
        .and_then(TodoState::parse)
        .unwrap_or(TodoState::Pending);
    let (mark, done) = match state {
        TodoState::Pending => ("☐", false),
        TodoState::InProgress => ("◐", false),
        TodoState::Done => ("☑", true),
        TodoState::Cancelled => ("☒", true),
    };
    let body = text(node.text.as_str());
    let body = if done {
        body.style(text::secondary)
    } else {
        body
    };
    row![
        text(mark).style(if done { text::secondary } else { text::primary }),
        body
    ]
    .spacing(8)
    .into()
}

fn surface(node: &Node) -> Element<'_, Message> {
    let name = node
        .str_attr(attr::NAME)
        .or_else(|| node.str_attr(attr::URI))
        .unwrap_or_default();
    let mode = node.str_attr(attr::MODE).unwrap_or_default();
    row![
        text("Surface"),
        text(name).font(Font::MONOSPACE),
        muted(mode)
    ]
    .spacing(8)
    .into()
}

fn compaction(node: &Node, expanded: bool) -> Element<'_, Message> {
    let covered = node
        .attr(attr::COVERS)
        .and_then(|v| v.as_list())
        .map_or(0, <[_]>::len);
    let label = match covered {
        0 => "Conversation compacted".to_owned(),
        1 => "Compacted 1 turn".to_owned(),
        n => format!("Compacted {n} turns"),
    };
    let head = header(
        row![muted(label), muted(chevron(expanded))].spacing(6),
        node.id,
    );
    if !expanded {
        return head;
    }
    column![head, muted(node.text.as_str())].spacing(4).into()
}

fn ext(node: &Node, expanded: bool) -> Element<'_, Message> {
    let harness = node.str_attr(attr::HARNESS).unwrap_or_default();
    let kind = node.str_attr(attr::TYPE).unwrap_or("event");
    let head = header(
        row![muted(harness), muted(kind), muted(chevron(expanded))].spacing(6),
        node.id,
    );
    if !expanded {
        return head;
    }
    let raw = if node.text.is_empty() {
        node.str_attr(attr::RAW).unwrap_or_default()
    } else {
        node.text.as_str()
    };
    let (shown, left_out) = leading(raw);
    column![head, block(shown, left_out, false)]
        .spacing(4)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{Draft, Value};

    fn call(dom: &mut Dom, turn: NodeId, tool: &str, attrs: &[(&str, Value)]) -> NodeId {
        let mut all = vec![("tool", Value::from(tool))];
        all.extend(attrs.iter().cloned());
        dom.commit(Draft::insert(turn, ElementKind::Call, all, ""), 0)
            .unwrap()
            .node
    }

    fn turn() -> (Dom, NodeId) {
        let mut dom = Dom::new();
        let attrs: [(&str, Value); 4] = [
            ("id", "s".into()),
            ("harness", "omp".into()),
            ("node", "n".into()),
            ("created", Value::Int(0)),
        ];
        let s = dom
            .commit(
                Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""),
                0,
            )
            .unwrap()
            .node;
        let t = dom
            .commit(
                Draft::insert(s, ElementKind::Turn, Vec::<(&str, Value)>::new(), ""),
                0,
            )
            .unwrap()
            .node;
        (dom, t)
    }

    fn ended(status: &str, started: i64, end: i64) -> Vec<(&'static str, Value)> {
        vec![
            ("status", status.into()),
            ("started", Value::Int(started)),
            ("ended", Value::Int(end)),
        ]
    }

    #[test]
    fn a_finished_group_reads_like_claude_code() {
        let (mut dom, t) = turn();
        let a = call(&mut dom, t, "read", &ended("done", 1_000, 4_000));
        let b = call(&mut dom, t, "read", &ended("done", 2_000, 9_000));
        let c = call(&mut dom, t, "read", &ended("done", 3_000, 13_000));
        let summary = summarize(&dom, &[a, b, c]);
        assert_eq!(summary.label, "Read 3 files");
        assert_eq!(summary.status, GroupStatus::Done);
        assert_eq!(
            summary.duration_ms.map(format_duration).as_deref(),
            Some("12s")
        );
        assert_eq!(summary.failed, 0);
    }

    #[test]
    fn one_call_shows_its_title_and_a_running_group_has_no_duration() {
        let (mut dom, t) = turn();
        let a = call(
            &mut dom,
            t,
            "Bash",
            &[("title", "cargo test".into()), ("status", "running".into())],
        );
        let summary = summarize(&dom, &[a]);
        assert_eq!(summary.label, "Ran cargo test");
        assert_eq!(summary.status, GroupStatus::Running);
        assert_eq!(summary.duration_ms, None);
    }

    #[test]
    fn an_error_result_marks_the_group_failed() {
        let (mut dom, t) = turn();
        let a = call(&mut dom, t, "edit", &ended("done", 0, 100));
        let b = call(&mut dom, t, "edit", &ended("done", 0, 100));
        dom.commit(
            Draft::insert(
                b,
                ElementKind::Result,
                [("is_error", Value::Bool(true))],
                "no match",
            ),
            0,
        )
        .unwrap();
        let summary = summarize(&dom, &[a, b]);
        assert_eq!(summary.status, GroupStatus::Error);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.label, "Edit 2 files");
    }

    #[test]
    fn long_output_is_cut_on_lines_and_reports_what_was_left_out() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let (shown, left_out) = leading(&text);
        assert_eq!(shown.lines().count(), MAX_LINES);
        assert_eq!(shown.len() + left_out, text.len());
        let (end, skipped) = trailing(&text);
        assert_eq!(end.lines().count(), MAX_LINES);
        assert!(end.ends_with("line 99\n"));
        assert_eq!(skipped + end.len(), text.len());
    }
}
