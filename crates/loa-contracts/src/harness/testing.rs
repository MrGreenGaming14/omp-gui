//! Adapter-test checks of the DOM conventions in `docs/ARCHITECTURE.md`.

use loa_core::{
    CallStatus, CommitError, DecisionStatus, Dom, Draft, ElementKind, JobStatus, MemorySink, MessageStatus, Node, NodeId,
    Op, Patch, PatchSink, Role, TurnStatus, Value, attr,
};
use tokio::sync::watch;

/// The convention a [`Violation`] breaks.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[rustfmt::skip]
pub enum Rule {
    Root, MissingStatus, NotTerminal, OpenTurn, TurnShape, CallInput, CallResult,
    Waiting, DecisionAnswer, DanglingRef, Transition, CloseOrder, Replay,
}

/// One broken rule and the node it was found on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Violation {
    pub rule: Rule,
    pub node: Option<NodeId>,
    pub detail: String,
}

impl Violation {
    pub fn new(rule: Rule, node: impl Into<Option<NodeId>>, detail: impl Into<String>) -> Self {
        Self { rule, node: node.into(), detail: detail.into() }
    }
}

/// `Settled`: everything has ended. `Live`: the harness may be mid event.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Phase {
    Settled,
    Live,
}

/// Checks a settled DOM; empty when every convention holds.
pub fn check_dom(dom: &Dom) -> Vec<Violation> {
    check_dom_in(dom, Phase::Settled)
}

/// Checks `dom` as seen at `phase`.
pub fn check_dom_in(dom: &Dom, phase: Phase) -> Vec<Violation> {
    let mut out = Vec::new();
    let Some(root) = dom.root().filter(|r| r.kind == ElementKind::Session) else {
        out.push(Violation::new(Rule::Root, None, "the root isn't a <session>"));
        return out;
    };
    let settled = phase == Phase::Settled;
    let turns: Vec<&Node> = dom.children_of_kind(root.id, ElementKind::Turn).collect();
    for turn in turns.iter().filter(|t| is_open(t)) {
        if !settled && Some(turn.id) != turns.last().map(|t| t.id) {
            out.push(Violation::new(Rule::OpenTurn, turn.id, "an open turn isn't the session's last turn"));
        }
    }
    for node in dom.iter() {
        if settled && is_open(node) {
            out.push(Violation::new(Rule::NotTerminal, node.id, format!("<{}> is open in a settled DOM", node.kind)));
        }
        if !status_valid(node) {
            let status = node.str_attr(attr::STATUS);
            out.push(Violation::new(Rule::MissingStatus, node.id, format!("<{}> status {status:?}", node.kind)));
        }
        match node.kind {
            ElementKind::Turn => check_turn(dom, node, settled, &mut out),
            ElementKind::Call => check_call(dom, node, settled, &mut out),
            ElementKind::Decision => check_decision(dom, node, settled, &mut out),
            ElementKind::Job => check_ref(dom, node, attr::CALL, ElementKind::Call, &mut out),
            ElementKind::Compaction | ElementKind::Digest => {
                let name = if node.kind == ElementKind::Compaction { attr::COVERS } else { attr::POINTERS };
                for value in node.attr(name).and_then(Value::as_list).unwrap_or_default() {
                    if let Err(why) = resolves(dom, value, ElementKind::Turn) {
                        out.push(Violation::new(Rule::DanglingRef, node.id, format!("`{name}`: {why}")));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Replays `patches` onto an empty DOM, checking status transitions; stops at the first failure.
pub fn check_history(patches: &[Patch]) -> Vec<Violation> {
    let mut out = Vec::new();
    let mut dom = Dom::new();
    for patch in patches {
        if let Op::Set { name, value } = &patch.op
            && name == attr::STATUS
            && let Some(node) = dom.get(patch.node)
        {
            check_set(&dom, node, value.as_ref().and_then(Value::as_str), patch.seq, &mut out);
        }
        if let Err(err) = dom.apply(patch) {
            out.push(Violation::new(Rule::Replay, patch.node, format!("patch {} doesn't apply: {err}", patch.seq)));
            break;
        }
    }
    out
}

fn check_turn(dom: &Dom, turn: &Node, settled: bool, out: &mut Vec<Violation>) {
    match dom.children(turn.id).find(|n| matches!(n.kind, ElementKind::Message | ElementKind::Call)) {
        Some(n) if n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == Some(Role::User.as_str()) => {
            if n.str_attr(attr::STATUS) != Some(MessageStatus::Done.as_str()) {
                out.push(Violation::new(Rule::TurnShape, n.id, "the user message isn't status=\"done\""));
            }
        }
        Some(n) => out.push(Violation::new(Rule::TurnShape, n.id, format!("the turn starts with <{}>", n.kind))),
        None => out.push(Violation::new(Rule::TurnShape, turn.id, "the turn has no user message")),
    }
    let status = turn.str_attr(attr::STATUS).and_then(TurnStatus::parse);
    if !settled && matches!(status, Some(TurnStatus::Done | TurnStatus::Error | TurnStatus::Cancelled)) {
        for node in dom.descendants(turn.id).skip(1).filter(|n| is_open(n)) {
            out.push(Violation::new(Rule::NotTerminal, node.id, format!("<{}> is open under an ended turn", node.kind)));
        }
    }
    if settled {
        let waiting = dom
            .descendants(turn.id)
            .any(|n| n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str()));
        if (status == Some(TurnStatus::Waiting)) != waiting {
            out.push(Violation::new(Rule::Waiting, turn.id, "turn and decision disagree on waiting"));
        }
    }
}

fn check_call(dom: &Dom, call: &Node, settled: bool, out: &mut Vec<Violation>) {
    let status = call.str_attr(attr::STATUS).and_then(CallStatus::parse);
    let open = matches!(status, Some(CallStatus::Pending | CallStatus::Running));
    if settled || !open {
        let inputs: Vec<&Node> = dom.children_of_kind(call.id, ElementKind::Input).collect();
        match inputs.as_slice() {
            [input] => {
                let json = input.str_attr(attr::FORMAT).is_none_or(|f| f == "json");
                if json && let Err(err) = serde_json::from_str::<serde_json::Value>(&input.text) {
                    out.push(Violation::new(Rule::CallInput, input.id, format!("the arguments aren't JSON: {err}")));
                }
            }
            _ => out.push(Violation::new(Rule::CallInput, call.id, format!("the call has {} <input>s", inputs.len()))),
        }
    }
    let results: Vec<&Node> = dom.children_of_kind(call.id, ElementKind::Result).collect();
    let is_error = results.first().and_then(|r| r.bool_attr(attr::IS_ERROR));
    let bad = match status {
        _ if results.len() > 1 => Some("the call has several <result>s"),
        Some(CallStatus::Done | CallStatus::Error) if results.is_empty() => Some("the call ended without a <result>"),
        Some(CallStatus::Done) if is_error == Some(true) => Some("status=\"done\" with an is_error result"),
        Some(CallStatus::Error) if is_error == Some(false) => Some("status=\"error\" with a result that isn't is_error"),
        _ => None,
    };
    if let Some(why) = bad {
        out.push(Violation::new(Rule::CallResult, call.id, why));
    }
}

fn check_decision(dom: &Dom, node: &Node, settled: bool, out: &mut Vec<Violation>) {
    check_ref(dom, node, attr::CALL, ElementKind::Call, out);
    if let (Some(call), Some(parent)) = (node.int_attr(attr::CALL), dom.get(node.parent))
        && parent.kind == ElementKind::Call
        && u64::try_from(call).ok() != Some(parent.id.0)
    {
        out.push(Violation::new(Rule::DanglingRef, node.id, format!("names call {call} but sits under {}", parent.id)));
    }
    let status = node.str_attr(attr::STATUS).and_then(DecisionStatus::parse);
    let waiting_turn = || {
        let turn = dom.ancestors(node.id).find(|a| a.kind == ElementKind::Turn);
        turn.is_some_and(|t| t.str_attr(attr::STATUS) == Some(TurnStatus::Waiting.as_str()))
    };
    if settled && status == Some(DecisionStatus::Waiting) && !waiting_turn() {
        out.push(Violation::new(Rule::Waiting, node.id, "a waiting decision has no waiting turn"));
    }
    match (status, node.attr(attr::ANSWER).is_some()) {
        (Some(DecisionStatus::Waiting), true) => {
            out.push(Violation::new(Rule::DecisionAnswer, node.id, "a waiting decision already has an answer"));
        }
        (Some(DecisionStatus::Answered), false) => {
            out.push(Violation::new(Rule::DecisionAnswer, node.id, "an answered decision has no answer"));
        }
        _ => {}
    }
}

fn check_ref(dom: &Dom, node: &Node, name: &str, want: ElementKind, out: &mut Vec<Violation>) {
    if let Some(value) = node.attr(name)
        && let Err(why) = resolves(dom, value, want)
    {
        out.push(Violation::new(Rule::DanglingRef, node.id, format!("`{name}`: {why}")));
    }
}

fn resolves(dom: &Dom, value: &Value, want: ElementKind) -> Result<(), String> {
    let id = value.as_int().and_then(|v| u64::try_from(v).ok()).ok_or_else(|| format!("{value:?} isn't a node id"))?;
    match dom.get(NodeId(id)) {
        Some(n) if n.kind == want => Ok(()),
        Some(n) => Err(format!("node {id} is <{}>, not <{want}>", n.kind)),
        None => Err(format!("node {id} doesn't exist")),
    }
}

fn status_valid(node: &Node) -> bool {
    let status = node.str_attr(attr::STATUS);
    match node.kind {
        ElementKind::Turn => status.and_then(TurnStatus::parse).is_some(),
        ElementKind::Message => status.and_then(MessageStatus::parse).is_some(),
        ElementKind::Call => status.and_then(CallStatus::parse).is_some(),
        ElementKind::Decision => status.and_then(DecisionStatus::parse).is_some(),
        ElementKind::Job => status.and_then(JobStatus::parse).is_some(),
        _ => true,
    }
}

/// A turn, message, call or decision whose status isn't terminal.
fn is_open(node: &Node) -> bool {
    let status = node.str_attr(attr::STATUS);
    match node.kind {
        ElementKind::Turn => matches!(status.and_then(TurnStatus::parse), Some(TurnStatus::Running | TurnStatus::Waiting)),
        ElementKind::Message => status == Some(MessageStatus::Streaming.as_str()),
        ElementKind::Call => matches!(status.and_then(CallStatus::parse), Some(CallStatus::Pending | CallStatus::Running)),
        ElementKind::Decision => status == Some(DecisionStatus::Waiting.as_str()),
        _ => false,
    }
}

/// Whether a status machine allows `from` to become `to`.
fn transition_allowed(kind: ElementKind, from: &str, to: &str) -> bool {
    use CallStatus as C;
    use TurnStatus as T;
    if from == to {
        return true;
    }
    match kind {
        ElementKind::Turn => matches!(
            (T::parse(from), T::parse(to)),
            (Some(T::Running), Some(T::Waiting | T::Done | T::Error | T::Cancelled))
                | (Some(T::Waiting), Some(T::Running | T::Error | T::Cancelled))
        ),
        ElementKind::Call => matches!(
            (C::parse(from), C::parse(to)),
            (Some(C::Pending), Some(C::Running | C::Done | C::Error | C::Cancelled))
                | (Some(C::Running), Some(C::Done | C::Error | C::Cancelled))
        ),
        ElementKind::Message => from == MessageStatus::Streaming.as_str(),
        ElementKind::Decision => from == DecisionStatus::Waiting.as_str(),
        ElementKind::Job => from == JobStatus::Running.as_str(),
        _ => true,
    }
}

fn check_set(dom: &Dom, node: &Node, to: Option<&str>, seq: u64, out: &mut Vec<Violation>) {
    let Some(to) = to else {
        let machine = [ElementKind::Turn, ElementKind::Message, ElementKind::Call, ElementKind::Decision, ElementKind::Job];
        if machine.contains(&node.kind) {
            out.push(Violation::new(Rule::Transition, node.id, format!("patch {seq} clears the <{}> status", node.kind)));
        }
        return;
    };
    if let Some(from) = node.str_attr(attr::STATUS)
        && !transition_allowed(node.kind, from, to)
    {
        out.push(Violation::new(Rule::Transition, node.id, format!("patch {seq} moves <{}> from {from} to {to}", node.kind)));
    }
    let ends_turn = matches!(TurnStatus::parse(to), Some(TurnStatus::Done | TurnStatus::Error | TurnStatus::Cancelled));
    if node.kind == ElementKind::Turn && ends_turn {
        for open in dom.descendants(node.id).skip(1).filter(|n| is_open(n)) {
            out.push(Violation::new(Rule::CloseOrder, open.id, format!("patch {seq} ends the turn while this is open")));
        }
    }
    if node.kind == ElementKind::Decision && to == DecisionStatus::Answered.as_str() && node.attr(attr::ANSWER).is_none() {
        out.push(Violation::new(Rule::CloseOrder, node.id, format!("patch {seq} answers before the answer is set")));
    }
}

/// A [`MemorySink`] that announces every commit's seq.
#[derive(Default)]
pub struct WatchedSink {
    pub inner: MemorySink,
    pub seq: watch::Sender<u64>,
}

impl PatchSink for WatchedSink {
    fn commit_at(&self, draft: Draft, ts: i64) -> Result<Patch, CommitError> {
        let patch = self.inner.commit_at(draft, ts)?;
        self.seq.send_replace(patch.seq);
        Ok(patch)
    }
}
