//! The Updates screen: what each watched product runs and what its feed offers, staged
//! versions, gate reports with the choices of a held update, rollback, app signatures and the
//! peers that are behind. Versions are their semver text.

/// What a product is to the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProductKind {
    App,
    Omp,
    Harness(String),
}

/// Where a product's version in use was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    Unknown,
    /// This app.
    Running,
    /// Installed by the updater at this path.
    Managed(String),
    /// Found on the system at this path.
    System(String),
    Missing(String),
}

/// Result of checking a staged app release's signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureStatus {
    Verified,
    Unsigned,
    NoTrustedKeys,
    Rejected(String),
}

/// The version in use and the one a rollback returns to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Active {
    pub current: String,
    pub previous: Option<String>,
}

/// Which gate check a result belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Harmful,
}

/// Outcome of one gate check with readable lines explaining it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckResult {
    pub check: GateCheck,
    pub verdict: Verdict,
    pub details: Vec<String>,
}

/// All gate results for one staged update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateReport {
    pub from: String,
    pub to: String,
    pub checks: Vec<CheckResult>,
}

impl GateReport {
    pub fn harmful(&self) -> impl Iterator<Item = &CheckResult> { self.checks.iter().filter(|c| c.verdict == Verdict::Harmful) }
}

/// An update held by a harmful gate report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldUpdate {
    pub version: String,
    pub report: GateReport,
}

/// One product on the Updates screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Product {
    pub id: String,
    pub label: String,
    pub kind: ProductKind,
    /// The feed, e.g. `GitHub can1357/oh-my-pi`.
    pub source: String,
    pub current: Option<String>,
    pub installed: Installed,
    /// The version the feed offers.
    pub offer: Option<String>,
    pub newest: Option<String>,
    /// Release notes between `current` and the offer, oldest first.
    pub changelog: String,
    pub last_check_unix: Option<u64>,
    pub staged: Vec<String>,
    pub active: Option<Active>,
    pub report: Option<GateReport>,
    pub held: Option<HeldUpdate>,
    pub pending_swap: Option<String>,
    pub pin: Option<String>,
    pub waiting_past: Option<String>,
    pub signature: Option<SignatureStatus>,
    pub restart_required: bool,
    pub error: Option<String>,
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

/// A peer's app version relative to this node's; `Behind` counts only the most significant differing part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    Current,
    Behind { majors: u64, minors: u64, patches: u64 },
    Ahead,
    Unknown,
}

/// One protocol family and the highest version both sides speak.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolStatus {
    pub family: String,
    pub shared: Option<u32>,
}

/// One row of the peers list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerStanding {
    pub node: String,
    pub name: Option<String>,
    pub app_version: Option<String>,
    pub standing: Standing,
    pub protocols: Vec<ProtocolStatus>,
    /// Whether the peer and this node share the protocols they need to talk.
    pub can_talk: bool,
}

/// The app's snapshot the screen shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Data {
    pub products: Vec<Product>,
    /// GitHub API requests left in the current window, when known.
    pub github_requests_left: Option<u64>,
    pub peers: Vec<PeerStanding>,
    /// Whether a session is mid turn: swaps wait for it.
    pub turn_running: bool,
    /// Current Unix time, for "checked 5m ago".
    pub now_unix: u64,
}

/// A request for the app's updater.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    CheckAll,
    Check(String),
    /// Download and unpack the offer beside the version in use.
    Stage(String),
    /// Run the staged offer's gate checks.
    Gate(String),
    /// Check the staged app release's signature.
    Verify(String),
    /// The answer to a held update.
    Choose(String, HoldChoice),
    /// Swap in the pending version (between turns only).
    Swap(String),
    Rollback(String),
    /// Drop a product's pin and wait.
    Unpin(String),
}
