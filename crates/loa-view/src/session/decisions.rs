//! Decision cards: what one session's cards and the desk over every session ask of the app, and
//! the unattended policy they apply and edit.

use super::Answer;
use loa_core::{DecisionKind, NodeId};

/// Default answers for decisions nobody answers in time; a kind without one waits forever.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnattendedView {
    pub approval: Option<DecisionDefaultView>,
    pub question: Option<DecisionDefaultView>,
    pub plan: Option<DecisionDefaultView>,
    pub form: Option<DecisionDefaultView>,
}

impl UnattendedView {
    pub fn for_kind(&self, kind: DecisionKind) -> Option<&DecisionDefaultView> {
        match kind {
            DecisionKind::Approval => self.approval.as_ref(),
            DecisionKind::Question => self.question.as_ref(),
            DecisionKind::Plan => self.plan.as_ref(),
            DecisionKind::Form => self.form.as_ref(),
        }
    }

    pub fn for_kind_mut(&mut self, kind: DecisionKind) -> &mut Option<DecisionDefaultView> {
        match kind {
            DecisionKind::Approval => &mut self.approval,
            DecisionKind::Question => &mut self.question,
            DecisionKind::Plan => &mut self.plan,
            DecisionKind::Form => &mut self.form,
        }
    }
}

/// What to answer when a decision has waited `timeout_secs`; `reason` is recorded with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionDefaultView {
    pub timeout_secs: u64,
    pub answer: String,
    pub reason: Option<String>,
}

/// What the cards of one session ask the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Pass to the harness; the card stays locked until the DOM records the answer or the app
    /// reports the answer failed.
    Answer { decision: NodeId, answer: Answer },
    OpenLink(String),
}

/// What the desk over every open session asks the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeskIntent {
    Answer { session: String, decision: NodeId, answer: Answer },
    OpenLink(String),
    /// The unattended settings changed: persist them.
    PolicyChanged(UnattendedView),
    NotifyFailed(String),
}
