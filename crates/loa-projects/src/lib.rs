//! Session digests, project context, and the `project_search` / `project_read` tools.
//! Every path that shows sessions starts at [`source::visible_sessions`], so a denied session never leaks.

pub mod context;
pub mod digest;
pub mod digester;
pub mod links;
pub mod rank;
pub mod read;
pub mod rebuild;
pub mod search;
pub mod service;
pub mod sessions;
pub mod settings;
pub mod source;
pub mod tools;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

pub use digester::{Summarizer, Summarizers};
pub use rebuild::RebuildOutcome;
pub use service::{ProjectService, ProjectSummary, ServiceConfig};
pub use sessions::LiveSessions;
pub use settings::{FileSettingsStore, is_excluded};
pub use source::LocalOnly;
pub use tools::{PROJECT_READ, PROJECT_SEARCH};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A port (session source, settings store, writer) failed.
    #[error("{0}")]
    Port(String),
    /// Missing, in another project or denied: not told apart, so access isn't revealed.
    #[error("session {0} not found")]
    SessionNotFound(String),
    #[error("node {node} not found in session {session}")]
    NodeNotFound { session: String, node: u64 },
    #[error("summarizer failed: {0}")]
    Summarizer(String),
    #[error("invalid digest answer: {0}")]
    InvalidAnswer(String),
    #[error("the session has no turns to digest")]
    NothingToDigest,
    #[error("invalid tool arguments: {0}")]
    Arguments(String),
    #[error("the project name is empty")]
    EmptyName,
    #[error("a project named {0} already exists")]
    ProjectExists(String),
    /// A file the user also edits outside Loa changed after it was loaded; nothing was written.
    #[error("{} was changed outside Loa since it was loaded", path.display())]
    ChangedOnDisk { path: std::path::PathBuf },
    #[error(transparent)]
    Commit(#[from] loa_core::CommitError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
