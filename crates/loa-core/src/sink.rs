//! The write side of a session: everything that changes a session hands a
//! [`Draft`] to its one [`PatchSink`], which orders, applies and returns it.

use loa_util::LockExt;
use crate::dom::{ApplyError, Dom};
use crate::patch::{Draft, Patch};
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum CommitError {
    #[error(transparent)]
    Apply(#[from] ApplyError),
    /// This node doesn't own the session, so it can't order patches for it.
    #[error("only the owner node writes this session")]
    NotWriter,
    #[error("the session is closed")]
    Closed,
    #[error("journal write failed: {0}")]
    Journal(String),
}

/// Commits drafts to one session. Implementations are cheap to call from any
/// thread and never block on subscribers.
pub trait PatchSink: Send + Sync {
    /// Commits `draft` with timestamp `ts` (unix milliseconds). Imports use it
    /// to keep the original times.
    fn commit_at(&self, draft: Draft, ts: i64) -> Result<Patch, CommitError>;

    /// Commits `draft` stamped with the current time.
    fn commit(&self, draft: Draft) -> Result<Patch, CommitError> {
        self.commit_at(draft, now_ms())
    }
}

/// Unix milliseconds.
pub use loa_util::time::now_ms;

/// A sink that keeps the DOM and every patch in memory. For tests and for
/// building a DOM without a journal (imports, previews).
#[derive(Debug, Default)]
pub struct MemorySink {
    state: Mutex<(Dom, Vec<Patch>)>,
}

impl MemorySink {
    pub fn new() -> Self { Self::default() }
    /// Continues from an existing DOM.
    pub fn from_dom(dom: Dom) -> Self { Self { state: Mutex::new((dom, Vec::new())) } }
    /// A copy of the current DOM.
    pub fn dom(&self) -> Dom { self.state.locked().0.clone() }
    /// Every patch committed through this sink, in order.
    pub fn patches(&self) -> Vec<Patch> { self.state.locked().1.clone() }
    /// Reads the current DOM without copying it.
    pub fn with_dom<R>(&self, read: impl FnOnce(&Dom) -> R) -> R { read(&self.state.locked().0) }
}

impl PatchSink for MemorySink {
    fn commit_at(&self, draft: Draft, ts: i64) -> Result<Patch, CommitError> {
        let mut state = self.state.locked();
        let patch = state.0.commit(draft, ts)?;
        state.1.push(patch.clone());
        Ok(patch)
    }
}
