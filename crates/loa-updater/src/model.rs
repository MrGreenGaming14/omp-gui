//! Products and how the updater watches them; the status the Updates screen
//! renders lives in `loa_settings::updates`.

use crate::gates::OmpGates;
use loa_settings::updates::{ProductKind, Release};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where a product's releases are published.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Feed {
    GitHub { owner: String, repo: String },
    Npm { package: String },
}

impl Feed {
    /// Registry key of the feed source serving this feed.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::GitHub { .. } => "github",
            Self::Npm { .. } => "npm",
        }
    }
}

/// Whether prereleases (`1.2.0-beta.1`) are offered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrereleasePolicy {
    #[default]
    Stable,
    Include,
}

/// One watched product; `id` is a single path component.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Product {
    pub id: String,
    pub kind: ProductKind,
    pub feed: Feed,
    #[serde(default)]
    pub prereleases: PrereleasePolicy,
    /// GitHub asset name with `{version}` placeholders; npm always installs the tarball.
    #[serde(default)]
    pub asset: Option<String>,
}

/// A feed's last response, kept for conditional requests.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFeed {
    pub etag: Option<String>,
    pub releases: Vec<Release>,
}

/// Where the version in use comes from when the updater hasn't installed one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Installed {
    Running(Version),
    /// A bare name looked up on `PATH`, or a path.
    Executable(PathBuf),
}

/// Which gate checks a product's updates go through.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Gates {
    /// None: the app relies on its signature.
    None,
    Changelog,
    Omp(OmpGates),
}

/// One watched product and how the updater treats it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductSpec {
    pub product: Product,
    pub label: String,
    pub installed: Installed,
    /// Executable inside a staged payload (`{version}` replaced); `None` derives it.
    #[serde(default)]
    pub executable: Option<String>,
    pub gates: Gates,
}

/// Result of a swap or rollback attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwapOutcome {
    /// A turn runs; nothing changed.
    Deferred,
    AlreadyActive,
    Swapped { from: Option<Version>, to: Version },
}
