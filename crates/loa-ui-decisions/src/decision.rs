//! `<decision>` nodes read out of the session DOM, and the canonical answers
//! each kind produces.

use loa_adapter::Answer;
use loa_core::{DecisionKind, DecisionStatus, Dom, ElementKind, Node, NodeId, answer, attr};

/// One `<decision>` node as plain data: what a card shows and what the
/// waiting tracker needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub id: NodeId,
    pub kind: DecisionKind,
    /// A decision inserted without `status` is waiting.
    pub status: DecisionStatus,
    pub title: Option<String>,
    /// Node text: command or diff preview, question, plan markdown.
    pub body: String,
    pub options: Vec<String>,
    pub multi: bool,
    pub answer: Vec<String>,
    pub by: Option<String>,
    pub reason: Option<String>,
    /// Form: JSON Schema of the fields.
    pub fields: Option<String>,
    /// Unattended runs: answer applied when `timeout_ms` passes.
    pub default: Vec<String>,
    pub timeout_ms: Option<i64>,
    pub created: Option<i64>,
    /// Tool of the `<call>` the decision is about: the node named by the
    /// `call` attribute, else the enclosing `<call>`.
    pub tool: Option<String>,
    /// Harness of the session the decision belongs to: the `harness`
    /// attribute of the node, or of its nearest ancestor that has one.
    pub harness: Option<String>,
}

impl Decision {
    /// Reads the node `id`. `None` when it is missing or not a `<decision>`.
    pub fn from_dom(dom: &Dom, id: NodeId) -> Option<Self> {
        Self::from_node(dom, dom.get(id)?)
    }

    /// Reads `node`. `None` when it is not a `<decision>`.
    pub fn from_node(dom: &Dom, node: &Node) -> Option<Self> {
        if node.kind != ElementKind::Decision {
            return None;
        }
        let kind = node.str_attr(attr::KIND).and_then(DecisionKind::parse)?;
        let status = node
            .str_attr(attr::STATUS)
            .and_then(DecisionStatus::parse)
            .unwrap_or(DecisionStatus::Waiting);
        let strings = |name: &str| -> Vec<String> {
            node.attr(name)
                .map(|v| v.str_items().map(str::to_owned).collect())
                .unwrap_or_default()
        };
        let call = node
            .int_attr(attr::CALL)
            .and_then(|id| u64::try_from(id).ok())
            .and_then(|id| dom.get(NodeId(id)))
            .filter(|n| n.kind == ElementKind::Call)
            .or_else(|| dom.ancestors(node.id).find(|n| n.kind == ElementKind::Call));
        Some(Self {
            id: node.id,
            kind,
            status,
            title: node.str_attr(attr::TITLE).map(str::to_owned),
            body: node.text.clone(),
            options: strings(attr::OPTIONS),
            multi: node.bool_attr(attr::MULTI).unwrap_or(false),
            answer: strings(attr::ANSWER),
            by: node.str_attr(attr::BY).map(str::to_owned),
            reason: node.str_attr(attr::REASON).map(str::to_owned),
            fields: node.str_attr(attr::FIELDS).map(str::to_owned),
            default: strings(attr::DEFAULT),
            timeout_ms: node.int_attr(attr::TIMEOUT_MS),
            created: node.int_attr(attr::CREATED),
            tool: call.and_then(|c| c.str_attr(attr::TOOL)).map(str::to_owned),
            harness: node
                .str_attr(attr::HARNESS)
                .or_else(|| {
                    dom.ancestors(node.id)
                        .find_map(|n| n.str_attr(attr::HARNESS))
                })
                .map(str::to_owned),
        })
    }

    /// Every decision in the DOM, in document order.
    pub fn all(dom: &Dom) -> impl Iterator<Item = Decision> + '_ {
        dom.iter()
            .filter_map(move |node| Self::from_node(dom, node))
    }

    pub fn is_waiting(&self) -> bool {
        self.status == DecisionStatus::Waiting
    }

    /// The answer an unattended run applies when the timeout passes, with
    /// `by` set to `by`. `None` when there is no default or it is not a valid
    /// answer for the kind (an edit needs the plan text, a form needs one
    /// JSON object, an approval one of the three approval answers).
    pub fn default_answer(&self, by: &str) -> Option<Answer> {
        let valid = match self.kind {
            DecisionKind::Approval => {
                matches!(self.default.as_slice(), [a] if ApprovalChoice::from_answer(a).is_some())
            }
            DecisionKind::Plan => {
                matches!(self.default.as_slice(), [a] if a == answer::APPROVE || a == answer::REJECT)
            }
            DecisionKind::Question => match self.default.as_slice() {
                [] => false,
                [_] => true,
                many => self.multi && many.iter().all(|a| self.options.contains(a)),
            },
            DecisionKind::Form => matches!(
                self.default.as_slice(),
                [json] if serde_json::from_str::<serde_json::Value>(json).is_ok_and(|v| v.is_object())
            ),
        };
        valid.then(|| Answer {
            values: self.default.clone(),
            reason: None,
            by: by.to_owned(),
        })
    }
}

/// The three responses of an approval card, in shortcut order.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ApprovalChoice {
    AllowOnce,
    AllowSession,
    Deny,
}

impl ApprovalChoice {
    pub const ALL: [ApprovalChoice; 3] = [Self::AllowOnce, Self::AllowSession, Self::Deny];

    pub fn answer(self) -> &'static str {
        match self {
            Self::AllowOnce => answer::ALLOW_ONCE,
            Self::AllowSession => answer::ALLOW_SESSION,
            Self::Deny => answer::DENY,
        }
    }

    pub fn from_answer(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.answer() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AllowOnce => "Allow once",
            Self::AllowSession => "Allow for this session",
            Self::Deny => "Deny",
        }
    }

    fn past(self) -> &'static str {
        match self {
            Self::AllowOnce => "Allowed",
            Self::AllowSession => "Allowed for this session",
            Self::Deny => "Denied",
        }
    }
}

/// The three responses of a plan review card, in shortcut order.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PlanChoice {
    Approve,
    Edit,
    Reject,
}

impl PlanChoice {
    pub const ALL: [PlanChoice; 3] = [Self::Approve, Self::Edit, Self::Reject];

    pub fn answer(self) -> &'static str {
        match self {
            Self::Approve => answer::APPROVE,
            Self::Edit => answer::EDIT,
            Self::Reject => answer::REJECT,
        }
    }

    pub fn from_answer(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.answer() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Approve => "Approve",
            Self::Edit => "Edit",
            Self::Reject => "Reject",
        }
    }

    fn past(self) -> &'static str {
        match self {
            Self::Approve => "Approved",
            Self::Edit => "Approved with edits",
            Self::Reject => "Rejected",
        }
    }
}

/// One line saying what was answered, for read only cards. An empty answer
/// is a skipped prompt.
pub fn answer_summary(kind: DecisionKind, values: &[String]) -> String {
    match (kind, values) {
        (_, []) => "Skipped".to_owned(),
        (DecisionKind::Approval, [a]) if let Some(c) = ApprovalChoice::from_answer(a) => {
            c.past().to_owned()
        }
        (DecisionKind::Plan, [a]) if let Some(c) = PlanChoice::from_answer(a) => {
            c.past().to_owned()
        }
        (_, values) => values.join(", "),
    }
}
