//! A broken contract rule, reported by every check in the testkit.

use loa_core::NodeId;
use std::fmt;

/// The rule a [`Violation`] breaks. Tests match on it; the detail text is for people.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Rule {
    /// The document has no root, or the root isn't a `<session>`.
    Root,
    /// A `<session>` attribute required by the conventions is missing or wrong.
    SessionAttrs,
    /// The schema version declared or written isn't `loa_core::SCHEMA_VERSION`.
    SchemaVersion,
    /// `<session harness>` or `<ext harness>` doesn't name the adapter.
    Harness,
    /// An element that has a status machine has no status.
    MissingStatus,
    /// A turn, message, call or decision is still open where everything must
    /// have ended (a settled DOM, or under a turn that ended).
    NotTerminal,
    /// More than one turn is open, or the open turn isn't the last one.
    OpenTurn,
    /// A turn doesn't start with the user's message, or a user message isn't done.
    TurnShape,
    /// Thinking on a message that isn't the assistant's.
    MessageShape,
    /// A call has no `<input>`, more than one, or arguments that aren't JSON.
    CallInput,
    /// A call's `<result>` disagrees with its status, or there are several.
    CallResult,
    /// A waiting decision without a waiting turn, or the other way round.
    Waiting,
    /// A decision's `answer`, `by` or `ended` disagrees with its status, or an
    /// answer isn't a valid value for the decision kind.
    DecisionAnswer,
    /// An attribute that holds a node id (`call`, `covers`, `pointers`) names
    /// a node that doesn't exist or has the wrong kind.
    DanglingRef,
    /// Two elements carry the same harness id (`call_id`, `request_id`).
    DuplicateId,
    /// `ended` before `started`.
    Timestamps,
    /// A status `set` that the status machine doesn't allow.
    Transition,
    /// A turn ended while an element under it was still open.
    CloseOrder,
    /// A patch history that doesn't replay onto an empty DOM.
    Replay,
    /// Declared capabilities disagree with what the adapter does.
    Capability,
    /// The adapter raised a decision kind its capabilities don't declare.
    Undeclared,
    /// An adapter call failed or timed out during a live run.
    Adapter,
    /// The DOM after a live step isn't what the step implies.
    Step,
    /// The exported native file differs from the imported one.
    Export,
    /// Re-importing the export gives a different DOM.
    Reimport,
}

/// One broken rule, with the node it was found on when there is one.
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

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.node {
            Some(node) => write!(f, "[{:?}] node {node}: {}", self.rule, self.detail),
            None => write!(f, "[{:?}] {}", self.rule, self.detail),
        }
    }
}

/// Writes one violation per line, or "no violations".
pub(crate) fn write_list(f: &mut fmt::Formatter<'_>, violations: &[Violation]) -> fmt::Result {
    if violations.is_empty() {
        return writeln!(f, "no violations");
    }
    for violation in violations {
        writeln!(f, "  {violation}")?;
    }
    Ok(())
}
