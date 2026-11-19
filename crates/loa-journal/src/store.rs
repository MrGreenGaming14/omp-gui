//! All sessions under one root directory, one subdirectory per session.

use crate::error::{Error, IoContext, Result};
use crate::journal::{JOURNAL_FILE, Journal, JournalOptions, SessionInfo, Truncation, lock, validate_session_id};
use std::fs;
use std::path::{Path, PathBuf};

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
