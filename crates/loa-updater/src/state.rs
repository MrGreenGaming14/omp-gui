//! Persisted updater decisions in `<root>/state.json`.

pub use crate::model::HeldUpdate;
use crate::gates::GateReport;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Everything the updater remembers between runs, keyed by product id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdaterState {
    /// Versions the user pinned; no other version is offered.
    pub pins: BTreeMap<String, Version>,
    pub held: BTreeMap<String, HeldUpdate>,
    /// Waited-on versions: neither they nor anything older is offered again.
    pub waits: BTreeMap<String, Version>,
    pub last_check_unix: BTreeMap<String, u64>,
    /// Versions cleared for swapping at the next point between sessions.
    pub pending_swap: BTreeMap<String, Version>,
    pub reports: BTreeMap<String, GateReport>,
}

impl UpdaterState {
    /// Reads the state under `root`; a missing file is an empty state.
    pub fn load(root: &Path) -> crate::Result<Self> {
        Ok(crate::read_json(&root.join("state.json"))?.unwrap_or_default())
    }

    pub fn save(&self, root: &Path) -> crate::Result<()> {
        crate::write_json(&root.join("state.json"), self)
    }
}
