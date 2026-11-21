//! Session digests, project context, and the `project_search` / `project_read` tools.
//!
//! Every path that shows sessions goes through [`source::visible_sessions`], so a session
//! the [`Access`] port denies never influences results, counts, snippets or digests.

pub mod context;
pub mod digest;
pub mod digester;
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

pub use context::{ContextRequest, PackedContext, build_context, render_markdown};
pub use digest::{Digest, DigestContent, DigestState, FileNote, digest_of, digest_state, mark_stale_draft, write_drafts};
pub use digester::{Digester, PromptLimits, Summarizer, Summarizers};
pub use loa_harness::omp::OneShot as OmpSummarizer;
pub use rank::{Bm25, Candidate, RankParams, Ranked, Recency, Scorer, Weighted, estimate_tokens, rank_and_pack};
pub use read::{ReadPage, ReadRequest, read_subtree};
pub use rebuild::{Decision, RebuildDriver, RebuildHandle, RebuildOutcome, SessionWriter, Trigger, decide};
pub use search::{AttrFilter, PathStep, SearchHit, SearchPage, SearchQuery, TextMode, search};
pub use service::{ProjectOverview, ProjectService, ProjectSummary, ServiceConfig, SessionRow};
pub use sessions::LiveSessions;
pub use settings::{
    FileSettingsStore, MemorySettingsStore, PinnedFile, ProjectSettings, SettingsStore, exclude_draft, is_excluded,
};
pub use source::{Access, LocalOnly, SessionInfo, SessionSource, project_name, session_project, visible_sessions};
pub use tools::{PROJECT_READ, PROJECT_SEARCH, ProjectRead, ProjectSearch, tool_set};

/// Errors of this crate.
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
    #[error(transparent)]
    Commit(#[from] loa_core::CommitError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
