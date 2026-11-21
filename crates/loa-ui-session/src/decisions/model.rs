//! `<decision>` nodes read out of the session DOM, and the fixed answer sets of approvals and
//! plan reviews as [`Choice`] enums.

use super::card::kind_for;
use loa_core::{DecisionKind, DecisionStatus, Dom, ElementKind, Node, NodeId, answer, attr};
use loa_contracts::harness::Answer;

/// One `<decision>` node as plain data: what a card shows and what the tracker needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub id: NodeId,
    pub kind: DecisionKind,
    /// A decision inserted without `status` is waiting.
    pub status: DecisionStatus,
    pub title: Option<String>,
    /// Command or diff preview, question, plan markdown.
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
    /// Tool of the `<call>` named by `call`, else of the enclosing `<call>`.
    pub tool: Option<String>,
    /// `harness` of the node or its nearest ancestor that has one.
    pub harness: Option<String>,
}

impl Decision {
    pub fn from_dom(dom: &Dom, id: NodeId) -> Option<Self> { Self::from_node(dom, dom.get(id)?) }

    /// Reads `node`; `None` when it is not a `<decision>` of a known kind.
    pub fn from_node(dom: &Dom, node: &Node) -> Option<Self> {
        if node.kind != ElementKind::Decision {
            return None;
        }
        let strings = |name: &str| -> Vec<String> { node.attr(name).map(|v| v.str_items().map(str::to_owned).collect()).unwrap_or_default() };
        let text = |name: &str| node.str_attr(name).map(str::to_owned);
        let named = node.int_attr(attr::CALL).and_then(|id| dom.get(NodeId(u64::try_from(id).ok()?))).filter(|n| n.kind == ElementKind::Call);
        let call = named.or_else(|| dom.ancestors(node.id).find(|n| n.kind == ElementKind::Call));
        Some(Self {
            id: node.id,
            kind: node.str_attr(attr::KIND).and_then(DecisionKind::parse)?,
            status: node.str_attr(attr::STATUS).and_then(DecisionStatus::parse).unwrap_or(DecisionStatus::Waiting),
            title: text(attr::TITLE),
            body: node.text.clone(),
            options: strings(attr::OPTIONS),
            multi: node.bool_attr(attr::MULTI).unwrap_or(false),
            answer: strings(attr::ANSWER),
            by: text(attr::BY),
            reason: text(attr::REASON),
            fields: text(attr::FIELDS),
            default: strings(attr::DEFAULT),
            timeout_ms: node.int_attr(attr::TIMEOUT_MS),
            created: node.int_attr(attr::CREATED),
            tool: call.and_then(|c| c.str_attr(attr::TOOL)).map(str::to_owned),
            harness: text(attr::HARNESS).or_else(|| dom.ancestors(node.id).find_map(|n| n.str_attr(attr::HARNESS)).map(str::to_owned)),
        })
    }

    /// Every decision in the DOM, in document order.
    pub fn all(dom: &Dom) -> impl Iterator<Item = Decision> + '_ { dom.iter().filter_map(move |node| Self::from_node(dom, node)) }

    pub fn is_waiting(&self) -> bool { self.status == DecisionStatus::Waiting }

    /// The decision's own default as an answer by `by`, when it is valid for the kind.
    pub fn default_answer(&self, by: &str) -> Option<Answer> {
        kind_for(self.kind).valid_default(self).then(|| Answer { values: self.default.clone(), reason: None, by: by.to_owned() })
    }
}

/// A fixed answer set of a card, in shortcut order.
pub trait Choice: Copy + PartialEq + 'static {
    const ALL: &'static [Self];
    /// The wire answer.
    fn answer(self) -> &'static str;
    /// The button label.
    fn label(self) -> &'static str;
    /// How an answered card reads.
    fn past(self) -> &'static str;
    fn from_answer(value: &str) -> Option<Self> { Self::ALL.iter().copied().find(|c| c.answer() == value) }
}

macro_rules! choice_enum {
    ($(#[$m:meta])* $name:ident { $($v:ident = $answer:expr, $label:literal, $past:literal;)+ }) => {
        $(#[$m])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq)]
        pub enum $name { $($v),+ }
        impl Choice for $name {
            const ALL: &'static [Self] = &[$(Self::$v),+];
            fn answer(self) -> &'static str { match self { $(Self::$v => $answer),+ } }
            fn label(self) -> &'static str { match self { $(Self::$v => $label),+ } }
            fn past(self) -> &'static str { match self { $(Self::$v => $past),+ } }
        }
    };
}

choice_enum!(
    /// The responses of an approval card.
    ApprovalChoice {
        AllowOnce = answer::ALLOW_ONCE, "Allow once", "Allowed";
        AllowSession = answer::ALLOW_SESSION, "Allow for this session", "Allowed for this session";
        Deny = answer::DENY, "Deny", "Denied";
    }
);

choice_enum!(
    /// The responses of a plan review card.
    PlanChoice {
        Approve = answer::APPROVE, "Approve", "Approved";
        Edit = answer::EDIT, "Edit", "Approved with edits";
        Reject = answer::REJECT, "Reject", "Rejected";
    }
);

/// One line saying what was answered; an empty answer is a skipped prompt.
pub fn answer_summary(kind: DecisionKind, values: &[String]) -> String {
    match (kind, values) {
        (_, []) => "Skipped".to_owned(),
        (DecisionKind::Approval, [a]) if let Some(c) = ApprovalChoice::from_answer(a) => c.past().to_owned(),
        (DecisionKind::Plan, [a]) if let Some(c) = PlanChoice::from_answer(a) => c.past().to_owned(),
        (_, values) => values.join(", "),
    }
}
