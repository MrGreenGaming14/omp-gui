//! Products, releases and the status snapshot the Updates screen renders.

use crate::gates::{GateReport, OmpGates};
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

/// What a product is to the app: decides gate checks and signature rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "harness", rename_all = "lowercase")]
pub enum ProductKind {
    App,
    Omp,
    Harness(String),
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

/// Expected digest of a downloaded artifact, as hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "algorithm", content = "hex", rename_all = "lowercase")]
pub enum Checksum {
    Sha256(String),
    Sha512(String),
}

/// How an artifact is unpacked; `NpmTarball` is a `.tgz` rooted at `package/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    TarGz,
    Zip,
    NpmTarball,
    Raw,
}

/// A downloadable file of a release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub name: String,
    pub url: String,
    pub size: Option<u64>,
    pub checksum: Option<Checksum>,
    pub format: ArchiveFormat,
    pub signature_url: Option<String>,
}

/// A release of one product, as a feed reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub product: String,
    pub version: Version,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub published_at: Option<String>,
    pub artifact: Option<Artifact>,
}

/// A feed's last response, kept for conditional requests.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFeed {
    pub etag: Option<String>,
    pub releases: Vec<Release>,
}

/// Rate limit state a feed reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    pub remaining: u64,
    pub reset_unix: u64,
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

/// An update held by a harmful gate report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldUpdate {
    pub version: Version,
    pub report: GateReport,
    pub held_at_unix: u64,
}

/// Contents of a product's `active.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Active {
    pub current: Version,
    pub previous: Option<Version>,
}

/// Result of a swap or rollback attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwapOutcome {
    /// A turn runs; nothing changed.
    Deferred,
    AlreadyActive,
    Swapped { from: Option<Version>, to: Version },
}

/// Where a product's version in use was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstalledStatus {
    Unknown,
    Running,
    Managed(PathBuf),
    System(PathBuf),
    Missing(String),
}

/// Result of checking a staged artifact's signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureStatus {
    Verified,
    Unsigned,
    NoTrustedKeys,
    Rejected(String),
}

/// One product on the Updates screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProductStatus {
    pub id: String,
    pub label: String,
    pub kind: ProductKind,
    /// The feed, e.g. `GitHub can1357/oh-my-pi`.
    pub source: String,
    pub current: Option<Version>,
    pub installed: InstalledStatus,
    pub offer: Option<Release>,
    pub newest: Option<Version>,
    /// Release notes between `current` and the offer, oldest first.
    pub changelog: String,
    pub last_check_unix: Option<u64>,
    pub staged: Vec<Version>,
    pub active: Option<Active>,
    pub report: Option<GateReport>,
    pub held: Option<HeldUpdate>,
    pub pending_swap: Option<Version>,
    pub pin: Option<Version>,
    pub waiting_past: Option<Version>,
    pub signature: Option<SignatureStatus>,
    pub restart_required: bool,
    pub error: Option<String>,
}

/// Snapshot of every product for the Updates screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overview {
    pub products: Vec<ProductStatus>,
    pub github_rate_limit: Option<RateLimit>,
}
