//! The project screen: shared instructions, pinned files, the digest model and the project's
//! sessions with their digests and context toggle.

/// A file every chat of the project should know about, with why it is pinned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PinnedFile {
    pub path: String,
    pub note: Option<String>,
}

/// Settings shared by every session of a project; `digest_model` `None` uses the summarizer's default. `folder`,
/// where the project's new chats start, isn't edited with the rest and is kept as stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectSettings {
    pub instructions: String,
    pub pinned: Vec<PinnedFile>,
    pub digest_model: Option<String>,
    pub folder: Option<String>,
}

/// Whether a session's digest is written and current.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DigestState {
    Missing,
    Fresh,
    Stale,
}

/// What the screen shows of a session's digest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Digest {
    pub summary: String,
    pub decisions: Vec<String>,
}

/// One session on the project screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub harness: Option<String>,
    /// Unix milliseconds of the last activity.
    pub last_active: i64,
    pub turns: usize,
    pub excluded: bool,
    pub digest_state: DigestState,
    pub digest: Option<Digest>,
    pub digest_model: Option<String>,
}

/// Everything the project screen shows; sessions newest activity first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectOverview {
    pub cwd: Option<String>,
    pub settings: ProjectSettings,
    pub sessions: Vec<SessionRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OmpHistoryRow {
    pub id: String,
    pub title: String,
    /// Unix milliseconds of the last write.
    pub modified: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OmpHistory {
    Loading,
    Ready(Vec<OmpHistoryRow>),
    Failed(String),
}

/// What the app does for the screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    /// Store these settings and report the result back to the screen.
    SaveSettings { project: String, settings: ProjectSettings },
    /// Record a session's context toggle; answer with a fresh overview.
    SetExcluded { session: String, excluded: bool },
    OpenSession(String),
    /// Import and open a listed OMP terminal conversation by its native id.
    OpenOmpHistory(String),
    /// Start a chat in this project, in its working directory when known.
    NewChat { project: String, cwd: Option<String> },
    /// Show a folder dialog for the project's folder, starting at the current one; the app stores the choice and
    /// sends a fresh overview.
    ChangeFolder { project: String, current: Option<String> },
    /// Delete the project, confirmed; its sessions are kept.
    Delete { project: String },
}

/// What the New project screen asks the app to do.
#[derive(Clone, Debug, PartialEq)]
pub enum CreateIntent {
    /// Show a folder dialog starting at this folder; the app answers with the folder picked.
    ChooseFolder(String),
    /// Create the project; the app answers a failure on the screen.
    Create { name: String, settings: ProjectSettings },
    Cancel,
}
