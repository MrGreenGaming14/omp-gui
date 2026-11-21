//! Plan mode on harnesses without it: a read-only tool gate enforced through
//! the harness's own tool approvals, the plan step added to each prompt, and
//! the plan review that ends the mode.
//!
//! The gate works on harnesses that ask the app before every tool that
//! writes or executes (omp with `--approval-mode always-ask` asks for every
//! `write` and `exec` tier tool, read tools run unasked). While a session is
//! in plan mode, [`PlanGate::verdict`] denies each such approval, so only
//! read-only tools run. A session the app did not register with
//! [`PlanGate::enforce_through_approvals`] can't enter plan mode: activation
//! fails closed.
//!
//! The plan review is a `<decision kind="plan">` the app inserts directly
//! under `<session>` ([`super::submit_plan`]). Harness-raised plan decisions
//! sit inside a `<turn>`; [`is_app_plan`] tells them apart, since only the
//! app can answer its own (with [`answer_plan`]).

use loa_util::LockExt;
use super::{Error, PlanPermit, Result};
use loa_contracts::harness::{AdapterError, Answer, HarnessSession};
use loa_core::{
    DecisionKind, DecisionStatus, Dom, Draft, ElementKind, MessageKind, Node, NodeId, PatchSink, Role,
    SessionMode, TodoState, Value, answer, attr,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The plan step, put before every prompt sent while a session is in plan mode.
pub const PLAN_STEP: &str = "[Plan mode] Tools that change files or run commands are blocked until the user \
approves a plan. Investigate with read-only tools only, then answer with a concise numbered plan for the \
user to review. Do not make any changes yet.";

/// Deny reason given to tool approvals while plan mode is on.
pub const DENY_REASON: &str = "Plan mode is read-only: write a plan for the user to review instead.";

/// What a prompt becomes while the session is in plan mode: the user's text
/// first, so it stays what the chat and the session title start with.
pub fn wrap_prompt(text: &str) -> String {
    format!("{text}\n\n{PLAN_STEP}")
}

/// Per session read-only tool gate. Shared by the app's decision handling,
/// which asks [`PlanGate::verdict`] about every waiting decision.
#[derive(Default)]
pub struct PlanGate {
    sessions: Mutex<HashMap<String, Entry>>,
}

#[derive(Default)]
struct Entry {
    enforced: bool,
    permits: usize,
}

impl PlanGate {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// Declares that `session_id`'s harness asks the app before every tool
    /// that writes or executes, so denying those approvals makes it read-only.
    pub fn enforce_through_approvals(&self, session_id: &str) {
        self.sessions.locked().entry(session_id.to_owned()).or_default().enforced = true;
    }

    /// Whether plan mode can be enforced for `session_id`.
    pub fn enforceable(&self, session_id: &str) -> bool {
        self.sessions.locked().get(session_id).is_some_and(|e| e.enforced)
    }

    /// Whether `session_id` is in plan mode.
    pub fn active(&self, session_id: &str) -> bool {
        self.sessions.locked().get(session_id).is_some_and(|e| e.permits > 0)
    }

    /// The answer the gate gives a waiting decision of `session_id`: a deny
    /// for every tool approval while plan mode is on, `None` otherwise.
    pub fn verdict(&self, session_id: &str, decision: &Node) -> Option<Answer> {
        let approval = decision.kind == ElementKind::Decision
            && decision.str_attr(attr::KIND) == Some(DecisionKind::Approval.as_str())
            && decision.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str());
        (approval && self.active(session_id)).then(|| Answer {
            values: vec![answer::DENY.to_owned()],
            reason: Some(DENY_REASON.to_owned()),
            by: "plan-mode".to_owned(),
        })
    }

    /// A permit that keeps `session_id` in plan mode while held. Fails
    /// closed for a session not registered with
    /// [`PlanGate::enforce_through_approvals`].
    pub fn permit(self: &Arc<Self>, session_id: &str) -> Result<Box<dyn PlanPermit>> {
        let mut sessions = self.sessions.locked();
        let entry = sessions
            .get_mut(session_id)
            .filter(|e| e.enforced)
            .ok_or_else(|| Error::Port("this harness can't be limited to read-only tools".into()))?;
        entry.permits += 1;
        drop(sessions);
        Ok(Box::new(Permit { gate: Arc::clone(self), session: session_id.to_owned() }))
    }
}

/// Puts the session in `dom` in plan mode: sets its `mode` and adds the plan
/// step, a `<todo>` in progress. Returns the step.
pub fn begin(dom: &Dom, sink: &dyn PatchSink) -> Result<NodeId> {
    let root = dom.root().ok_or(Error::NoSession)?.id;
    sink.commit(Draft::set(root, attr::MODE, SessionMode::Plan))?;
    Ok(sink
        .commit(Draft::insert(root, ElementKind::Todo, [(attr::STATE, Value::from(TodoState::InProgress))], "Write and review a plan"))?
        .node)
}

struct Permit {
    gate: Arc<PlanGate>,
    session: String,
}

impl PlanPermit for Permit {}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(entry) = self.gate.sessions.locked().get_mut(&self.session) {
            entry.permits = entry.permits.saturating_sub(1);
        }
    }
}

/// Whether `decision` is a plan review the app raised (see the module docs).
pub fn is_app_plan(dom: &Dom, decision: NodeId) -> bool {
    let Some(node) = dom.get(decision) else { return false };
    node.kind == ElementKind::Decision
        && node.str_attr(attr::KIND) == Some(DecisionKind::Plan.as_str())
        && dom.root().is_some_and(|root| root.children.contains(&decision))
}

/// The plan an agent wrote in `turn`: its last assistant text message.
pub fn plan_text(dom: &Dom, turn: NodeId) -> Option<&str> {
    dom.children_of_kind(turn, ElementKind::Message)
        .filter(|m| m.str_attr(attr::ROLE) == Some(Role::Assistant.as_str()))
        .filter(|m| m.str_attr(attr::KIND).is_none_or(|k| k == MessageKind::Text.as_str()))
        .map(|m| m.text.trim())
        .filter(|text| !text.is_empty())
        .last()
}

/// The plan step of plan mode: the `<todo>` [`begin`] inserted,
/// still in progress.
pub fn open_step(dom: &Dom) -> Option<NodeId> {
    let root = dom.root()?;
    dom.children_of_kind(root.id, ElementKind::Todo)
        .filter(|t| t.str_attr(attr::STATE) == Some(TodoState::InProgress.as_str()))
        .last()
        .map(|t| t.id)
}

/// How the user answered a plan review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    /// Approve this edited plan instead.
    Edit(String),
    /// Reject with feedback; the session stays in plan mode.
    Reject(String),
}

/// What follows a plan review.
pub enum Outcome {
    /// Plan mode is over and the gate released. Send `prompt` to carry out the plan.
    Execute { prompt: String },
    /// Still in plan mode. Send `prompt` for a revised plan; `step` is the new plan step.
    Revise { prompt: String, permit: Box<dyn PlanPermit>, step: NodeId },
}

/// Records the user's answer on an app plan review and ends or continues
/// plan mode accordingly.
pub async fn answer_plan(
    session: &dyn HarnessSession,
    dom: &Dom,
    sink: &dyn PatchSink,
    decision: NodeId,
    by: &str,
    verdict: Verdict,
    permit: Box<dyn PlanPermit>,
) -> Result<Outcome> {
    if !is_app_plan(dom, decision) {
        return Err(Error::Port(format!("{decision} is not a plan review the app raised")));
    }
    let root = dom.root().ok_or(Error::NoSession)?.id;
    let plan = dom.get(decision).map(|d| d.text.clone()).unwrap_or_default();
    let (value, reason) = match &verdict {
        Verdict::Approve => (answer::APPROVE, None),
        Verdict::Edit(edited) => (answer::EDIT, Some(edited.as_str())),
        Verdict::Reject(feedback) => (answer::REJECT, Some(feedback.as_str())),
    };
    sink.commit(Draft::set(decision, attr::ANSWER, vec![Value::from(value)]))?;
    if let Some(reason) = reason {
        sink.commit(Draft::set(decision, attr::REASON, reason))?;
    }
    sink.commit(Draft::set(decision, attr::BY, by))?;
    sink.commit(Draft::set(decision, attr::STATUS, DecisionStatus::Answered))?;
    sink.commit(Draft::set(decision, attr::ENDED, loa_core::now_ms()))?;
    match verdict {
        Verdict::Reject(feedback) => {
            let step = sink
                .commit(Draft::insert(root, ElementKind::Todo, [(attr::STATE, Value::from(TodoState::InProgress))], "Revise the plan"))?
                .node;
            let prompt = wrap_prompt(&format!("The user rejected the plan. Feedback: {feedback}\nRevise the plan."));
            Ok(Outcome::Revise { prompt, permit, step })
        }
        Verdict::Approve | Verdict::Edit(_) => {
            let plan = if let Verdict::Edit(edited) = verdict { edited } else { plan };
            match session.set_mode(SessionMode::Normal).await {
                Ok(()) | Err(AdapterError::Unsupported { .. }) => {}
                Err(error) => return Err(error.into()),
            }
            sink.commit(Draft::set(root, attr::MODE, SessionMode::Normal))?;
            drop(permit);
            Ok(Outcome::Execute { prompt: format!("The user approved this plan. Carry it out now.\n\n{plan}") })
        }
    }
}
