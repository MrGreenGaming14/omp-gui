//! The agents tab: live subagents and background shells from one session's `<job>` elements,
//! and whether omp's `wait` blocks on them. `wait` takes no arguments and blocks on every
//! running job, so while a `<call tool="wait">` is pending or running, every running job is
//! awaited.

use crate::{action, list_row_height};
use iced::widget::text::Wrapping;
use iced::widget::{Column, Row, container, scrollable, space, text};
use iced::{Center, Element, Fill, Font, Task};
use loa_core::schema::TEXT;
use loa_core::{CallStatus, Dom, ElementKind, JobKind, JobStatus, Node, NodeId, Op, Patch, attr};
use loa_ui_kit::icons::{Icon, icon};
use loa_ui_kit::style::{self as kit, Tone};
use loa_ui_kit::text::format_duration;
use loa_ui_kit::theme::{ColorRole as C, SpaceRole as Sp, TextRole as T, sp, text_size as ts};
use loa_ui_kit::widgets::{PanelPane, badge, label};
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

const WAIT_TOOL: &str = "wait";

/// One `<job>` as the panel shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    node: NodeId,
    kind: JobKind,
    status: JobStatus,
    /// `id`, `title`, `command`, `agent` and `session` attributes.
    attrs: [Option<String>; 5],
    started: Option<i64>,
    ended: Option<i64>,
    exit_code: Option<i64>,
    output: String,
    /// The last non-blank output line, trimmed; empty when there is none.
    last_line: Range<usize>,
    /// Start of the unfinished last line; lines before it were already searched.
    line_start: usize,
    expanded: bool,
}

const ATTRS: [&str; 5] = [attr::ID, attr::TITLE, attr::COMMAND, attr::AGENT, attr::SESSION];

impl Job {
    fn from_node(node: &Node) -> Option<Self> {
        if node.kind != ElementKind::Job { return None }
        let kind = JobKind::parse(node.str_attr(attr::KIND)?)?;
        let mut job = Job { node: node.id, kind, status: JobStatus::Running, attrs: Default::default(), started: None, ended: None, exit_code: None, output: node.text.clone(), last_line: 0..0, line_start: 0, expanded: false };
        job.read_attrs(node);
        job.rescan();
        Some(job)
    }

    /// Copies every attribute; a missing status reads as running.
    fn read_attrs(&mut self, node: &Node) {
        self.kind = node.str_attr(attr::KIND).and_then(JobKind::parse).unwrap_or(self.kind);
        self.status = node.str_attr(attr::STATUS).and_then(JobStatus::parse).unwrap_or(JobStatus::Running);
        self.attrs = ATTRS.map(|a| node.str_attr(a).map(str::to_owned));
        (self.started, self.ended, self.exit_code) = (node.int_attr(attr::STARTED), node.int_attr(attr::ENDED), node.int_attr(attr::EXIT_CODE));
    }

    fn rescan(&mut self) {
        self.last_line = 0..0;
        self.find_last_line(0);
        self.line_start = self.output.rfind('\n').map_or(0, |i| i + 1);
    }

    /// Adds streamed output, searching only the unfinished line and `chunk`.
    fn append(&mut self, chunk: &str) {
        let old = self.output.len();
        self.output.push_str(chunk);
        self.find_last_line(self.line_start);
        if let Some(i) = chunk.rfind('\n') { self.line_start = old + i + 1 }
    }

    /// The last non-blank line starting at or after `from`; keeps the old one when none.
    fn find_last_line(&mut self, from: usize) {
        let region = &self.output[from..];
        if let Some(line) = region.split('\n').rev().find(|l| !l.trim().is_empty()) {
            let start = from + line.as_ptr() as usize - region.as_ptr() as usize + (line.len() - line.trim_start().len());
            self.last_line = start..start + line.trim().len();
        }
    }

    pub fn node(&self) -> NodeId { self.node }
    pub fn status(&self) -> JobStatus { self.status }
    /// The child session of a subagent.
    pub fn session(&self) -> Option<&str> { self.attrs[4].as_deref() }
    /// Title, else agent, else command, else kind.
    pub fn label(&self) -> &str { [1, 3, 2].iter().find_map(|&i| self.attrs[i].as_deref()).unwrap_or(self.kind.as_str()) }
    pub fn output(&self) -> &str { &self.output }
    pub fn last_line(&self) -> &str { &self.output[self.last_line.clone()] }
}

/// Jobs of one session in document order and the wait calls blocked on them.
#[derive(Debug, Default)]
pub struct State {
    jobs: BTreeMap<NodeId, Job>,
    /// Pending or running `<call tool="wait">` nodes.
    waits: HashSet<NodeId>,
    /// Unix milliseconds of the last tick.
    now: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// The time in unix milliseconds.
    Tick(i64),
    /// Show or hide a job's output.
    Toggle(NodeId),
    Open(NodeId),
    /// Open the subagent's session in its own window.
    Detach(NodeId),
    Cancel(NodeId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    OpenSession(String),
    DetachSession(String),
    /// Stop a running job; `id` is the harness's job id when known.
    Cancel { node: NodeId, id: Option<String> },
}

impl State {
    pub fn from_dom(dom: &Dom) -> Self {
        let mut s = Self::default();
        s.scan(dom);
        s
    }

    /// Rebuilds from `dom`, keeping rows that were expanded.
    pub fn reset(&mut self, dom: &Dom) {
        let expanded: Vec<NodeId> = self.jobs.values().filter(|j| j.expanded).map(|j| j.node).collect();
        (self.jobs.clear(), self.waits.clear(), self.scan(dom));
        for n in expanded {
            if let Some(j) = self.jobs.get_mut(&n) { j.expanded = true }
        }
    }

    fn scan(&mut self, dom: &Dom) {
        for node in dom.iter() {
            if let Some(job) = Job::from_node(node) { self.jobs.insert(node.id, job); }
            if is_active_wait(node) { self.waits.insert(node.id); }
        }
    }

    /// Follows `patch`, already applied to `dom`.
    pub fn apply(&mut self, dom: &Dom, patch: &Patch) {
        let node = patch.node;
        match (&patch.op, self.jobs.get_mut(&node), dom.get(node)) {
            (Op::Insert { .. }, _, Some(n)) => match Job::from_node(n) {
                Some(job) => { self.jobs.insert(node, job); }
                None => self.refresh_wait(n),
            },
            (Op::Set { name, .. }, Some(job), Some(n)) if name == TEXT => {
                n.text.clone_into(&mut job.output);
                job.rescan();
            }
            (Op::Set { .. }, Some(job), Some(n)) => job.read_attrs(n),
            (Op::Set { .. }, None, Some(n)) => self.refresh_wait(n),
            (Op::Append { text }, Some(job), _) => job.append(text),
            (Op::Remove, ..) => {
                self.jobs.retain(|n, _| dom.contains(*n));
                self.waits.retain(|n| dom.contains(*n));
            }
            _ => {}
        }
    }

    /// Tracks `node` as a blocking wait exactly while it is one.
    fn refresh_wait(&mut self, node: &Node) {
        if is_active_wait(node) { self.waits.insert(node.id); } else { self.waits.remove(&node.id); }
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let session = |s: &Self, n: NodeId| s.jobs.get(&n).and_then(|j| j.session()).map(str::to_owned);
        let event = match message {
            Message::Tick(now) => {
                self.now = Some(now);
                None
            }
            Message::Toggle(n) => {
                if let Some(j) = self.jobs.get_mut(&n) { j.expanded = !j.expanded }
                None
            }
            Message::Open(n) => session(self, n).map(Event::OpenSession),
            Message::Detach(n) => session(self, n).map(Event::DetachSession),
            Message::Cancel(n) => self.jobs.get(&n).filter(|j| j.status == JobStatus::Running).map(|j| Event::Cancel { node: n, id: j.attrs[0].clone() }),
        };
        (Task::none(), event)
    }

    pub fn jobs(&self) -> impl Iterator<Item = &Job> { self.jobs.values() }

    pub fn job(&self, node: NodeId) -> Option<&Job> { self.jobs.get(&node) }

    /// A `wait` call blocks on the running jobs.
    pub fn waiting(&self) -> bool { !self.waits.is_empty() }

    pub fn is_awaited(&self, node: NodeId) -> bool { self.waiting() && self.jobs.get(&node).is_some_and(|j| j.status == JobStatus::Running) }

    /// How long the job ran: to `ended`, or to the last tick while it runs.
    pub fn elapsed_ms(&self, node: NodeId) -> Option<i64> {
        let job = self.jobs.get(&node)?;
        let end = job.ended.or(if job.status == JobStatus::Running { self.now } else { None })?;
        Some((end - job.started?).max(0))
    }

    pub fn view(&self) -> Element<'_, Message> {
        let running = self.jobs.values().filter(|j| j.status == JobStatus::Running).count();
        let blocked = (self.waiting() && running > 0).then(|| badge("wait blocked", Tone::Muted));
        let header = Row::new().push(label(format!("{running} running"), T::Caption, Tone::Muted)).push(blocked).spacing(sp(Sp::S)).align_y(Center).height(list_row_height());
        let empty = self.jobs.is_empty().then(|| label("No subagents or background jobs", T::Label, Tone::Muted));
        let list = Column::new().push(header).push(empty).extend(self.jobs.values().map(|j| self.job_view(j)));
        scrollable(list.spacing(sp(Sp::Xs)).padding([sp(Sp::Xs), sp(Sp::L)])).height(Fill).into()
    }

    fn job_view<'a>(&'a self, job: &'a Job) -> Element<'a, Message> {
        let tone = match job.status {
            JobStatus::Running | JobStatus::Cancelled => Tone::Muted,
            JobStatus::Done => Tone::Ok,
            JobStatus::Error => Tone::Danger,
        };
        let exit = job.exit_code.filter(|&c| c != 0 && job.status != JobStatus::Running).map(|c| label(format!("exit {c}"), T::Caption, Tone::Danger));
        let child = job.kind == JobKind::Subagent && job.session().is_some();
        let n = job.node;
        let head = Row::new()
            .push(icon(if job.kind == JobKind::Shell { Icon::Terminal } else { Icon::Cpu }, ts(T::Body)))
            .push(text(job.label()).size(ts(T::Body)).wrapping(Wrapping::None))
            .push(label(job.status.as_str(), T::Caption, tone))
            .push(exit)
            .push(self.is_awaited(n).then(|| badge("waiting on", Tone::Muted)))
            .push(self.elapsed_ms(n).map(|ms| label(format_duration(ms), T::Caption, Tone::Muted)))
            .push(space().width(Fill))
            .push(child.then(|| action("Open", Message::Open(n))))
            .push(child.then(|| action("Window", Message::Detach(n))))
            .push((job.status == JobStatus::Running).then(|| action("Cancel", Message::Cancel(n))))
            .push((!job.output.is_empty()).then(|| action(if job.expanded { "Collapse" } else { "Expand" }, Message::Toggle(n))));
        let below: Option<Element<'a, Message>> = if job.expanded {
            Some(container(text(job.output()).font(Font::MONOSPACE).size(ts(T::Label))).padding([sp(Sp::S), sp(Sp::M)]).width(Fill).style(kit::fill(C::Raised, 0.0)).into())
        } else {
            (!job.last_line.is_empty()).then(|| Row::new().push(space().width(sp(Sp::Xl))).push(label(job.last_line(), T::Caption, Tone::Muted).wrapping(Wrapping::None)).into())
        };
        Column::new().push(head.spacing(sp(Sp::S)).align_y(Center).height(list_row_height())).push(below).into()
    }
}

impl PanelPane for State {
    type Message = Message;
    fn id(&self) -> &str { "agents" }
    fn title(&self) -> &str { "Agents" }
    fn icon(&self) -> Icon { Icon::Bot }
    fn view(&self) -> Element<'_, Message> { State::view(self) }
}

/// A pending or running `<call tool="wait">`.
fn is_active_wait(n: &Node) -> bool {
    n.kind == ElementKind::Call && n.str_attr(attr::TOOL) == Some(WAIT_TOOL) && matches!(n.str_attr(attr::STATUS).and_then(CallStatus::parse), Some(CallStatus::Pending | CallStatus::Running))
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{Draft, Value};

    struct Fixture {
        dom: Dom,
        state: State,
        session: NodeId,
        turn: NodeId,
    }

    impl Fixture {
        fn new() -> Self {
            let mut f = Fixture { dom: Dom::new(), state: State::default(), session: NodeId::DOCUMENT, turn: NodeId::DOCUMENT };
            let attrs = [(attr::ID, Value::from("s1")), (attr::HARNESS, "omp".into()), (attr::NODE, "key".into()), (attr::CREATED, 0_i64.into())];
            f.session = f.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""));
            f.turn = f.commit(Draft::insert(f.session, ElementKind::Turn, [(attr::STATUS, "running".into())], ""));
            f
        }

        fn commit(&mut self, draft: Draft) -> NodeId {
            let patch = self.dom.commit(draft, 0).expect("valid patch");
            self.state.apply(&self.dom, &patch);
            patch.node
        }

        fn job(&mut self, parent: NodeId, attrs: Vec<(&str, Value)>) -> NodeId { self.commit(Draft::insert(parent, ElementKind::Job, attrs, "")) }

        fn shell(&mut self, parent: NodeId, command: &str) -> NodeId { self.job(parent, vec![(attr::KIND, JobKind::Shell.into()), (attr::STATUS, JobStatus::Running.into()), (attr::COMMAND, command.into()), (attr::STARTED, 1_000_i64.into())]) }
    }

    #[test]
    fn output_last_line_elapsed_and_waits_follow_patches() {
        let mut f = Fixture::new();
        let (turn, session) = (f.turn, f.session);
        let a = f.shell(turn, "cargo build");
        assert_eq!(f.state.jobs().map(|j| (j.status(), j.label())).collect::<Vec<_>>(), [(JobStatus::Running, "cargo build")]);
        for (chunk, last) in [("ab", "ab"), ("c\nde", "de"), ("f\n", "def"), ("  \n\n", "def")] {
            f.commit(Draft::append(a, chunk));
            assert_eq!(f.state.job(a).unwrap().last_line(), last);
        }
        let _ = f.state.update(Message::Tick(66_000));
        assert_eq!(f.state.elapsed_ms(a), Some(65_000));
        let b = f.shell(session, "b");
        let wait = f.commit(Draft::insert(turn, ElementKind::Call, [(attr::TOOL, Value::from(WAIT_TOOL)), (attr::STATUS, "pending".into())], ""));
        f.commit(Draft::set(b, attr::STATUS, JobStatus::Done));
        f.commit(Draft::set(b, attr::ENDED, 6_000_i64));
        assert_eq!((f.state.is_awaited(a), f.state.is_awaited(b), f.state.elapsed_ms(b)), (true, false, Some(5_000)));
        f.commit(Draft::set(wait, attr::STATUS, CallStatus::Done));
        assert!(!f.state.waiting());
        f.commit(Draft::set(b, attr::TITLE, "build"));
        f.commit(Draft::set_text(b, "replaced\n\n"));
        f.commit(Draft::append(b, "tail"));
        let turn2 = f.commit(Draft::insert(session, ElementKind::Turn, Vec::<(&str, Value)>::new(), ""));
        let c = f.shell(turn2, "c");
        f.commit(Draft::append(c, "\n  last  \n"));
        f.commit(Draft::remove(turn));
        let full = State::from_dom(&f.dom);
        assert!(f.state.jobs().eq(full.jobs()) && f.state.waiting() == full.waiting(), "incremental equals a fresh scan");
        assert_eq!((f.state.job(b).unwrap().last_line(), f.state.job(c).unwrap().last_line(), f.state.jobs().count()), ("tail", "last", 2));
    }

    #[test]
    fn open_and_cancel_emit_only_for_fitting_jobs() {
        let mut f = Fixture::new();
        let turn = f.turn;
        let sub = f.job(turn, vec![(attr::KIND, JobKind::Subagent.into()), (attr::ID, "job-1".into()), (attr::SESSION, "child-7".into())]);
        let sh = f.shell(turn, "ls");
        let cases = [(Message::Open(sub), Some(Event::OpenSession("child-7".into()))), (Message::Cancel(sub), Some(Event::Cancel { node: sub, id: Some("job-1".into()) })), (Message::Open(sh), None)];
        for (m, e) in cases { assert_eq!(f.state.update(m).1, e) }
        f.commit(Draft::set(sub, attr::STATUS, JobStatus::Done));
        assert_eq!(f.state.update(Message::Cancel(sub)).1, None);
        drop(iced_test::simulator(f.state.view()));
    }
}
