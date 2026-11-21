//! Agents tab: live subagents and background shells from the `<job>` elements
//! of one session, plus whether omp's `wait` tool is blocked on them.
//!
//! omp's `wait` takes no arguments and blocks on every running job of the
//! caller, so while a `<call tool="wait">` is pending or running, every running
//! job is awaited.

use crate::style;
use iced::widget::text::Wrapping;
use iced::widget::{Column, Row, button, container, scrollable, space, text};
use iced::{Center, Element, Fill, Font, Task, Theme};
use loa_core::schema::TEXT;
use loa_core::{CallStatus, Dom, ElementKind, JobKind, JobStatus, Node, NodeId, Op, Patch, attr};
use loa_ui_kit::icons::Icon;
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

/// Tool name of omp's blocking wait.
const WAIT_TOOL: &str = "wait";

/// One `<job>` element as the panel shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    node: NodeId,
    kind: JobKind,
    status: JobStatus,
    id: Option<String>,
    title: Option<String>,
    command: Option<String>,
    agent: Option<String>,
    session: Option<String>,
    started: Option<i64>,
    ended: Option<i64>,
    exit_code: Option<i64>,
    output: String,
    /// Byte range of the last non-blank output line, trimmed. Empty when the
    /// output has no such line.
    last_line: Range<usize>,
    /// Start of the line the output ends in. Lines before it are complete and
    /// already searched for `last_line`.
    line_start: usize,
    expanded: bool,
}

impl Job {
    /// Builds an entry from a `<job>` node. `None` when the node isn't a job.
    fn from_node(node: &Node) -> Option<Self> {
        if node.kind != ElementKind::Job {
            return None;
        }
        let mut job = Job {
            node: node.id,
            kind: JobKind::parse(node.str_attr(attr::KIND)?)?,
            status: JobStatus::Running,
            id: None,
            title: None,
            command: None,
            agent: None,
            session: None,
            started: None,
            ended: None,
            exit_code: None,
            output: node.text.clone(),
            last_line: 0..0,
            line_start: 0,
            expanded: false,
        };
        job.read_attrs(node);
        job.rescan_output();
        Some(job)
    }

    /// Copies every attribute from `node`. A missing status reads as running.
    fn read_attrs(&mut self, node: &Node) {
        fn copy(slot: &mut Option<String>, node: &Node, name: &str) {
            match (node.str_attr(name), slot) {
                (Some(new), Some(old)) => new.clone_into(old),
                (new, slot) => *slot = new.map(str::to_owned),
            }
        }
        if let Some(kind) = node.str_attr(attr::KIND).and_then(JobKind::parse) {
            self.kind = kind;
        }
        self.status = node.str_attr(attr::STATUS).and_then(JobStatus::parse).unwrap_or(JobStatus::Running);
        copy(&mut self.id, node, attr::ID);
        copy(&mut self.title, node, attr::TITLE);
        copy(&mut self.command, node, attr::COMMAND);
        copy(&mut self.agent, node, attr::AGENT);
        copy(&mut self.session, node, attr::SESSION);
        self.started = node.int_attr(attr::STARTED);
        self.ended = node.int_attr(attr::ENDED);
        self.exit_code = node.int_attr(attr::EXIT_CODE);
    }

    /// Recomputes the last line after the whole output was replaced.
    fn rescan_output(&mut self) {
        self.last_line = 0..0;
        self.find_last_line(0);
        self.line_start = self.output.rfind('\n').map_or(0, |i| i + 1);
    }

    /// Adds streamed output, searching only the unfinished line and `chunk`.
    fn append(&mut self, chunk: &str) {
        let old_len = self.output.len();
        self.output.push_str(chunk);
        self.find_last_line(self.line_start);
        if let Some(i) = chunk.rfind('\n') {
            self.line_start = old_len + i + 1;
        }
    }

    /// Sets `last_line` to the last non-blank line starting at or after `from`,
    /// scanning backwards from the end. Keeps the old value when there is none.
    fn find_last_line(&mut self, from: usize) {
        let region = &self.output[from..];
        let mut end = region.len();
        loop {
            let start = region[..end].rfind('\n').map_or(0, |i| i + 1);
            let line = &region[start..end];
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                let lead = line.len() - line.trim_start().len();
                let begin = from + start + lead;
                self.last_line = begin..begin + trimmed.len();
                return;
            }
            if start == 0 {
                return;
            }
            end = start - 1;
        }
    }

    /// Node id of the `<job>` element.
    pub fn node(&self) -> NodeId {
        self.node
    }

    pub fn kind(&self) -> JobKind {
        self.kind
    }

    /// Status of the job. A job without a `status` attribute is running.
    pub fn status(&self) -> JobStatus {
        self.status
    }

    /// The harness's id for the job, used to cancel it.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    pub fn agent(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// Session id of a subagent's child session.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Unix milliseconds.
    pub fn started(&self) -> Option<i64> {
        self.started
    }

    /// Unix milliseconds.
    pub fn ended(&self) -> Option<i64> {
        self.ended
    }

    pub fn exit_code(&self) -> Option<i64> {
        self.exit_code
    }

    /// Name shown on the row: title, else agent, else command, else kind.
    pub fn label(&self) -> &str {
        self.title
            .as_deref()
            .or(self.agent.as_deref())
            .or(self.command.as_deref())
            .unwrap_or(self.kind.as_str())
    }

    /// Output so far.
    pub fn output(&self) -> &str {
        &self.output
    }

    /// Last non-blank output line without surrounding whitespace. Empty when
    /// there is none.
    pub fn last_line(&self) -> &str {
        &self.output[self.last_line.clone()]
    }

    /// The full output is shown below the row.
    pub fn expanded(&self) -> bool {
        self.expanded
    }
}

/// Jobs of one session in document order and the wait calls blocked on them.
#[derive(Debug, Default)]
pub struct State {
    jobs: BTreeMap<NodeId, Job>,
    /// `<call tool="wait">` nodes that are pending or running.
    waits: HashSet<NodeId>,
    /// Unix milliseconds of the last tick.
    now: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Current time in unix milliseconds.
    Tick(i64),
    /// Show or hide the full output of a job.
    Toggle(NodeId),
    Open(NodeId),
    /// Open the subagent's session in its own window.
    Detach(NodeId),
    Cancel(NodeId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Open the child session with this id.
    OpenSession(String),
    /// Show the child session with this id in its own window.
    DetachSession(String),
    /// Stop a running job. `id` is the harness's job id when known.
    Cancel { node: NodeId, id: Option<String> },
}

impl State {
    pub fn from_dom(dom: &Dom) -> Self {
        let mut state = Self::default();
        state.scan(dom);
        state
    }

    /// Rebuilds the list from `dom`, keeping expanded rows that still exist.
    pub fn reset(&mut self, dom: &Dom) {
        let expanded: Vec<NodeId> = self.jobs.values().filter(|j| j.expanded).map(|j| j.node).collect();
        self.jobs.clear();
        self.waits.clear();
        self.scan(dom);
        for node in expanded {
            if let Some(job) = self.jobs.get_mut(&node) {
                job.expanded = true;
            }
        }
    }

    fn scan(&mut self, dom: &Dom) {
        for node in dom.iter() {
            match node.kind {
                ElementKind::Job => {
                    if let Some(job) = Job::from_node(node) {
                        self.jobs.insert(node.id, job);
                    }
                }
                ElementKind::Call if is_active_wait(node) => {
                    self.waits.insert(node.id);
                }
                _ => {}
            }
        }
    }

    /// Updates the list for `patch`, which was already applied to `dom`.
    pub fn apply(&mut self, dom: &Dom, patch: &Patch) {
        match &patch.op {
            Op::Insert { kind: ElementKind::Job, .. } => {
                if let Some(job) = dom.get(patch.node).and_then(Job::from_node) {
                    self.jobs.insert(patch.node, job);
                }
            }
            Op::Insert { kind: ElementKind::Call, .. } => self.refresh_wait(dom, patch.node),
            Op::Insert { .. } => {}
            Op::Set { name, .. } => {
                if let Some(job) = self.jobs.get_mut(&patch.node) {
                    if let Some(node) = dom.get(patch.node) {
                        if name == TEXT {
                            node.text.clone_into(&mut job.output);
                            job.rescan_output();
                        } else {
                            job.read_attrs(node);
                        }
                    }
                } else {
                    self.refresh_wait(dom, patch.node);
                }
            }
            Op::Append { text } => {
                if let Some(job) = self.jobs.get_mut(&patch.node) {
                    job.append(text);
                }
            }
            Op::Remove => {
                self.jobs.retain(|node, _| dom.contains(*node));
                self.waits.retain(|node| dom.contains(*node));
            }
        }
    }

    /// Tracks `node` as a blocking wait call exactly while it is one.
    fn refresh_wait(&mut self, dom: &Dom, node: NodeId) {
        if dom.get(node).is_some_and(is_active_wait) {
            self.waits.insert(node);
        } else {
            self.waits.remove(&node);
        }
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::Tick(now) => {
                self.now = Some(now);
                None
            }
            Message::Toggle(node) => {
                if let Some(job) = self.jobs.get_mut(&node) {
                    job.expanded = !job.expanded;
                }
                None
            }
            Message::Open(node) => {
                self.jobs.get(&node).and_then(|job| job.session.clone()).map(Event::OpenSession)
            }
            Message::Detach(node) => {
                self.jobs.get(&node).and_then(|job| job.session.clone()).map(Event::DetachSession)
            }
            Message::Cancel(node) => self
                .jobs
                .get(&node)
                .filter(|job| job.status == JobStatus::Running)
                .map(|job| Event::Cancel { node, id: job.id.clone() }),
        };
        (Task::none(), event)
    }

    /// Jobs in document order.
    pub fn jobs(&self) -> impl Iterator<Item = &Job> {
        self.jobs.values()
    }

    pub fn job(&self, node: NodeId) -> Option<&Job> {
        self.jobs.get(&node)
    }

    /// A `wait` call is blocked on the running jobs.
    pub fn waiting(&self) -> bool {
        !self.waits.is_empty()
    }

    /// The job is running and a `wait` call is blocked on it.
    pub fn is_awaited(&self, node: NodeId) -> bool {
        self.waiting() && self.jobs.get(&node).is_some_and(|job| job.status == JobStatus::Running)
    }

    /// Time the job ran: until `ended`, or until the last tick while it is
    /// still running. `None` without `started`, before the first tick, or for
    /// a finished job without `ended`.
    pub fn elapsed_ms(&self, node: NodeId) -> Option<i64> {
        let job = self.jobs.get(&node)?;
        let started = job.started?;
        let end = match job.ended {
            Some(ended) => ended,
            None if job.status == JobStatus::Running => self.now?,
            None => return None,
        };
        Some((end - started).max(0))
    }

    pub fn view(&self) -> Element<'_, Message> {
        let running = self.jobs.values().filter(|job| job.status == JobStatus::Running).count();
        let mut header = Row::new().spacing(6).align_y(Center).height(style::LIST_ROW_HEIGHT);
        header = header.push(text(format!("{running} running")).size(12).style(style::muted));
        if self.waiting() && running > 0 {
            header = header.push(badge("wait blocked"));
        }

        let mut list = Column::with_capacity(self.jobs.len() + 1).spacing(2).padding([4, 14]).push(header);
        if self.jobs.is_empty() {
            list = list.push(text("No subagents or background jobs").size(style::TEXT_SIZE).style(style::muted));
        }
        for job in self.jobs.values() {
            list = list.push(self.job_view(job));
        }
        scrollable(list).height(Fill).into()
    }

    fn job_view<'a>(&'a self, job: &'a Job) -> Element<'a, Message> {
        let kind = match job.kind {
            JobKind::Shell => Icon::Terminal,
            JobKind::Subagent => Icon::Cpu,
        };
        let status_style: fn(&Theme) -> text::Style = match job.status {
            JobStatus::Running => style::muted,
            JobStatus::Done => style::success_text,
            JobStatus::Error => style::danger_text,
            JobStatus::Cancelled => style::muted,
        };

        let mut head = Row::new().spacing(8).align_y(Center).height(style::LIST_ROW_HEIGHT);
        head = head
            .push(loa_ui_kit::icons::icon(kind, 14.0))
            .push(text(job.label()).size(style::LIST_TEXT_SIZE).wrapping(Wrapping::None))
            .push(text(job.status.as_str()).size(12).style(status_style));
        if job.status != JobStatus::Running
            && let Some(code) = job.exit_code
            && code != 0
        {
            head = head.push(text(format!("exit {code}")).size(12).style(style::danger_text));
        }
        if self.is_awaited(job.node) {
            head = head.push(badge("waiting on"));
        }
        if let Some(ms) = self.elapsed_ms(job.node) {
            head = head.push(text(format_elapsed(ms)).size(12).style(style::muted));
        }
        head = head.push(space().width(Fill));
        if job.kind == JobKind::Subagent && job.session.is_some() {
            head = head.push(action("Open", Message::Open(job.node)));
            head = head.push(action("Window", Message::Detach(job.node)));
        }
        if job.status == JobStatus::Running {
            head = head.push(action("Cancel", Message::Cancel(job.node)));
        }
        if !job.output.is_empty() {
            let label = if job.expanded { "Collapse" } else { "Expand" };
            head = head.push(action(label, Message::Toggle(job.node)));
        }

        let mut entry = Column::with_capacity(2).push(head);
        if job.expanded {
            let output = text(job.output()).font(Font::MONOSPACE).size(style::TEXT_SIZE);
            entry = entry.push(container(output).padding([6, 10]).width(Fill).style(style::band));
        } else if !job.last_line.is_empty() {
            let line = text(job.last_line()).size(12).style(style::muted).wrapping(Wrapping::None);
            entry = entry.push(Row::new().push(space().width(22)).push(line));
        }
        entry.into()
    }
}

/// Formats a duration as its two largest units: `5s`, `1m 05s`, `2h 03m`.
pub fn format_elapsed(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
    }
}

/// A `<call tool="wait">` that is pending or running.
fn is_active_wait(node: &Node) -> bool {
    node.kind == ElementKind::Call
        && node.str_attr(attr::TOOL) == Some(WAIT_TOOL)
        && matches!(
            node.str_attr(attr::STATUS).and_then(CallStatus::parse),
            Some(CallStatus::Pending | CallStatus::Running)
        )
}

fn badge(label: &str) -> Element<'_, Message> {
    container(text(label).size(12).style(style::muted)).padding([1, 6]).style(style::badge).into()
}

fn action(label: &str, message: Message) -> Element<'_, Message> {
    button(text(label).size(12)).padding([2, 6]).style(style::action_button).on_press(message).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{Draft, Value};

    struct Fixture {
        dom: Dom,
        state: State,
        turn: NodeId,
    }

    impl Fixture {
        fn new() -> Self {
            let mut fixture = Fixture { dom: Dom::new(), state: State::default(), turn: NodeId::DOCUMENT };
            let session = fixture.commit(Draft::insert(
                NodeId::DOCUMENT,
                ElementKind::Session,
                [
                    (attr::ID, Value::from("s1")),
                    (attr::HARNESS, Value::from("omp")),
                    (attr::NODE, Value::from("key")),
                    (attr::CREATED, Value::from(0_i64)),
                ],
                "",
            ));
            fixture.turn = fixture.commit(Draft::insert(session, ElementKind::Turn, [(attr::STATUS, "running".into())], ""));
            fixture
        }

        fn commit(&mut self, draft: Draft) -> NodeId {
            let patch = self.dom.commit(draft, 0).expect("fixture patch is valid");
            self.state.apply(&self.dom, &patch);
            patch.node
        }

        fn job(&mut self, parent: NodeId, attrs: Vec<(&str, Value)>) -> NodeId {
            self.commit(Draft::insert(parent, ElementKind::Job, attrs, ""))
        }

        fn wait_call(&mut self, status: &str) -> NodeId {
            let turn = self.turn;
            self.commit(Draft::insert(
                turn,
                ElementKind::Call,
                [(attr::TOOL, Value::from(WAIT_TOOL)), (attr::STATUS, status.into())],
                "",
            ))
        }

        fn get(&self, node: NodeId) -> &Job {
            self.state.job(node).expect("job is listed")
        }
    }

    fn shell(command: &str) -> Vec<(&'static str, Value)> {
        vec![
            (attr::KIND, JobKind::Shell.into()),
            (attr::STATUS, JobStatus::Running.into()),
            (attr::COMMAND, command.into()),
            (attr::STARTED, 1_000_i64.into()),
        ]
    }

    #[test]
    fn inserted_job_is_listed_as_running() {
        let mut fx = Fixture::new();
        let turn = fx.turn;
        let job = fx.job(turn, shell("cargo build"));
        let listed: Vec<_> = fx.state.jobs().map(|j| (j.node(), j.status(), j.label())).collect();
        assert_eq!(listed, [(job, JobStatus::Running, "cargo build")]);
    }

    #[test]
    fn last_line_follows_appends_split_mid_line() {
        let mut fx = Fixture::new();
        let turn = fx.turn;
        let job = fx.job(turn, shell("ls"));
        fx.commit(Draft::append(job, "ab"));
        assert_eq!(fx.get(job).last_line(), "ab");
        fx.commit(Draft::append(job, "c\nde"));
        assert_eq!(fx.get(job).last_line(), "de");
        fx.commit(Draft::append(job, "f\n"));
        assert_eq!(fx.get(job).last_line(), "def");
        fx.commit(Draft::append(job, "  \n\n"));
        assert_eq!(fx.get(job).last_line(), "def");
        assert_eq!(fx.get(job).output(), "abc\ndef\n  \n\n");
    }

    #[test]
    fn finished_job_stops_elapsed_at_ended() {
        let mut fx = Fixture::new();
        let turn = fx.turn;
        let job = fx.job(turn, shell("sleep 100"));
        let _ = fx.state.update(Message::Tick(66_000));
        assert_eq!(fx.state.elapsed_ms(job), Some(65_000));
        assert_eq!(format_elapsed(65_000), "1m 05s");

        fx.commit(Draft::set(job, attr::STATUS, JobStatus::Done));
        fx.commit(Draft::set(job, attr::ENDED, 6_000_i64));
        let _ = fx.state.update(Message::Tick(500_000));
        assert_eq!(fx.state.elapsed_ms(job), Some(5_000));
    }

    #[test]
    fn format_elapsed_uses_two_largest_units() {
        assert_eq!(format_elapsed(5_999), "5s");
        assert_eq!(format_elapsed(60_000), "1m 00s");
        assert_eq!(format_elapsed((2 * 3600 + 3 * 60 + 59) * 1000), "2h 03m");
    }

    #[test]
    fn removing_turn_drops_its_jobs() {
        let mut fx = Fixture::new();
        let session = fx.dom.root().expect("session exists").id;
        let turn = fx.turn;
        fx.job(turn, shell("a"));
        let kept = fx.job(session, shell("b"));
        fx.commit(Draft::remove(turn));
        let left: Vec<_> = fx.state.jobs().map(Job::node).collect();
        assert_eq!(left, [kept]);
    }

    #[test]
    fn running_wait_marks_running_jobs_awaited_until_done() {
        let mut fx = Fixture::new();
        let turn = fx.turn;
        let running = fx.job(turn, shell("a"));
        let finished = fx.job(turn, shell("b"));
        fx.commit(Draft::set(finished, attr::STATUS, JobStatus::Done));
        assert!(!fx.state.is_awaited(running));

        let wait = fx.wait_call("pending");
        assert!(fx.state.is_awaited(running));
        assert!(!fx.state.is_awaited(finished));
        fx.commit(Draft::set(wait, attr::STATUS, CallStatus::Running));
        assert!(fx.state.is_awaited(running));

        fx.commit(Draft::set(wait, attr::STATUS, CallStatus::Done));
        assert!(!fx.state.is_awaited(running));
        assert!(!fx.state.waiting());
    }

    #[test]
    fn open_emits_child_session_and_cancel_ignores_finished_jobs() {
        let mut fx = Fixture::new();
        let turn = fx.turn;
        let sub = fx.job(
            turn,
            vec![
                (attr::KIND, JobKind::Subagent.into()),
                (attr::ID, "job-1".into()),
                (attr::SESSION, "child-7".into()),
            ],
        );
        let (_, event) = fx.state.update(Message::Open(sub));
        assert_eq!(event, Some(Event::OpenSession("child-7".to_owned())));
        let (_, event) = fx.state.update(Message::Cancel(sub));
        assert_eq!(event, Some(Event::Cancel { node: sub, id: Some("job-1".to_owned()) }));

        fx.commit(Draft::set(sub, attr::STATUS, JobStatus::Done));
        let (_, event) = fx.state.update(Message::Cancel(sub));
        assert_eq!(event, None);

        let sh = fx.job(turn, shell("ls"));
        let (_, event) = fx.state.update(Message::Open(sh));
        assert_eq!(event, None);
    }

    #[test]
    fn incremental_state_matches_full_scan() {
        let mut fx = Fixture::new();
        let session = fx.dom.root().expect("session exists").id;
        let turn = fx.turn;
        let a = fx.job(turn, shell("a"));
        let b = fx.job(session, shell("b"));
        let wait = fx.wait_call("running");
        fx.commit(Draft::append(a, "one\ntw"));
        fx.commit(Draft::append(b, "x"));
        fx.commit(Draft::append(a, "o\n"));
        fx.commit(Draft::set(b, attr::TITLE, "build"));
        fx.commit(Draft::set_text(b, "replaced\n\n"));
        fx.commit(Draft::append(b, "tail"));
        fx.commit(Draft::set(a, attr::EXIT_CODE, 1_i64));
        fx.commit(Draft::set(a, attr::STATUS, JobStatus::Error));
        let turn2 = fx.commit(Draft::insert(session, ElementKind::Turn, Vec::<(&str, Value)>::new(), ""));
        let c = fx.job(turn2, shell("c"));
        fx.commit(Draft::append(c, "\n  last  \n"));
        fx.commit(Draft::remove(turn));

        let full = State::from_dom(&fx.dom);
        assert!(fx.state.jobs().eq(full.jobs()));
        assert_eq!(fx.state.waiting(), full.waiting());
        assert!(!fx.dom.contains(wait));
        assert_eq!(fx.get(b).last_line(), "tail");
        assert_eq!(fx.get(c).last_line(), "last");
    }
}
