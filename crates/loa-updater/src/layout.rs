//! On-disk layout of installed versions under the updater root.
//!
//! ```text
//! <root>/versions/<product>/<version>/payload/     unpacked files
//! <root>/versions/<product>/<version>/artifact/    downloaded file and its .sig
//! <root>/versions/<product>/<version>/staged.json  StagedManifest
//! <root>/versions/<product>/active.json            which version is active (swap)
//! <root>/state.json                                UpdaterState
//! ```
//!
//! A version directory is complete only once `staged.json` exists: staging
//! writes it last, so a half finished download is never mistaken for a
//! staged version.

use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Directory holding every version of `product`.
pub fn product_dir(root: &Path, product: &str) -> PathBuf {
    root.join("versions").join(product)
}

/// Directory of one version of `product`.
pub fn version_dir(root: &Path, product: &str, version: &Version) -> PathBuf {
    product_dir(root, product).join(version.to_string())
}

/// Persisted updater state file.
pub fn state_path(root: &Path) -> PathBuf {
    root.join("state.json")
}

pub const PAYLOAD_DIR: &str = "payload";
pub const ARTIFACT_DIR: &str = "artifact";
pub const MANIFEST_FILE: &str = "staged.json";
pub const ACTIVE_FILE: &str = "active.json";

/// Written to `staged.json` when a version is fully staged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedManifest {
    pub product: String,
    pub version: Version,
    /// File name of the downloaded artifact inside `artifact/`.
    pub artifact_name: String,
    /// SHA-256 of the artifact, hex.
    pub sha256: String,
    pub size: u64,
    /// File name of the detached signature inside `artifact/`, when the
    /// release had one.
    pub signature_name: Option<String>,
}

/// A fully staged version, ready for gate checks and swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    /// `<root>/versions/<product>/<version>`.
    pub dir: PathBuf,
    pub manifest: StagedManifest,
}

impl Staged {
    /// Unpacked files of this version.
    pub fn payload(&self) -> PathBuf {
        self.dir.join(PAYLOAD_DIR)
    }

    /// The downloaded artifact.
    pub fn artifact(&self) -> PathBuf {
        self.dir.join(ARTIFACT_DIR).join(&self.manifest.artifact_name)
    }

    /// The detached signature, when the release had one.
    pub fn signature(&self) -> Option<PathBuf> {
        self.manifest.signature_name.as_ref().map(|name| self.dir.join(ARTIFACT_DIR).join(name))
    }
}
