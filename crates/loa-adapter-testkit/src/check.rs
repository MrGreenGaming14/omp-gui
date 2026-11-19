//! DOM invariants from `docs/ARCHITECTURE.md`, "DOM conventions for adapters".
//!
//! [`check_dom`] runs on any DOM an adapter produced: an import, or a live
//! session once it is idle. [`check_dom_in`] with [`Phase::Live`] runs while
//! a turn may still be in flight. [`check_history`] replays the patches that
//! built a DOM and checks the status machines and the order the conventions
//! fix (children close before their turn, an answer before `answered`).

use crate::violation::{Rule, Violation};
use loa_core::{
    CallStatus, DecisionKind, DecisionStatus, Dom, ElementKind, JobStatus, MessageKind, MessageStatus, Node, NodeId,
    Op, Patch, Role, SCHEMA_VERSION, TurnStatus, Value, answer, attr,
};
use std::collections::HashMap;

/// When a DOM is checked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Phase {
    /// Nothing runs: an import, or a session after `wait_idle`. Every turn,
    /// message, call and decision has reached a terminal status.
    Settled,
    /// The harness may be writing. The last turn may be open, and rules that
    /// break for a moment between two patches of one event (a call before its
    /// `<input>`, a waiting decision before its turn is set waiting) are skipped.
    Live,
}

/// Checks a settled DOM. Empty when every convention holds.
pub fn check_dom(dom: &Dom) -> Vec<Violation> {
    check_dom_in(dom, Phase::Settled)
}

/// Checks `dom` as seen at `phase`.
pub fn check_dom_in(dom: &Dom, phase: Phase) -> Vec<Violation> {
    let mut out = Vec::new();
    let Some(root) = dom.root() else {
        out.push(Violation::new(Rule::Root, None, "the document has no root"));
        return out;
    };
    if root.kind != ElementKind::Session {
        out.push(Violation::new(Rule::Root, root.id, format!("the root is <{}>, not <session>", root.kind)));
        return out;
    }
    check_session(root, &mut out);
    check_turn_order(dom, root, phase, &mut out);

    let mut call_ids: HashMap<&str, NodeId> = HashMap::new();
    let mut request_ids: HashMap<&str, NodeId> = HashMap::new();
    let mut turn_ids: HashMap<&str, NodeId> = HashMap::new();
    for node in dom.iter() {
        check_times(node, &mut out);
        if phase == Phase::Settled && is_open(node) {
            out.push(Violation::new(
                Rule::NotTerminal,
                node.id,
                format!("<{}> is still {} in a settled DOM", node.kind, status_of(node).unwrap_or_default()),
            ));
        }
        match node.kind {
            ElementKind::Turn => {
                unique(&mut turn_ids, node, attr::ID, &mut out);
                check_turn(dom, node, phase, &mut out);
            }
            ElementKind::Message => check_message(node, &mut out),
            ElementKind::Call => {
                unique(&mut call_ids, node, attr::CALL_ID, &mut out);
                check_call(dom, node, phase, &mut out);
            }
            ElementKind::Decision => {
                unique(&mut request_ids, node, attr::REQUEST_ID, &mut out);
                check_decision(dom, node, phase, &mut out);
            }
            ElementKind::Job => {
                require_status(node, &mut out);
                check_ref(dom, node, attr::CALL, ElementKind::Call, &mut out);
            }
            ElementKind::Todo if node.str_attr(attr::STATE).is_none() => {
                out.push(Violation::new(Rule::MissingStatus, node.id, "<todo> has no state"));
            }
            ElementKind::Compaction => check_ref_list(dom, node, attr::COVERS, &mut out),
            ElementKind::Digest => check_ref_list(dom, node, attr::POINTERS, &mut out),
            _ => {}
        }
    }
    out
}

/// Replays `patches` onto an empty DOM and checks every status change
/// against the status machines. Stops at the first patch that doesn't apply.
pub fn check_history(patches: &[Patch]) -> Vec<Violation> {
    let mut out = Vec::new();
    let mut dom = Dom::new();
    for patch in patches {
        if let Op::Set { name, value } = &patch.op
            && let Some(node) = dom.get(patch.node)
        {
            check_set(&dom, node, name, value.as_ref(), patch.seq, &mut out);
        }
        if let Err(err) = dom.apply(patch) {
            out.push(Violation::new(Rule::Replay, patch.node, format!("patch {} doesn't apply: {err}", patch.seq)));
            break;
        }
    }
    out
}

fn check_session(root: &Node, out: &mut Vec<Violation>) {
    for name in [attr::ID, attr::HARNESS, attr::NODE] {
        if root.str_attr(name).is_none_or(str::is_empty) {
            out.push(Violation::new(Rule::SessionAttrs, root.id, format!("<session {name}> is missing or empty")));
        }
    }
    if root.int_attr(attr::CREATED).is_none() {
        out.push(Violation::new(Rule::SessionAttrs, root.id, "<session created> is missing"));
    }
    match root.int_attr(attr::SCHEMA) {
        Some(v) if v == i64::from(SCHEMA_VERSION) => {}
        Some(v) => out.push(Violation::new(
            Rule::SchemaVersion,
            root.id,
            format!("<session schema={v}>, the schema is {SCHEMA_VERSION}"),
        )),
        None => out.push(Violation::new(Rule::SchemaVersion, root.id, "<session schema> is missing")),
    }
}

/// At most one open turn, and only the last one.
fn check_turn_order(dom: &Dom, root: &Node, phase: Phase, out: &mut Vec<Violation>) {
    if phase == Phase::Settled {
        return;
    }
    let turns: Vec<&Node> = dom.children_of_kind(root.id, ElementKind::Turn).collect();
    let last = turns.last().map(|t| t.id);
    for turn in turns.iter().filter(|t| is_open(t)) {
        if Some(turn.id) != last {
            out.push(Violation::new(Rule::OpenTurn, turn.id, "an open turn isn't the session's last turn"));
        }
    }
}

fn check_turn(dom: &Dom, turn: &Node, phase: Phase, out: &mut Vec<Violation>) {
    require_status(turn, out);
    let first = dom.children(turn.id).find(|n| matches!(n.kind, ElementKind::Message | ElementKind::Call));
    match first {
        None => out.push(Violation::new(Rule::TurnShape, turn.id, "the turn has no user message")),
        Some(n) if n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == Some(Role::User.as_str()) => {
            if n.str_attr(attr::STATUS) != Some(MessageStatus::Done.as_str()) {
                out.push(Violation::new(Rule::TurnShape, n.id, "the user message isn't status=\"done\""));
            }
        }
        Some(n) => out.push(Violation::new(
            Rule::TurnShape,
            n.id,
            format!("the turn starts with <{}> instead of the user message", n.kind),
        )),
    }

    let status = turn.str_attr(attr::STATUS).and_then(TurnStatus::parse);
    let ended = matches!(status, Some(TurnStatus::Done | TurnStatus::Error | TurnStatus::Cancelled));
    if ended && phase == Phase::Live {
        for node in dom.descendants(turn.id).skip(1).filter(|n| is_open(n)) {
            out.push(Violation::new(
                Rule::NotTerminal,
                node.id,
                format!(
                    "<{}> is still {} under a turn that is {}",
                    node.kind,
                    status_of(node).unwrap_or_default(),
                    status_of(turn).unwrap_or_default()
                ),
            ));
        }
    }
    if phase == Phase::Settled {
        let waiting = dom
            .descendants(turn.id)
            .any(|n| n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str()));
        match (status == Some(TurnStatus::Waiting), waiting) {
            (true, false) => out.push(Violation::new(Rule::Waiting, turn.id, "the turn is waiting on no decision")),
            (false, true) => out.push(Violation::new(
                Rule::Waiting,
                turn.id,
                "a decision under the turn is waiting but the turn isn't",
            )),
            _ => {}
        }
    }
}

fn check_message(node: &Node, out: &mut Vec<Violation>) {
    require_status(node, out);
    let thinking = node.str_attr(attr::KIND) == Some(MessageKind::Thinking.as_str());
    if thinking && node.str_attr(attr::ROLE) != Some(Role::Assistant.as_str()) {
        out.push(Violation::new(Rule::MessageShape, node.id, "only assistant messages can be thinking"));
    }
}

fn check_call(dom: &Dom, call: &Node, phase: Phase, out: &mut Vec<Violation>) {
    require_status(call, out);
    let status = call.str_attr(attr::STATUS).and_then(CallStatus::parse);
    let open = matches!(status, Some(CallStatus::Pending | CallStatus::Running));
    let inputs: Vec<&Node> = dom.children_of_kind(call.id, ElementKind::Input).collect();
    // A live call may sit between its insert and its `<input>` insert, and its
    // arguments may still be streaming.
    if phase == Phase::Settled || !open {
        match inputs.as_slice() {
            [] => out.push(Violation::new(Rule::CallInput, call.id, "the call has no <input>")),
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
    if results.len() > 1 {
        out.push(Violation::new(Rule::CallResult, call.id, format!("the call has {} <result>s", results.len())));
    }
    let is_error = results.first().and_then(|r| r.bool_attr(attr::IS_ERROR));
    match status {
        Some(CallStatus::Done | CallStatus::Error) if results.is_empty() => {
            out.push(Violation::new(Rule::CallResult, call.id, "the call ended without a <result>"));
        }
        Some(CallStatus::Done) if is_error == Some(true) => {
            out.push(Violation::new(Rule::CallResult, call.id, "status=\"done\" with an is_error result"));
        }
        Some(CallStatus::Error) if is_error == Some(false) => {
            out.push(Violation::new(Rule::CallResult, call.id, "status=\"error\" with a result that isn't is_error"));
        }
        _ => {}
    }
}

fn check_decision(dom: &Dom, node: &Node, phase: Phase, out: &mut Vec<Violation>) {
    require_status(node, out);
    check_ref(dom, node, attr::CALL, ElementKind::Call, out);
    if let (Some(call), Some(parent)) = (node.int_attr(attr::CALL), dom.get(node.parent))
        && parent.kind == ElementKind::Call
        && u64::try_from(call).ok() != Some(parent.id.0)
    {
        out.push(Violation::new(
            Rule::DanglingRef,
            node.id,
            format!("the decision names call {call} but sits under call {}", parent.id),
        ));
    }

    let status = node.str_attr(attr::STATUS).and_then(DecisionStatus::parse);
    if phase == Phase::Settled && status == Some(DecisionStatus::Waiting)
        && dom.ancestors(node.id).find(|ancestor| ancestor.kind == ElementKind::Turn).is_none_or(|turn| {
            turn.str_attr(attr::STATUS) != Some(TurnStatus::Waiting.as_str())
        })
    {
        out.push(Violation::new(Rule::Waiting, node.id, "a waiting decision has no waiting turn"));
    }
    let values: Option<Vec<&str>> = node.attr(attr::ANSWER).map(|v| v.str_items().collect());
    match (status, &values) {
        (Some(DecisionStatus::Waiting), Some(_)) => {
            out.push(Violation::new(Rule::DecisionAnswer, node.id, "a waiting decision already has an answer"));
        }
        (Some(DecisionStatus::Answered), None) => {
            out.push(Violation::new(Rule::DecisionAnswer, node.id, "an answered decision has no answer"));
        }
        (Some(DecisionStatus::Answered), Some(values)) => {
            let kind = node.str_attr(attr::KIND).and_then(DecisionKind::parse);
            let multi = node.bool_attr(attr::MULTI) == Some(true);
            if let Err(why) = answer_fits(kind, multi, values) {
                out.push(Violation::new(Rule::DecisionAnswer, node.id, why));
            }
        }
        _ => {}
    }
}

/// Checks answer values against the decision kind (see `loa_core::answer`).
fn answer_fits(kind: Option<DecisionKind>, multi: bool, values: &[&str]) -> Result<(), String> {
    let one_of = |allowed: &[&str]| match values {
        [v] if allowed.contains(v) => Ok(()),
        _ => Err(format!("answer {values:?} isn't one of {allowed:?}")),
    };
    match kind {
        Some(DecisionKind::Approval) => one_of(&[answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY]),
        Some(DecisionKind::Plan) => one_of(&[answer::APPROVE, answer::EDIT, answer::REJECT]),
        Some(DecisionKind::Form) => match values {
            [v] if serde_json::from_str::<serde_json::Value>(v).is_ok_and(|j| j.is_object()) => Ok(()),
            _ => Err(format!("a form answer must be one JSON object, got {values:?}")),
        },
        Some(DecisionKind::Question) => match values.len() {
            0 => Err("a question answer is empty".to_owned()),
            1 => Ok(()),
            _ if multi => Ok(()),
            n => Err(format!("{n} answers to a question that isn't multi")),
        },
        None => Ok(()),
    }
}

fn check_ref(dom: &Dom, node: &Node, name: &str, want: ElementKind, out: &mut Vec<Violation>) {
    let Some(value) = node.attr(name) else { return };
    if let Err(why) = resolves(dom, value, want) {
        out.push(Violation::new(Rule::DanglingRef, node.id, format!("`{name}`: {why}")));
    }
}

fn check_ref_list(dom: &Dom, node: &Node, name: &str, out: &mut Vec<Violation>) {
    for value in node.attr(name).and_then(Value::as_list).unwrap_or_default() {
        if let Err(why) = resolves(dom, value, ElementKind::Turn) {
            out.push(Violation::new(Rule::DanglingRef, node.id, format!("`{name}`: {why}")));
        }
    }
}

fn resolves(dom: &Dom, value: &Value, want: ElementKind) -> Result<(), String> {
    let id = value
        .as_int()
        .and_then(|v| u64::try_from(v).ok())
        .ok_or_else(|| format!("{value:?} isn't a node id"))?;
    match dom.get(NodeId(id)) {
        Some(n) if n.kind == want => Ok(()),
        Some(n) => Err(format!("node {id} is <{}>, not <{want}>", n.kind)),
        None => Err(format!("node {id} doesn't exist")),
    }
}

fn unique<'a>(seen: &mut HashMap<&'a str, NodeId>, node: &'a Node, name: &str, out: &mut Vec<Violation>) {
    let Some(id) = node.str_attr(name) else { return };
    if let Some(first) = seen.insert(id, node.id) {
        out.push(Violation::new(
            Rule::DuplicateId,
            node.id,
            format!("<{} {name}=\"{id}\"> is also on node {first}", node.kind),
        ));
    }
}

fn check_times(node: &Node, out: &mut Vec<Violation>) {
    let start = node.int_attr(attr::STARTED).or_else(|| node.int_attr(attr::CREATED));
    if let (Some(start), Some(end)) = (start, node.int_attr(attr::ENDED))
        && end < start
    {
        out.push(Violation::new(Rule::Timestamps, node.id, format!("ended {end} is before start {start}")));
    }
}

fn require_status(node: &Node, out: &mut Vec<Violation>) {
    let status = node.str_attr(attr::STATUS);
    let valid = match node.kind {
        ElementKind::Turn => status.is_some_and(|s| TurnStatus::parse(s).is_some()),
        ElementKind::Message => status.is_some_and(|s| MessageStatus::parse(s).is_some()),
        ElementKind::Call => status.is_some_and(|s| CallStatus::parse(s).is_some()),
        ElementKind::Decision => status.is_some_and(|s| DecisionStatus::parse(s).is_some()),
        ElementKind::Job => status.is_some_and(|s| JobStatus::parse(s).is_some()),
        _ => true,
    };
    if !valid {
        out.push(Violation::new(
            Rule::MissingStatus,
            node.id,
            format!("<{}> has missing or invalid status {status:?}", node.kind),
        ));
    }
}

fn status_of(node: &Node) -> Option<&str> {
    node.str_attr(attr::STATUS)
}

/// A turn, message, call or decision whose status isn't terminal.
pub(crate) fn is_open(node: &Node) -> bool {
    let status = node.str_attr(attr::STATUS);
    match node.kind {
        ElementKind::Turn => {
            matches!(status.and_then(TurnStatus::parse), Some(TurnStatus::Running | TurnStatus::Waiting))
        }
        ElementKind::Message => status == Some(MessageStatus::Streaming.as_str()),
        ElementKind::Call => {
            matches!(status.and_then(CallStatus::parse), Some(CallStatus::Pending | CallStatus::Running))
        }
        ElementKind::Decision => status == Some(DecisionStatus::Waiting.as_str()),
        _ => false,
    }
}

/// Whether a status machine allows `from` to become `to`. Setting the same
/// status again is allowed.
fn transition_allowed(kind: ElementKind, from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    match kind {
        ElementKind::Turn => {
            use TurnStatus::*;
            matches!(
                (TurnStatus::parse(from), TurnStatus::parse(to)),
                (Some(Running), Some(Waiting | Done | Error | Cancelled)) | (Some(Waiting), Some(Running | Error | Cancelled))
            )
        }
        ElementKind::Message => {
            use MessageStatus::*;
            matches!(
                (MessageStatus::parse(from), MessageStatus::parse(to)),
                (Some(Streaming), Some(Done | Error | Cancelled))
            )
        }
        ElementKind::Call => {
            use CallStatus::*;
            matches!(
                (CallStatus::parse(from), CallStatus::parse(to)),
                (Some(Pending), Some(Running | Done | Error | Cancelled)) | (Some(Running), Some(Done | Error | Cancelled))
            )
        }
        ElementKind::Decision => {
            use DecisionStatus::*;
            matches!(
                (DecisionStatus::parse(from), DecisionStatus::parse(to)),
                (Some(Waiting), Some(Answered | Expired | Cancelled))
            )
        }
        ElementKind::Job => {
            use JobStatus::*;
            matches!((JobStatus::parse(from), JobStatus::parse(to)), (Some(Running), Some(Done | Error | Cancelled)))
        }
        _ => true,
    }
}

fn check_set(dom: &Dom, node: &Node, name: &str, value: Option<&Value>, seq: u64, out: &mut Vec<Violation>) {
    if name != attr::STATUS {
        return;
    }
    let from = node.str_attr(attr::STATUS);
    let to = value.and_then(Value::as_str);
    match (from, to) {
        (_, None) if matches!(node.kind, ElementKind::Turn | ElementKind::Message | ElementKind::Call | ElementKind::Decision | ElementKind::Job) => {
            out.push(Violation::new(Rule::Transition, node.id, format!("patch {seq} clears the <{}> status", node.kind)));
            return;
        }
        (Some(from), Some(to)) if !transition_allowed(node.kind, from, to) => {
            out.push(Violation::new(
                Rule::Transition,
                node.id,
                format!("patch {seq} moves <{}> from {from} to {to}", node.kind),
            ));
        }
        _ => {}
    }
    let Some(to) = to else { return };
    match node.kind {
        ElementKind::Turn
            if matches!(TurnStatus::parse(to), Some(TurnStatus::Done | TurnStatus::Error | TurnStatus::Cancelled)) =>
        {
            for open in dom.descendants(node.id).skip(1).filter(|n| is_open(n)) {
                out.push(Violation::new(
                    Rule::CloseOrder,
                    open.id,
                    format!("patch {seq} ends turn {} while this <{}> is still open", node.id, open.kind),
                ));
            }
        }
        ElementKind::Decision if to == DecisionStatus::Answered.as_str() && node.attr(attr::ANSWER).is_none() => {
            out.push(Violation::new(
                Rule::CloseOrder,
                node.id,
                format!("patch {seq} sets status=\"answered\" before the answer"),
            ));
        }
        _ => {}
    }
}
