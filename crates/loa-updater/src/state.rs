//! Persisted updater decisions: pins, held updates, waits, check times and
//! pending swaps, stored as JSON at [`layout::state_path`].

use crate::fsio;
use crate::gates::GateReport;
use crate::layout;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("updater state {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("updater state {path} is not valid: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T, E = StateError> = std::result::Result<T, E>;

/// An update whose gate checks found something harmful, waiting for the
/// user to choose what to do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldUpdate {
    pub version: Version,
    pub report: GateReport,
    pub held_at_unix: u64,
}

/// Everything the updater remembers between runs, keyed by product id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdaterState {
    /// Versions the user pinned; no other version is offered.
    #[serde(default)]
    pub pins: BTreeMap<String, Version>,
    /// Updates held by harmful gate reports.
    #[serde(default)]
    pub held: BTreeMap<String, HeldUpdate>,
    /// Versions the user chose to wait on: neither they nor anything older
    /// is offered again.
    #[serde(default)]
    pub waits: BTreeMap<String, Version>,
    /// When each product's feed was last checked.
    #[serde(default)]
    pub last_check_unix: BTreeMap<String, u64>,
    /// Versions cleared for swapping at the next point between sessions.
    #[serde(default)]
    pub pending_swap: BTreeMap<String, Version>,
}

impl UpdaterState {
    /// Reads the state under `root`; a missing file is an empty state.
    pub fn load(root: &Path) -> Result<Self> {
        let path = layout::state_path(root);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(StateError::Io { path, source }),
        };
        serde_json::from_slice(&bytes).map_err(|source| StateError::Json { path, source })
    }

    /// Replaces the state file under `root` atomically.
    pub fn save(&self, root: &Path) -> Result<()> {
        let path = layout::state_path(root);
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|source| StateError::Json { path: path.clone(), source })?;
        fsio::write_atomic(&path, &bytes).map_err(|source| StateError::Io { path, source })
    }
}
