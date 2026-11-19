//! The write side of a session.
//!
//! One writer per session: everything that wants to change a session hands a
//! [`Draft`] to the session's [`PatchSink`], which orders it, applies it and
//! returns the committed [`Patch`].

use crate::dom::{ApplyError, Dom};
use crate::patch::{Draft, Patch};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

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
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// A sink that keeps the DOM and every patch in memory. For tests and for
/// building a DOM without a journal (imports, previews).
#[derive(Debug, Default)]
pub struct MemorySink {
    state: Mutex<(Dom, Vec<Patch>)>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Continues from an existing DOM.
    pub fn from_dom(dom: Dom) -> Self {
        Self { state: Mutex::new((dom, Vec::new())) }
    }

    /// A copy of the current DOM.
    pub fn dom(&self) -> Dom {
        self.lock().0.clone()
    }

    /// Every patch committed through this sink, in order.
    pub fn patches(&self) -> Vec<Patch> {
        self.lock().1.clone()
    }

    /// Reads the current DOM without copying it.
    pub fn with_dom<R>(&self, read: impl FnOnce(&Dom) -> R) -> R {
        read(&self.lock().0)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (Dom, Vec<Patch>)> {
        // A panic while holding the lock can't leave the pair half updated:
        // `Dom::commit` is atomic and the push follows it.
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl PatchSink for MemorySink {
    fn commit_at(&self, draft: Draft, ts: i64) -> Result<Patch, CommitError> {
        let mut state = self.lock();
        let patch = state.0.commit(draft, ts)?;
        state.1.push(patch.clone());
        Ok(patch)
    }
}
