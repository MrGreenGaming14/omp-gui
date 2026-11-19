//! Plain data the Updates screen shows. The app fills it from its updater
//! service; versions are display strings.

/// What a product is to the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    App,
    Omp,
    Harness,
}

/// Where the version in use was found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Installed {
    /// Not looked for yet.
    #[default]
    Unknown,
    /// The running app.
    Running,
    /// A version the updater installed, at this path.
    Managed(String),
    /// An executable the updater didn't install, at this path.
    System(String),
    /// Not found, with the reason.
    Missing(String),
}

/// The updater's active pointer of a product.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Active {
    pub current: String,
    pub previous: Option<String>,
}

/// One gate check of a report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// `Tool schemas`, `Changelog`, …
    pub label: String,
    pub pass: bool,
    /// Diff lines, failures or flagged entries.
    pub details: Vec<String>,
}

/// Gate checks of one update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub from: String,
    pub to: String,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn harmful(&self) -> usize {
        self.checks.iter().filter(|c| !c.pass).count()
    }
}

/// An update held by a harmful report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub version: String,
    /// Number of harmful checks.
    pub harmful: usize,
}

/// Signature of a staged app update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signature {
    Verified,
    Unsigned,
    NoTrustedKeys,
    Rejected(String),
}

/// One watched product.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Product {
    pub id: String,
    pub label: String,
    pub kind: Kind,
    /// The feed, e.g. `GitHub can1357/oh-my-pi`.
    pub source: String,
    /// Version in use.
    pub current: Option<String>,
    pub installed: Installed,
    /// Version offered over `current`, after pins and waits.
    pub offer: Option<String>,
    /// Highest version the feed lists.
    pub newest: Option<String>,
    /// Release notes up to the offer.
    pub changelog: String,
    pub last_check_unix: Option<u64>,
    /// Staged versions, ascending.
    pub staged: Vec<String>,
    pub active: Option<Active>,
    /// Last gate report.
    pub report: Option<Report>,
    pub held: Option<Held>,
    /// Version cleared to swap in between sessions.
    pub pending_swap: Option<String>,
    pub pin: Option<String>,
    /// Versions up to this one are skipped.
    pub waiting_past: Option<String>,
    pub signature: Option<Signature>,
    /// The app swapped to a version it isn't running yet.
    pub restart_required: bool,
    /// Last error of a step on this product.
    pub error: Option<String>,
}

/// A peer's app version relative to this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    Current,
    /// Behind by the most significant differing component.
    Behind { majors: u64, minors: u64, patches: u64 },
    Ahead,
    Unknown,
}

/// One peer in the "peers behind" list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub node: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub standing: Standing,
    /// Protocol families with the highest version both sides speak.
    pub protocols: Vec<(String, Option<u32>)>,
    /// Every needed protocol family has a shared version.
    pub can_talk: bool,
}

/// The user's answer to a held update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    UpdateAnyway,
    PinCurrent,
    Wait,
}
