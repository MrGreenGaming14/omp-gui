//! Shared test fixtures. `loa-bus` implements its `Authorize`/`Persist` ports
//! for [`AllowAll`], [`DenyAll`] and [`MemoryPersist`] behind its `testkit` feature.

use crate::{DecisionKind, Dom, Draft, ElementKind, IntoAttrs, JobKind, NodeId, Patch, Role, Value, attrs};
use std::sync::{Arc, Mutex};

/// Grants every request.
#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAll;

/// Refuses every request.
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyAll;

/// Keeps every persisted patch in memory; clones share the list.
#[derive(Clone, Debug, Default)]
pub struct MemoryPersist {
    pub patches: Arc<Mutex<Vec<Patch>>>,
    /// When set, appends fail with this message.
    pub fail: Option<String>,
}

/// Fluent session tree builder. Each call adds one node under the latest
/// fitting parent and records it as [`DomFixture::last`].
#[derive(Clone, Debug)]
pub struct DomFixture {
    pub dom: Dom,
    pub session: NodeId,
    turn: Option<NodeId>,
    call: Option<NodeId>,
    pub last: NodeId,
}

impl DomFixture {
    /// A `<session id="s1" harness="omp" node="k" created=1>`.
    pub fn session() -> Self {
        let mut dom = Dom::new();
        let attrs = attrs! { ID: "s1", HARNESS: "omp", NODE: "k", CREATED: 1i64 };
        let session = dom.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""), 1).unwrap().node;
        Self { dom, session, turn: None, call: None, last: session }
    }

    /// Inserts any node; panics when the schema refuses it.
    pub fn add(&mut self, parent: NodeId, kind: ElementKind, attrs: impl IntoAttrs, text: &str) -> NodeId {
        let draft = Draft::insert(parent, kind, attrs.into_attrs(), text);
        self.last = self.dom.commit(draft, 0).unwrap_or_else(|e| panic!("fixture insert {kind}: {e}")).node;
        self.last
    }

    pub fn commit(&mut self, draft: Draft) -> &mut Self {
        self.dom.commit(draft, 0).unwrap_or_else(|e| panic!("fixture commit: {e}"));
        self
    }

    pub fn turn(&mut self) -> &mut Self {
        self.turn = Some(self.add(self.session, ElementKind::Turn, attrs! {}, ""));
        self.call = None;
        self
    }

    pub fn message(&mut self, role: Role, text: &str) -> &mut Self {
        let turn = self.need_turn();
        self.add(turn, ElementKind::Message, attrs! { ROLE: role }, text);
        self
    }

    pub fn call(&mut self, tool: &str, attrs: impl IntoAttrs) -> &mut Self {
        let mut attrs = attrs.into_attrs();
        attrs.insert(crate::attr::TOOL.into(), Value::from(tool));
        let turn = self.need_turn();
        self.call = Some(self.add(turn, ElementKind::Call, attrs, ""));
        self
    }

    /// A decision under the latest call, else the latest turn.
    pub fn decision(&mut self, kind: DecisionKind) -> &mut Self {
        let parent = match self.call {
            Some(call) => call,
            None => self.need_turn(),
        };
        self.add(parent, ElementKind::Decision, attrs! { KIND: kind }, "");
        self
    }

    pub fn job(&mut self, kind: JobKind) -> &mut Self {
        self.add(self.session, ElementKind::Job, attrs! { KIND: kind }, "");
        self
    }

    pub fn build(&self) -> Dom {
        self.dom.clone()
    }

    fn need_turn(&mut self) -> NodeId {
        match self.turn {
            Some(turn) => turn,
            None => self.turn().turn.unwrap(),
        }
    }
}
