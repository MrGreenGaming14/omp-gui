//! [`TurnLedger`]: the open turn of a session and what is open inside it
//! (messages and calls by harness key, waiting decisions), so every mapper
//! derives its state and closes turns the same way.

use loa_contracts::harness::{Prompt, SessionState};
use loa_core::{
    CallStatus, CommitError, DecisionStatus, Dom, DomWriter, ElementKind as E, MessageStatus, Node, NodeId, Role,
    TurnStatus, attr,
};
use std::collections::HashMap;
use std::hash::Hash;

/// The turn being run.
#[derive(Debug)]
pub struct OpenTurn<K> {
    pub node: NodeId,
    /// Streaming messages and running calls, by the harness's id for them.
    pub open: HashMap<K, NodeId>,
    /// Decisions waiting for an answer.
    pub waiting: Vec<NodeId>,
}

/// What [`TurnLedger::end_turn`] reports to the harness policy, in order:
/// each child it cancels, then the turn itself.
#[derive(Debug)]
pub enum EndEvent<'a, K> {
    /// An open message or call, closed as cancelled (or as the turn's error).
    Child { key: &'a K, node: NodeId },
    /// A waiting decision, cancelled.
    Decision(NodeId),
    /// The turn, closed with `status`. Policies hook here to drain after an
    /// interrupt (claude), adopt self-started turns (codex), start the next
    /// queued follow-up (omp) or skip replayed history (acp).
    Turn { node: NodeId, status: TurnStatus },
}

/// Per-session turn bookkeeping. `K` is the harness's key for messages and calls.
#[derive(Debug)]
pub struct TurnLedger<K: Hash + Eq> {
    pub session: NodeId,
    turn: Option<OpenTurn<K>>,
    closed: bool,
}

impl<K: Hash + Eq> TurnLedger<K> {
    pub fn new(session: NodeId) -> Self {
        Self { session, turn: None, closed: false }
    }

    /// Rebuilds the ledger of a resumed session: the root, and its last turn
    /// when that is still open (its children can't be keyed again, so they
    /// are cancelled when the turn ends).
    pub fn from_dom(dom: &Dom) -> Self {
        let session = dom.root().map_or(NodeId::DOCUMENT, |root| root.id);
        let open = |n: &&Node| matches!(n.str_attr(attr::STATUS).and_then(TurnStatus::parse), Some(TurnStatus::Running | TurnStatus::Waiting));
        let turn = dom.children_of_kind(session, E::Turn).last().filter(open).map(|turn| OpenTurn {
            node: turn.id,
            open: HashMap::new(),
            waiting: dom
                .descendants(turn.id)
                .filter(|n| n.kind == E::Decision && n.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str()))
                .map(|n| n.id)
                .collect(),
        });
        Self { session, turn, closed: false }
    }

    pub fn state(&self) -> SessionState {
        match &self.turn {
            _ if self.closed => SessionState::Closed,
            Some(turn) if !turn.waiting.is_empty() => SessionState::Waiting,
            Some(_) => SessionState::Running,
            None => SessionState::Idle,
        }
    }

    pub fn turn(&self) -> Option<&OpenTurn<K>> {
        self.turn.as_ref()
    }

    /// Opens a turn with the user's message.
    pub fn begin(&mut self, w: &DomWriter, p: &Prompt) -> Result<NodeId, CommitError> {
        let node = w.insert(self.session, E::Turn, [(attr::STATUS, TurnStatus::Running.as_str())], "")?;
        let by = [(attr::ROLE, Role::User.as_str()), (attr::STATUS, MessageStatus::Done.as_str()), (attr::BY, p.by.as_str())];
        w.insert(node, E::Message, by, p.text.as_str())?;
        self.turn = Some(OpenTurn { node, open: HashMap::new(), waiting: Vec::new() });
        Ok(node)
    }

    /// Tracks an open message or call under the harness's `key`.
    pub fn open(&mut self, key: K, node: NodeId) {
        if let Some(turn) = &mut self.turn {
            turn.open.insert(key, node);
        }
    }

    pub fn get(&self, key: &K) -> Option<NodeId> {
        self.turn.as_ref()?.open.get(key).copied()
    }

    /// Stops tracking `key`; the caller closes the node.
    pub fn take(&mut self, key: &K) -> Option<NodeId> {
        self.turn.as_mut()?.open.remove(key)
    }

    pub fn wait(&mut self, decision: NodeId) {
        if let Some(turn) = &mut self.turn {
            turn.waiting.push(decision);
        }
    }

    pub fn answered(&mut self, decision: NodeId) {
        if let Some(turn) = &mut self.turn {
            turn.waiting.retain(|d| *d != decision);
        }
    }

    /// Ends the open turn: cancels what is still open inside it (as `error`
    /// when the turn failed), closes the turn and reports each step to `hook`.
    pub fn end_turn(&mut self, w: &DomWriter, status: TurnStatus, mut hook: impl FnMut(EndEvent<'_, K>)) -> Result<(), CommitError> {
        let Some(turn) = self.turn.take() else { return Ok(()) };
        let failed = status == TurnStatus::Error;
        for (key, node) in &turn.open {
            // Calls and messages share the `error` and `cancelled` names.
            w.set(*node, attr::STATUS, if failed { CallStatus::Error } else { CallStatus::Cancelled })?;
            hook(EndEvent::Child { key, node: *node });
        }
        for decision in turn.waiting {
            w.close(decision, DecisionStatus::Cancelled)?;
            hook(EndEvent::Decision(decision));
        }
        w.close(turn.node, status)?;
        hook(EndEvent::Turn { node: turn.node, status });
        Ok(())
    }

    /// The harness is gone: nothing more will open.
    pub fn close(&mut self) {
        self.closed = true;
    }
}
