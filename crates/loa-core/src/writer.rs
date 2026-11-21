//! [`DomWriter`]: the drafts every harness, sandbox and hub writes, in one place.

use crate::schema::{ElementKind, attr};
use crate::sink::{CommitError, PatchSink, now_ms};
use crate::value::{IntoAttrs, Value};
use crate::{Draft, NodeId};

/// Commits convention-shaped drafts to one session's sink.
#[derive(Clone, Copy)]
pub struct DomWriter<'a> {
    sink: &'a dyn PatchSink,
    /// Fixed timestamp for imports; `None` stamps the current time.
    ts: Option<i64>,
}

impl<'a> DomWriter<'a> {
    pub fn new(sink: &'a dyn PatchSink) -> Self {
        Self { sink, ts: None }
    }

    /// A writer that stamps every patch, and `ended`, with `ts` (unix ms).
    pub fn at(self, ts: i64) -> Self {
        Self { ts: Some(ts), ..self }
    }

    pub fn now(&self) -> i64 {
        self.ts.unwrap_or_else(now_ms)
    }

    /// Commits any draft, for writes the helpers below don't cover.
    pub fn commit(&self, draft: Draft) -> Result<NodeId, CommitError> {
        Ok(self.sink.commit_at(draft, self.now())?.node)
    }

    /// Appends a `kind` node under `parent`. Returns the new node.
    pub fn insert(
        &self,
        parent: NodeId,
        kind: ElementKind,
        attrs: impl IntoAttrs,
        text: impl Into<String>,
    ) -> Result<NodeId, CommitError> {
        self.commit(Draft::insert(parent, kind, attrs.into_attrs(), text))
    }

    pub fn set(&self, node: NodeId, name: &str, value: impl Into<Value>) -> Result<(), CommitError> {
        self.commit(Draft::set(node, name, value)).map(drop)
    }

    pub fn clear(&self, node: NodeId, name: &str) -> Result<(), CommitError> {
        self.commit(Draft::clear(node, name)).map(drop)
    }

    pub fn remove(&self, node: NodeId) -> Result<(), CommitError> {
        self.commit(Draft::remove(node)).map(drop)
    }

    pub fn append(&self, node: NodeId, text: impl Into<String>) -> Result<(), CommitError> {
        self.commit(Draft::append(node, text)).map(drop)
    }

    pub fn set_text(&self, node: NodeId, text: impl Into<String>) -> Result<(), CommitError> {
        self.commit(Draft::set_text(node, text)).map(drop)
    }

    /// Ends a turn, call, job or decision: `status`, then `ended`.
    pub fn close(&self, node: NodeId, status: impl Into<Value>) -> Result<(), CommitError> {
        self.set(node, attr::STATUS, status)?;
        self.set(node, attr::ENDED, self.now())
    }

    /// An `<ext>` keeping harness data the schema has no place for; `raw` is
    /// written as text (a `serde_json::Value` displays as compact JSON).
    pub fn ext(&self, parent: NodeId, harness: &str, kind: &str, raw: impl std::fmt::Display) -> Result<NodeId, CommitError> {
        self.insert(parent, ElementKind::Ext, [(attr::HARNESS, harness), (attr::TYPE, kind)], raw.to_string())
    }
}
