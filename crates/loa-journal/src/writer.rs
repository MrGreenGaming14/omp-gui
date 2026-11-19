//! The journal side of a session writer: shared, cheap appends with a
//! configurable fsync policy.

use crate::error::Result;
use crate::journal::{Journal, Patches};
use loa_core::{Dom, Patch};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// When appended patches are fsynced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncPolicy {
    /// After every patch. Slowest, nothing is ever lost.
    EveryPatch,
    /// Once `max_patches` patches are pending, or when an append finds the
    /// oldest pending patch older than `max_delay`.
    Batch { max_patches: u32, max_delay: Duration },
    /// Only on [`JournalWriter::sync`], a snapshot, or drop.
    Manual,
}

impl Default for SyncPolicy {
    fn default() -> Self {
        SyncPolicy::Batch { max_patches: 64, max_delay: Duration::from_millis(200) }
    }
}

/// A [`Journal`] shared by the session's single writer and its readers.
///
/// Every append is written to the OS at once, so a crash of the app loses
/// nothing; the policy only bounds what a power loss can take.
pub struct JournalWriter {
    state: Mutex<State>,
}

struct State {
    journal: Journal,
    policy: SyncPolicy,
    pending: u32,
    oldest_pending: Option<Instant>,
}

impl JournalWriter {
    pub fn new(journal: Journal, policy: SyncPolicy) -> Self {
        Self { state: Mutex::new(State { journal, policy, pending: 0, oldest_pending: None }) }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Journal methods update their state only after the file write
        // succeeded, so a panic elsewhere can't leave it half changed.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Journals `patch`, which the writer has already applied to `dom`, writes
    /// a snapshot of `dom` when one is due, and fsyncs as the policy says.
    pub fn append(&self, patch: &Patch, dom: &Dom) -> Result<()> {
        let mut state = self.lock();
        state.journal.append_applied(patch, dom)?;
        if !state.journal.has_unsynced() {
            // A snapshot synced everything.
            state.pending = 0;
            state.oldest_pending = None;
            return Ok(());
        }
        let now = Instant::now();
        state.pending += 1;
        let oldest = *state.oldest_pending.get_or_insert(now);
        let due = match state.policy {
            SyncPolicy::EveryPatch => true,
            SyncPolicy::Batch { max_patches, max_delay } => {
                state.pending >= max_patches || now.duration_since(oldest) >= max_delay
            }
            SyncPolicy::Manual => false,
        };
        if due {
            state.sync()?;
        }
        Ok(())
    }

    /// Fsyncs every pending patch.
    pub fn sync(&self) -> Result<()> {
        self.lock().sync()
    }

    /// Patches appended but not fsynced yet.
    pub fn pending(&self) -> u32 {
        self.lock().pending
    }

    pub fn last_seq(&self) -> u64 {
        self.lock().journal.last_seq()
    }

    /// See [`Journal::patches`]. The returned iterator doesn't hold the lock.
    pub fn patches(&self, from_seq: u64) -> Result<Patches> {
        self.lock().journal.patches(from_seq)
    }

    /// See [`Journal::fork`].
    pub fn fork(&self, at_seq: u64, new_session_id: &str) -> Result<Journal> {
        self.lock().journal.fork(at_seq, new_session_id)
    }

    /// Syncs and hands the journal back.
    pub fn into_journal(self) -> Result<Journal> {
        let mut state = self.state.into_inner().unwrap_or_else(PoisonError::into_inner);
        state.sync()?;
        Ok(state.journal)
    }
}

impl State {
    fn sync(&mut self) -> Result<()> {
        self.journal.sync()?;
        self.pending = 0;
        self.oldest_pending = None;
        Ok(())
    }
}
