//! Products the updater watches and the releases their feeds publish.

use semver::Version;
use serde::{Deserialize, Serialize};

/// Where a product's releases are published.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Feed {
    /// GitHub releases of `owner/repo`.
    GitHub { owner: String, repo: String },
    /// Versions of an npm package.
    Npm { package: String },
}

/// What a product is to the app. Decides which gate checks and signature
/// rules apply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "harness", rename_all = "lowercase")]
pub enum ProductKind {
    /// The app itself. Its updates must be signed.
    App,
    /// omp, driven over RPC.
    Omp,
    /// Another harness an adapter drives, by harness name.
    Harness(String),
}

/// Whether prereleases (`1.2.0-beta.1`) are offered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrereleasePolicy {
    /// Stable releases only.
    #[default]
    Stable,
    /// Prereleases too.
    Include,
}

/// One watched product.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Product {
    /// Stable id used in paths and state (`app`, `omp`, `claude`).
    /// Must be a single path component.
    pub id: String,
    pub kind: ProductKind,
    pub feed: Feed,
    #[serde(default)]
    pub prereleases: PrereleasePolicy,
    /// Name of the release asset to install for this platform. For GitHub
    /// feeds the asset whose name equals this after replacing `{version}`
    /// with the version (without a leading `v`). Ignored for npm feeds,
    /// which always install the package tarball.
    #[serde(default)]
    pub asset: Option<String>,
}

/// Expected digest of a downloaded artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "algorithm", content = "hex", rename_all = "lowercase")]
pub enum Checksum {
    /// GitHub's asset `digest` (`sha256:<hex>`) or a `.sha256` sidecar asset.
    Sha256(String),
    /// npm's `dist.integrity` (`sha512-<base64>`), stored here as hex.
    Sha512(String),
}

/// How an artifact is unpacked into the version directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    TarGz,
    Zip,
    /// An npm package tarball: a `.tgz` whose entries sit under `package/`.
    NpmTarball,
    /// A single file (a binary), copied as is.
    Raw,
}

/// A downloadable file of a release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub name: String,
    pub url: String,
    /// Size in bytes as the feed reports it.
    pub size: Option<u64>,
    pub checksum: Option<Checksum>,
    pub format: ArchiveFormat,
    /// URL of the detached signature (`<name>.sig`) when the release has one.
    pub signature_url: Option<String>,
}

/// A release of one product, as a feed reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// [`Product::id`].
    pub product: String,
    pub version: Version,
    /// Release notes or changelog text for this version, possibly empty.
    #[serde(default)]
    pub notes: String,
    /// RFC 3339 publication time when the feed gives one.
    #[serde(default)]
    pub published_at: Option<String>,
    /// The artifact to install for this platform, when the release has one.
    pub artifact: Option<Artifact>,
}

/// A feed's last response, kept for conditional requests (`If-None-Match`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFeed {
    pub etag: Option<String>,
    pub releases: Vec<Release>,
}
