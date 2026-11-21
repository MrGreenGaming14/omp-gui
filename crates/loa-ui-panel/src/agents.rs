//! The agents tab: one session's `<job>`s; a pending or running `<call tool="wait">` awaits every running job.

use crate::{action, list_row_height};
use iced::widget::text::Wrapping;
use iced::widget::{Column, container, row, scrollable, space, text};
use iced::{Center, Element, Fill, Font, Task};
use loa_core::schema::TEXT;
use loa_core::{CallStatus, Dom, ElementKind, JobKind, JobStatus, Node, NodeId, Op, Patch, attr};
use loa_ui_kit::icons::icon;
use loa_view::panel::AgentsIntent;
use loa_view::theme::Icon;
use loa_ui_kit::style::{self as kit, Tone};
use loa_ui_kit::text::format_duration;
use loa_ui_kit::theme::{ColorRole as C, SpaceRole as Sp, TextRole as T, sp, text_size as ts};
use loa_ui_kit::widgets::{badge, label};
use std::collections::{BTreeMap, HashSet};

const WAIT_TOOL: &str = "wait";
/// The attributes a [`Job`] keeps, in its `attrs` order.
const ATTRS: [&str; 5] = [attr::ID, attr::TITLE, attr::COMMAND, attr::AGENT, attr::SESSION];

/// One `<job>` as the panel shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    node: NodeId, kind: JobKind, status: JobStatus, attrs: [Option<String>; 5],
    started: Option<i64>, ended: Option<i64>, exit_code: Option<i64>, output: String, expanded: bool,
}

impl Job {
    fn from_node(node: &Node) -> Option<Self> {
        if node.kind != ElementKind::Job { return None }
        let kind = JobKind::parse(node.str_attr(attr::KIND)?)?;
        let mut job = Job { node: node.id, kind, status: JobStatus::Running, attrs: Default::default(), started: None, ended: None, exit_code: None, output: node.text.clone(), expanded: false };
        job.read_attrs(node);
        Some(job)
    }
    /// Copies every attribute; a missing status reads as running.
    fn read_attrs(&mut self, node: &Node) {
        self.kind = node.str_attr(attr::KIND).and_then(JobKind::parse).unwrap_or(self.kind);
        self.status = node.str_attr(attr::STATUS).and_then(JobStatus::parse).unwrap_or(JobStatus::Running);
        self.attrs = ATTRS.map(|a| node.str_attr(a).map(str::to_owned));
        (self.started, self.ended, self.exit_code) = (node.int_attr(attr::STARTED), node.int_attr(attr::ENDED), node.int_attr(attr::EXIT_CODE));
    }
    pub fn node(&self) -> NodeId { self.node }
    pub fn status(&self) -> JobStatus { self.status }
    /// The child session of a subagent.
    pub fn session(&self) -> Option<&str> { self.attrs[4].as_deref() }
    /// Title, else agent, else command, else kind.
    pub fn label(&self) -> &str { [1, 3, 2].iter().find_map(|&i| self.attrs[i].as_deref()).unwrap_or(self.kind.as_str()) }
    pub fn output(&self) -> &str { &self.output }
    /// The last non-blank output line, trimmed.
    pub fn last_line(&self) -> &str { self.output.rsplit('\n').map(str::trim).find(|l| !l.is_empty()).unwrap_or_default() }
    fn running(&self) -> bool { self.status == JobStatus::Running }
}

/// Jobs of one session in document order, the active `wait` calls, and the last tick in unix ms.
#[derive(Debug, Default)]
pub struct State { jobs: BTreeMap<NodeId, Job>, waits: HashSet<NodeId>, now: Option<i64> }
/// `Tick` carries unix ms; `Toggle` shows or hides a job's output; `Detach` opens a subagent's session in a window.
#[derive(Debug, Clone)]
pub enum Message { Tick(i64), Toggle(NodeId), Open(NodeId), Detach(NodeId), Cancel(NodeId) }

impl State {
    pub fn from_dom(dom: &Dom) -> Self {
        let mut s = Self::default();
        s.reset(dom);
        s
    }
    /// Rebuilds from `dom`, keeping rows that were expanded.
    pub fn reset(&mut self, dom: &Dom) {
        let expanded: HashSet<NodeId> = self.jobs.values().filter(|j| j.expanded).map(|j| j.node).collect();
        self.jobs = dom.iter().filter_map(Job::from_node).map(|j| (j.node, Job { expanded: expanded.contains(&j.node), ..j })).collect();
        self.waits = dom.iter().filter(|n| is_active_wait(n)).map(|n| n.id).collect();
    }
    /// Follows `patch`, already applied to `dom`.
    pub fn apply(&mut self, dom: &Dom, patch: &Patch) {
        let node = patch.node;
        match (&patch.op, self.jobs.get_mut(&node), dom.get(node)) {
            (Op::Insert { .. }, _, Some(n)) => match Job::from_node(n) {
                Some(job) => { self.jobs.insert(node, job); }
                None => self.refresh_wait(n),
            },
            (Op::Set { name, .. }, Some(job), Some(n)) if name == TEXT => n.text.clone_into(&mut job.output),
            (Op::Set { .. }, Some(job), Some(n)) => job.read_attrs(n),
            (Op::Set { .. }, None, Some(n)) => self.refresh_wait(n),
            (Op::Append { text }, Some(job), _) => job.output.push_str(text),
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
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<AgentsIntent>) {
        let session = |n: NodeId| self.jobs.get(&n).and_then(Job::session).map(str::to_owned);
        let event = match message {
            Message::Open(n) => session(n).map(AgentsIntent::OpenSession),
            Message::Detach(n) => session(n).map(AgentsIntent::DetachSession),
            Message::Cancel(n) => self.jobs.get(&n).filter(|j| j.running()).map(|j| AgentsIntent::Cancel { node: n, id: j.attrs[0].clone() }),
            Message::Tick(now) => { self.now = Some(now); None }
            Message::Toggle(n) => { self.jobs.entry(n).and_modify(|j| j.expanded = !j.expanded); None }
        };
        (Task::none(), event)
    }
    pub fn jobs(&self) -> impl Iterator<Item = &Job> { self.jobs.values() }
    pub fn job(&self, node: NodeId) -> Option<&Job> { self.jobs.get(&node) }
    /// A `wait` call blocks on the running jobs.
    pub fn waiting(&self) -> bool { !self.waits.is_empty() }
    pub fn is_awaited(&self, node: NodeId) -> bool { self.waiting() && self.jobs.get(&node).is_some_and(Job::running) }
    /// How long the job ran: to `ended`, or to the last tick while it runs.
    pub fn elapsed_ms(&self, node: NodeId) -> Option<i64> {
        let job = self.jobs.get(&node)?;
        let end = job.ended.or(self.now.filter(|_| job.running()))?;
        Some((end - job.started?).max(0))
    }
    pub fn view(&self) -> Element<'_, Message> {
        let running = self.jobs.values().filter(|j| j.running()).count();
        let blocked = (self.waiting() && running > 0).then(|| badge("wait blocked", Tone::Muted));
        let header = row![label(format!("{running} running"), T::Caption, Tone::Muted), blocked].spacing(sp(Sp::S)).align_y(Center).height(list_row_height());
        let empty = self.jobs.is_empty().then(|| label("No subagents or background jobs", T::Label, Tone::Muted));
        let list = Column::new().push(header).push(empty).extend(self.jobs.values().map(|j| self.job_view(j)));
        scrollable(list.spacing(sp(Sp::Xs)).padding([sp(Sp::Xs), sp(Sp::L)])).height(Fill).into()
    }
    fn job_view<'a>(&'a self, job: &'a Job) -> Element<'a, Message> {
        let tone = [Tone::Muted, Tone::Ok, Tone::Danger, Tone::Muted][job.status as usize];
        let exit = job.exit_code.filter(|&c| c != 0 && !job.running()).map(|c| label(format!("exit {c}"), T::Caption, Tone::Danger));
        let (child, n) = (job.kind == JobKind::Subagent && job.session().is_some(), job.node);
        let head = row![
            icon(if job.kind == JobKind::Shell { Icon::Terminal } else { Icon::Cpu }, ts(T::Body)),
            text(job.label()).size(ts(T::Body)).wrapping(Wrapping::None),
            label(job.status.as_str(), T::Caption, tone),
            exit,
            self.is_awaited(n).then(|| badge("waiting on", Tone::Muted)),
            self.elapsed_ms(n).map(|ms| label(format_duration(ms), T::Caption, Tone::Muted)),
            space().width(Fill),
            child.then(|| action("Open", Message::Open(n))),
            child.then(|| action("Window", Message::Detach(n))),
            job.running().then(|| action("Cancel", Message::Cancel(n))),
            (!job.output.is_empty()).then(|| action(if job.expanded { "Collapse" } else { "Expand" }, Message::Toggle(n))),
        ];
        let below: Option<Element<'a, Message>> = if job.expanded {
            Some(container(text(job.output()).font(Font::MONOSPACE).size(ts(T::Label))).padding([sp(Sp::S), sp(Sp::M)]).width(Fill).style(kit::fill(C::Raised, 0.0)).into())
        } else {
            Some(job.last_line()).filter(|l| !l.is_empty()).map(|l| row![space().width(sp(Sp::Xl)), label(l, T::Caption, Tone::Muted).wrapping(Wrapping::None)].into())
        };
        Column::new().push(head.spacing(sp(Sp::S)).align_y(Center).height(list_row_height())).push(below).into()
    }
}

panel_pane!("agents", "Agents", Bot);

/// A pending or running `<call tool="wait">`.
fn is_active_wait(n: &Node) -> bool {
    n.kind == ElementKind::Call && n.str_attr(attr::TOOL) == Some(WAIT_TOOL) && matches!(n.str_attr(attr::STATUS).and_then(CallStatus::parse), Some(CallStatus::Pending | CallStatus::Running))
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{Draft, Value};

    fn commit(dom: &mut Dom, state: &mut State, draft: Draft) -> NodeId {
        let patch = dom.commit(draft, 0).unwrap();
        state.apply(dom, &patch);
        patch.node
    }
    #[test]
    fn patches_track_output_waits_and_events() {
        let (mut dom, mut state) = (Dom::new(), State::default());
        let attrs = [(attr::ID, Value::from("s1")), (attr::HARNESS, "omp".into()), (attr::NODE, "key".into()), (attr::CREATED, 0_i64.into())];
        let session = commit(&mut dom, &mut state, Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""));
        let turn = commit(&mut dom, &mut state, Draft::insert(session, ElementKind::Turn, [(attr::STATUS, Value::from("running"))], ""));
        let shell = [(attr::KIND, Value::from(JobKind::Shell)), (attr::COMMAND, "cargo build".into()), (attr::STARTED, 1_000_i64.into())];
        let a = commit(&mut dom, &mut state, Draft::insert(turn, ElementKind::Job, shell, ""));
        for (chunk, last) in [("ab", "ab"), ("c\nde", "de"), ("f\n", "def"), ("  \n\n", "def")] {
            commit(&mut dom, &mut state, Draft::append(a, chunk));
            assert_eq!(state.job(a).unwrap().last_line(), last);
        }
        let _ = state.update(Message::Tick(66_000));
        let wait = commit(&mut dom, &mut state, Draft::insert(turn, ElementKind::Call, [(attr::TOOL, Value::from(WAIT_TOOL)), (attr::STATUS, "pending".into())], ""));
        assert_eq!((state.job(a).unwrap().label(), state.elapsed_ms(a), state.is_awaited(a)), ("cargo build", Some(65_000), true));
        assert_eq!(state.update(Message::Cancel(a)).1, Some(AgentsIntent::Cancel { node: a, id: None }));
        commit(&mut dom, &mut state, Draft::set(wait, attr::STATUS, CallStatus::Done));
        commit(&mut dom, &mut state, Draft::set(a, attr::STATUS, JobStatus::Done));
        assert_eq!((state.waiting(), state.update(Message::Cancel(a)).1, state.update(Message::Open(a)).1), (false, None, None));
        assert!(state.jobs().eq(State::from_dom(&dom).jobs()), "incremental equals a fresh scan");
    }
}
