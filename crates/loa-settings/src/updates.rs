//! Releases, gate reports and peer standings the Updates screen shows.

use semver::Version;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// What a product is to the app: decides gate checks and signature rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "harness", rename_all = "lowercase")]
pub enum ProductKind {
    App,
    Omp,
    Harness(String),
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

/// Rate limit state a feed reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    pub remaining: u64,
    pub reset_unix: u64,
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

/// Which gate check a result belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateCheck {
    ToolSchemas,
    SessionFormat,
    ConfigSchema,
    RpcProtocol,
    ContractTests,
    Changelog,
}

impl GateCheck {
    pub fn label(self) -> &'static str {
        match self {
            Self::ToolSchemas => "Tool schemas",
            Self::SessionFormat => "Session format version",
            Self::ConfigSchema => "Config schema",
            Self::RpcProtocol => "RPC protocol version",
            Self::ContractTests => "Adapter contract tests",
            Self::Changelog => "Changelog",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Harmful,
}

/// Outcome of one check with readable lines explaining it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub check: GateCheck,
    pub verdict: Verdict,
    pub details: Vec<String>,
}

impl CheckResult {
    pub fn pass(check: GateCheck, details: Vec<String>) -> Self {
        Self { check, verdict: Verdict::Pass, details }
    }

    pub fn harmful(check: GateCheck, details: Vec<String>) -> Self {
        Self { check, verdict: Verdict::Harmful, details }
    }

    /// Harmful when `details` is not empty.
    pub fn flag(check: GateCheck, details: Vec<String>) -> Self {
        if details.is_empty() { Self::pass(check, details) } else { Self::harmful(check, details) }
    }

    /// The check's port failed; counts as harmful.
    pub fn failed_to_run(check: GateCheck, error: impl fmt::Display) -> Self {
        Self::harmful(check, vec![format!("check could not run: {error}")])
    }
}

/// All gate results for one staged update.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateReport {
    pub product: String,
    pub from: Version,
    pub to: Version,
    pub checks: Vec<CheckResult>,
}

impl GateReport {
    pub fn is_clean(&self) -> bool {
        self.checks.iter().all(|c| c.verdict == Verdict::Pass)
    }

    pub fn harmful(&self) -> impl Iterator<Item = &CheckResult> {
        self.checks.iter().filter(|c| c.verdict == Verdict::Harmful)
    }
}

impl fmt::Display for GateReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} {} -> {}", self.product, self.from, self.to)?;
        for check in &self.checks {
            let verdict = if check.verdict == Verdict::Pass { "pass" } else { "HARMFUL" };
            writeln!(f, "  {}: {verdict}", check.check.label())?;
            check.details.iter().try_for_each(|line| writeln!(f, "    {line}"))?;
        }
        Ok(())
    }
}

/// The user's answer to a held update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldChoice {
    UpdateAnyway,
    /// Stay on the current version until unpinned.
    PinCurrent,
    /// Skip the held version and offer the next newer one.
    Wait,
}

/// What this node knows about a peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub node: String,
    pub name: Option<String>,
    pub app_version: Option<Version>,
    pub alpns: Vec<String>,
}

/// A peer's app version relative to this node's; `Behind` counts only the most significant differing part.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Standing {
    Current,
    Behind { majors: u64, minors: u64, patches: u64 },
    Ahead,
    Unknown,
}

/// One protocol family as both sides speak it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolStatus {
    pub family: String,
    /// The highest version with an exact label announced by both sides.
    pub shared: Option<u32>,
    /// The highest version this node announces, if any.
    pub local: Option<u32>,
    /// The highest version the peer announces, if any.
    pub peer: Option<u32>,
}

/// One row of the Peers list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerStanding {
    pub node: String,
    pub name: Option<String>,
    pub app_version: Option<Version>,
    pub standing: Standing,
    pub protocols: Vec<ProtocolStatus>,
    /// True when every family this node speaks, and at least control and
    /// session, has the same label on both sides.
    pub can_talk: bool,
}
