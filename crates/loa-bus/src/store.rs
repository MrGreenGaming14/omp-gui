//! All sessions under one root directory, one subdirectory per session, and
//! the journal side of a session writer: shared, cheap appends with a
//! pluggable fsync strategy.

use crate::error::{IoContext, JournalError as Error, Result};
use crate::journal::{JOURNAL_FILE, Journal, JournalOptions, Patches, SessionInfo, Truncation, lock, validate_session_id};
use crate::{Persist, PersistError};
use loa_core::{Dom, Patch};
use loa_util::LockExt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// One entry of [`Store::list`].
#[derive(Debug)]
pub enum Listing {
    Session(SessionInfo),
    /// A session directory whose journal can't be read.
    Unreadable { id: String, error: Error },
}

/// The session journals under a root directory.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
    options: JournalOptions,
}

impl Store {
    /// A store over `root`, created when missing. `options` apply to every
    /// journal it opens or creates.
    pub fn new(root: impl Into<PathBuf>, options: JournalOptions) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root).at(&root)?;
        Ok(Self { root, options })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn dir(&self, id: &str) -> Result<PathBuf> {
        validate_session_id(id)?;
        Ok(self.root.join(id))
    }

    /// Every session, sorted by id. Reads only each journal's tail after its
    /// newest snapshot and never repairs anything.
    pub fn list(&self) -> Result<Vec<Listing>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root).at(&self.root)? {
            let entry = entry.at(&self.root)?;
            let dir = entry.path();
            if !dir.join(JOURNAL_FILE).is_file() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            out.push(match Journal::inspect(&dir) {
                Ok(info) => Listing::Session(info),
                Err(error) => Listing::Unreadable { id, error },
            });
        }
        out.sort_by(|a, b| listing_id(a).cmp(listing_id(b)));
        Ok(out)
    }

    /// Creates a new empty session.
    pub fn create(&self, id: &str) -> Result<Journal> {
        Journal::create(&self.dir(id)?, id, self.options.clone())
    }

    /// Opens a session for writing; see [`Journal::open`].
    pub fn open(&self, id: &str) -> Result<(Journal, Option<Truncation>)> {
        let dir = self.dir(id)?;
        if !dir.join(JOURNAL_FILE).is_file() {
            return Err(Error::NotFound(id.to_owned()));
        }
        Journal::open(&dir, self.options.clone())
    }

    /// Deletes a session and its snapshots. Fails with [`Error::Locked`]
    /// while a journal has it open. Forks of it keep working: they hold their
    /// own copy of the shared patches.
    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.dir(id)?;
        if !dir.join(JOURNAL_FILE).is_file() {
            return Err(Error::NotFound(id.to_owned()));
        }
        // Released before removal: some platforms can't delete an open file.
        drop(lock(&dir)?);
        fs::remove_dir_all(&dir).at(&dir)
    }
}

fn listing_id(listing: &Listing) -> &str {
    match listing {
        Listing::Session(info) => &info.id,
        Listing::Unreadable { id, .. } => id,
    }
}
/// Decides after each append whether the pending patches are fsynced now.
pub trait SyncStrategy: Send {
    /// `pending` patches are unsynced, the oldest appended at `oldest`.
    fn after_append(&mut self, pending: usize, oldest: Instant) -> bool;
}

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

impl SyncStrategy for SyncPolicy {
    fn after_append(&mut self, pending: usize, oldest: Instant) -> bool {
        match *self {
            SyncPolicy::EveryPatch => true,
            SyncPolicy::Batch { max_patches, max_delay } => {
                pending >= max_patches as usize || oldest.elapsed() >= max_delay
            }
            SyncPolicy::Manual => false,
        }
    }
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
    session_id: String,
    state: Mutex<State>,
}

struct State {
    /// `None` once [`JournalWriter::close`] released it.
    journal: Option<Journal>,
    /// Last sequence, kept for after the close.
    last_seq: u64,
    strategy: Box<dyn SyncStrategy>,
    pending: usize,
    oldest_pending: Option<Instant>,
}

impl JournalWriter {
    pub fn new(journal: Journal, strategy: impl SyncStrategy + 'static) -> Self {
        let session_id = journal.session_id().to_owned();
        let last_seq = journal.last_seq();
        let strategy = Box::new(strategy);
        Self { session_id, state: Mutex::new(State { journal: Some(journal), last_seq, strategy, pending: 0, oldest_pending: None }) }
    }

    /// Journals `patch`, which the writer has already applied to `dom`, writes
    /// a snapshot of `dom` when one is due, and fsyncs as the policy says.
    pub fn append(&self, patch: &Patch, dom: &Dom) -> Result<()> {
        let mut state = self.state.locked();
        state.journal()?.append_applied(patch, dom)?;
        state.appended()
    }

    /// Journals `patch` before it is applied, for writers that persist first
    /// (a [`crate::SessionWriter`]). Follow up with
    /// [`JournalWriter::snapshot_if_due`] once the patch is applied.
    pub fn append_patch(&self, patch: &Patch) -> Result<()> {
        let mut state = self.state.locked();
        state.journal()?.append(patch)?;
        state.appended()
    }

    /// Writes a snapshot of `dom`, which must be at the last journaled patch,
    /// when [`JournalOptions::snapshot_every`] patches followed the
    /// last one.
    pub fn snapshot_if_due(&self, dom: &Dom) -> Result<()> {
        let mut state = self.state.locked();
        let journal = state.journal()?;
        if !journal.snapshot_due() {
            return Ok(());
        }
        journal.write_snapshot(dom)?;
        // A snapshot syncs everything before it.
        state.pending = 0;
        state.oldest_pending = None;
        Ok(())
    }

    /// See [`Journal::load_at`]: the DOM right after patch `seq`.
    pub fn load_at(&self, seq: u64) -> Result<Dom> {
        self.state.locked().journal()?.load_at(seq)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Fsyncs every pending patch.
    pub fn sync(&self) -> Result<()> {
        self.state.locked().sync()
    }

    /// Patches appended but not fsynced yet.
    pub fn pending(&self) -> usize {
        self.state.locked().pending
    }

    pub fn last_seq(&self) -> u64 {
        let state = self.state.locked();
        state.journal.as_ref().map_or(state.last_seq, Journal::last_seq)
    }

    /// See [`Journal::patches`]. The returned iterator doesn't hold the lock.
    pub fn patches(&self, from_seq: u64) -> Result<Patches> {
        self.state.locked().journal()?.patches(from_seq)
    }

    /// See [`Journal::fork`].
    pub fn fork(&self, at_seq: u64, new_session_id: &str) -> Result<Journal> {
        self.state.locked().journal()?.fork(at_seq, new_session_id)
    }

    /// Syncs and releases the journal and its session lock, while others may
    /// still hold this writer: later appends fail with [`Error::Closed`]. The
    /// session can then be opened again or deleted.
    pub fn close(&self) -> Result<()> {
        let mut state = self.state.locked();
        if state.journal.is_none() {
            return Ok(());
        }
        state.sync()?;
        if let Some(journal) = state.journal.take() {
            state.last_seq = journal.last_seq();
        }
        Ok(())
    }

    /// Syncs and hands the journal back.
    pub fn into_journal(self) -> Result<Journal> {
        let mut state = self.state.into_inner().unwrap_or_else(PoisonError::into_inner);
        state.sync()?;
        state.journal.ok_or(Error::Closed)
    }
}

impl State {
    fn journal(&mut self) -> Result<&mut Journal> {
        self.journal.as_mut().ok_or(Error::Closed)
    }

    fn sync(&mut self) -> Result<()> {
        if let Some(journal) = &mut self.journal {
            journal.sync()?;
        }
        self.pending = 0;
        self.oldest_pending = None;
        Ok(())
    }

    /// Counts a journaled patch and fsyncs as the policy says.
    fn appended(&mut self) -> Result<()> {
        if !self.journal()?.has_unsynced() {
            // A snapshot synced everything.
            self.pending = 0;
            self.oldest_pending = None;
            return Ok(());
        }
        self.pending += 1;
        let oldest = *self.oldest_pending.get_or_insert_with(Instant::now);
        if self.strategy.after_append(self.pending, oldest) {
            self.sync()?;
        }
        Ok(())
    }
}

/// The [`Persist`] adapter of a live session backed by its journal.
/// Patches are journaled before the writer applies them; snapshots are
/// written after, from the writer's own DOM, so nothing keeps a second copy.
/// Clone the inner writer first to fork or read the journal while the
/// session is live.
pub struct JournalPersist(pub Arc<JournalWriter>);

impl Persist for JournalPersist {
    fn append(&mut self, patch: &Patch) -> std::result::Result<(), PersistError> {
        Ok(self.0.append_patch(patch)?)
    }

    fn applied(&mut self, dom: &Dom) {
        // The patch is already in the journal; a failed snapshot only means
        // the next load replays further. The next due patch retries it.
        let _ = self.0.snapshot_if_due(dom);
    }
}
