//! Session digests, project context, `project_search` and `project_read`.
//!
//! - [`digest`]: the `<digest>` element as typed data, its drafts and stale tracking
//! - [`digester`]: builds a digest of a session with omp's `smol` role
//! - [`rebuild`]: rebuilds stale digests at turn end or compaction, debounced
//! - [`rank`]: ranks a project's digests and packs them into a token budget
//! - [`context`]: renders project context and delivers it per harness
//! - [`search`], [`read`], [`tools`]: `project_search` and `project_read` host tools
//! - [`settings`]: shared instructions, pinned files and the per session exclude toggle
//! - [`source`]: the ports for loading sessions and checking access
//! - [`sessions`]: the app's live and stored sessions behind those ports
//! - [`service`]: [`ProjectService`], what the app holds: project listing,
//!   the project screen's data, rebuilds, new chat context and host tools
//!
//! Every path that shows other sessions to a requester goes through
//! [`source::visible_sessions`], so a session the requester can't open never
//! influences results, counts, snippets or digests.

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
mod text;

pub use context::{ContextRequest, Delivered, McpResource, PackedContext, build_context, deliver, render_markdown};
pub use digest::{Digest, DigestContent, DigestState, FileNote, digest_of, digest_state, mark_stale_draft, write_drafts};
pub use digester::{Digester, OmpSummarizer, PromptLimits, Summarizer, SummaryAnswer};
pub use rank::{Candidate, RankParams, Ranked, estimate_tokens, rank_and_pack};
pub use read::{ReadPage, ReadRequest, read_subtree};
pub use rebuild::{Decision, RebuildDriver, RebuildHandle, RebuildOutcome, SessionWriter, Trigger, decide};
pub use search::{AttrFilter, PathStep, SearchHit, SearchPage, SearchQuery, TextMode, search};
pub use service::{ProjectOverview, ProjectService, ProjectSummary, ServiceConfig, SessionRow};
pub use sessions::LiveSessions;
pub use settings::{
    FileSettingsStore, MemorySettingsStore, PinnedFile, ProjectSettings, SettingsStore, exclude_draft, is_excluded,
};
pub use source::{
    Access, LocalOnly, Requester, SessionInfo, SessionSource, project_name, session_project, visible_sessions,
};
pub use tools::{PROJECT_READ, PROJECT_SEARCH, ProjectTools};

/// Errors of this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A port (session source, settings store, writer) failed.
    #[error("{0}")]
    Port(String),
    /// The session doesn't exist, isn't in the project, or the requester
    /// can't open it. The three are not told apart so access isn't revealed.
    #[error("session {0} not found")]
    SessionNotFound(String),
    #[error("node {node} not found in session {session}")]
    NodeNotFound { session: String, node: u64 },
    /// The summarizer failed to answer.
    #[error("summarizer failed: {0}")]
    Summarizer(String),
    /// The summarizer answered, but not with a valid digest.
    #[error("invalid digest answer: {0}")]
    InvalidAnswer(String),
    /// The session has nothing to digest yet.
    #[error("the session has no turns to digest")]
    NothingToDigest,
    #[error("invalid tool arguments: {0}")]
    Arguments(String),
    /// A project was given a blank name.
    #[error("the project name is empty")]
    EmptyName,
    #[error(transparent)]
    Commit(#[from] loa_core::CommitError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
