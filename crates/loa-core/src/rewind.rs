//! Rewind plans between two states of one session: which live work to cancel
//! or resume, and how to reach the target (drafts, else a fork). An id whose
//! kind or parent differs between the two is treated as two different elements.

use crate::schema::TEXT;
use crate::{
    CallStatus, DecisionStatus, Dom, Draft, ElementKind, JobStatus, MessageStatus, Node, NodeId, TurnStatus, Value,
    attr,
};
use std::collections::{BTreeSet, HashSet};

/// Why drafts can't move one DOM to the other.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum Inexpressible {
    /// Ids are never reused, so a node only in the target can't be inserted again.
    #[error("node {0} exists only in the target state and ids are never reused")]
    Added(NodeId),
    /// An `<ext>` attribute named like the reserved text name can't be `set`.
    #[error("node {0} has an attribute named `{TEXT}`, which a set patch can't write")]
    TextAttr(NodeId),
}

/// What the app does to rewind a session from its current DOM to a target DOM.
#[derive(Clone, Debug, PartialEq)]
pub struct RewindPlan {
    /// Live now and not live in the target: stop them, deepest first.
    pub cancel: Vec<NodeId>,
    /// Live in the target and not live now: restart them, parents first.
    pub resume: Vec<NodeId>,
    pub apply: Apply,
}

/// How a rewind moves the session DOM to the target state.
#[derive(Clone, Debug, PartialEq)]
pub enum Apply {
    /// Commit these drafts, in order, to the current session.
    Drafts(Vec<Draft>),
    /// Fork the session's journal at the target's `seq` and continue there.
    Fork { seq: u64, why: Inexpressible },
}

/// Plans a rewind from `current` to `target`, an earlier state of the same session.
pub fn plan(current: &Dom, target: &Dom) -> RewindPlan {
    let live_only_in = |dom: &Dom, other: &Dom| -> Vec<NodeId> {
        dom.iter().filter(|n| is_live(n) && !other.get(n.id).is_some_and(is_live)).map(|n| n.id).collect()
    };
    let mut cancel = live_only_in(current, target);
    cancel.reverse();
    let apply = match drafts(current, target) {
        Ok(drafts) => Apply::Drafts(drafts),
        Err(why) => Apply::Fork { seq: target.seq(), why },
    };
    RewindPlan { cancel, resume: live_only_in(target, current), apply }
}

/// Drafts that turn `old` into `new`: removes of the removed subtrees, then
/// sets and appends on changed nodes, each in `old`'s document order.
pub fn drafts(old: &Dom, new: &Dom) -> Result<Vec<Draft>, Inexpressible> {
    let same = |a: &Node, b: &Node| a.kind == b.kind && a.parent == b.parent;
    if let Some(added) = new.iter().find(|n| !old.get(n.id).is_some_and(|o| same(o, n))) {
        return Err(Inexpressible::Added(added.id));
    }
    let (mut removed, mut out, mut sets) = (HashSet::new(), Vec::new(), Vec::new());
    for o in old.iter() {
        let Some(n) = new.get(o.id).filter(|n| same(o, n)) else {
            // Parents come first in document order, so only tops are drafted.
            if !removed.contains(&o.parent) {
                out.push(Draft::remove(o.id));
            }
            removed.insert(o.id);
            continue;
        };
        let names: BTreeSet<&String> = o.attrs.keys().chain(n.attrs.keys()).collect();
        for name in names {
            let (was, now) = (o.attrs.get(name), n.attrs.get(name));
            if matches!((was, now), (Some(x), Some(y)) if same_value(x, y)) {
                continue;
            }
            if name == TEXT {
                return Err(Inexpressible::TextAttr(o.id));
            }
            sets.push(Draft::Set { node: o.id, name: name.clone(), value: now.cloned() });
        }
        if o.text != n.text {
            sets.push(match n.text.strip_prefix(o.text.as_str()) {
                Some(tail) if !o.text.is_empty() => Draft::append(o.id, tail),
                _ => Draft::set_text(o.id, n.text.as_str()),
            });
        }
    }
    out.extend(sets);
    Ok(out)
}

/// Value equality where a float equals itself bit for bit, so `NaN` isn't a change.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::List(x), Value::List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_value(x, y)),
        _ => a == b,
    }
}

/// True for work in progress a rewind has to stop or restart: running or
/// waiting turns, streaming messages, pending or running calls, running jobs
/// and waiting decisions.
pub fn is_live(node: &Node) -> bool {
    let Some(status) = node.str_attr(attr::STATUS) else { return false };
    match node.kind {
        ElementKind::Turn => matches!(TurnStatus::parse(status), Some(TurnStatus::Running | TurnStatus::Waiting)),
        ElementKind::Message => MessageStatus::parse(status) == Some(MessageStatus::Streaming),
        ElementKind::Call => matches!(CallStatus::parse(status), Some(CallStatus::Pending | CallStatus::Running)),
        ElementKind::Job => JobStatus::parse(status) == Some(JobStatus::Running),
        ElementKind::Decision => DecisionStatus::parse(status) == Some(DecisionStatus::Waiting),
        _ => false,
    }
}
