#![allow(dead_code)]

use loa_bus::{Authorize, Denied, NodeKey, Persist, PersistError, SessionId, WriterConfig};
use loa_core::{Dom, Draft, ElementKind, NodeId, Patch, PatchSink, Value};
use std::sync::{Arc, Mutex};

pub const OWNER: NodeKey = NodeKey([1; 32]);
pub const PEER: NodeKey = NodeKey([2; 32]);
pub const STRANGER: NodeKey = NodeKey([3; 32]);

/// An in-memory journal. `fail_at` makes the append of that sequence fail.
#[derive(Clone, Default)]
pub struct Journal {
    pub patches: Arc<Mutex<Vec<Patch>>>,
    pub fail_at: Arc<Mutex<Option<u64>>>,
}

impl Journal {
    pub fn seqs(&self) -> Vec<u64> {
        self.patches.lock().unwrap().iter().map(|p| p.seq).collect()
    }

    pub fn replay(&self) -> Dom {
        let mut dom = Dom::new();
        for patch in self.patches.lock().unwrap().iter() {
            dom.apply(patch).unwrap();
        }
        dom
    }
}

impl Persist for Journal {
    fn append(&mut self, patch: &Patch) -> Result<(), PersistError> {
        if *self.fail_at.lock().unwrap() == Some(patch.seq) {
            return Err("disk full".into());
        }
        self.patches.lock().unwrap().push(patch.clone());
        Ok(())
    }
}

/// Allows `PEER` to append to and set attributes on nodes under `scope`
/// only; refuses everyone else.
pub struct Scoped {
    pub scope: Mutex<Option<NodeId>>,
}

impl Authorize for Scoped {
    fn authorize(&self, _: &SessionId, requester: &NodeKey, draft: &Draft, dom: &Dom) -> Result<(), Denied> {
        if *requester != PEER {
            return Err(Denied("no grant".into()));
        }
        let target = match draft {
            Draft::Insert { parent, .. } => *parent,
            Draft::Set { node, .. } | Draft::Append { node, .. } | Draft::Remove { node } => *node,
        };
        let scope = self.scope.lock().unwrap().ok_or_else(|| Denied("no scope".into()))?;
        let inside = target == scope || dom.ancestors(target).any(|n| n.id == scope);
        if inside { Ok(()) } else { Err(Denied("outside the granted turn".into())) }
    }
}

pub fn allow_none() -> Arc<Scoped> {
    Arc::new(Scoped { scope: Mutex::new(None) })
}

pub fn config(session: &str, local: NodeKey, journal: &Journal, authorize: Arc<dyn Authorize>) -> WriterConfig {
    WriterConfig {
        session: SessionId::from(session),
        owner: OWNER,
        local,
        dom: Dom::new(),
        persist: Box::new(journal.clone()),
        authorize,
    }
}

/// Ids of a session with one running turn and one streaming assistant message.
pub struct Tree {
    pub session: NodeId,
    pub turn: NodeId,
    pub message: NodeId,
}

pub fn build(sink: &dyn PatchSink, id: &str) -> Tree {
    let session = sink
        .commit(Draft::insert(
            NodeId::DOCUMENT,
            ElementKind::Session,
            [
                ("id", Value::from(id)),
                ("harness", Value::from("omp")),
                ("node", Value::from("owner")),
                ("created", Value::from(1_i64)),
            ],
            "",
        ))
        .unwrap()
        .node;
    let turn = sink.commit(Draft::insert(session, ElementKind::Turn, [("status", Value::from("running"))], "")).unwrap().node;
    let message = sink
        .commit(Draft::insert(
            turn,
            ElementKind::Message,
            [("role", Value::from("assistant")), ("status", Value::from("streaming"))],
            "",
        ))
        .unwrap()
        .node;
    Tree { session, turn, message }
}
