//! The contract every harness adapter implements.
//!
//! An [`Adapter`] starts, resumes and forks sessions of one harness and
//! converts its native session files. A running session is a
//! [`HarnessSession`]: the app sends prompts and answers into it, and the
//! adapter turns everything the harness does into drafts committed to the
//! session's [`PatchSink`]. The DOM is the only channel back to the app.
//!
//! Who writes what (see `docs/ARCHITECTURE.md`, "DOM conventions"):
//! - `start` and `import` insert the `<session>` root
//! - `send` inserts the `<turn>` and the user `<message>`
//! - the adapter writes everything the harness reports, including decision
//!   answers passed to [`HarnessSession::answer`]

mod capabilities;

pub use capabilities::{Capabilities, ContextDelivery, Feature, ModelSwitch};

use async_trait::async_trait;
use loa_core::{Dom, NodeId, PatchSink, SessionMode};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Semantic version of this interface. Bump the major on a breaking change.
pub const INTERFACE_VERSION: &str = "0.1.0";

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// The harness has no native support. The app provides the feature on top.
    #[error("{harness} doesn't support {what}")]
    Unsupported { harness: &'static str, what: &'static str },
    /// A turn is running and the harness can't queue.
    #[error("a turn is already running")]
    Busy,
    #[error("the harness executable `{0}` wasn't found")]
    NotInstalled(String),
    /// The harness speaks a protocol or session format version the adapter can't handle.
    #[error("{harness} version mismatch: {detail}")]
    Version { harness: &'static str, detail: String },
    #[error("the session is closed")]
    Closed,
    #[error("unknown decision {0}")]
    UnknownDecision(NodeId),
    #[error("harness protocol error: {0}")]
    Protocol(String),
    #[error("malformed native session: {0}")]
    Format(String),
    #[error(transparent)]
    Commit(#[from] loa_core::CommitError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = AdapterError> = std::result::Result<T, E>;

/// How an MCP server is reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpTransport {
    Stdio { command: String, args: Vec<String>, env: BTreeMap<String, String> },
    Http { url: String, headers: BTreeMap<String, String> },
}

/// An MCP server the harness should connect to for this session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerSpec {
    pub name: String,
    pub transport: McpTransport,
}

/// A tool the app itself serves to the harness (`project_search`,
/// `project_read`, `sandbox_task`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments, as text.
    pub input_schema: String,
}

/// Runs host tool calls coming from the harness.
#[async_trait]
pub trait HostTools: Send + Sync {
    /// `args` is the JSON arguments as text. `Err` is shown to the model as a tool error.
    async fn call(&self, session_id: &str, tool: &str, args: &str) -> Result<String, String>;
}

/// Everything needed to start or attach a session.
#[derive(Clone, Default)]
pub struct SessionConfig {
    /// App-wide session id (UUID), written to `<session id>`.
    pub session_id: String,
    /// Public key of the owner node, written to `<session node>`.
    pub node: String,
    pub cwd: PathBuf,
    pub project: Option<String>,
    pub model: Option<String>,
    pub model_role: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<SessionMode>,
    /// Project context (digests, shared instructions) to deliver in the way
    /// [`Capabilities::context_delivery`] declares.
    pub context: Option<String>,
    pub mcp_servers: Vec<McpServerSpec>,
    pub host_tools: Vec<HostToolSpec>,
    /// Required when `host_tools` isn't empty.
    pub host_tool_handler: Option<Arc<dyn HostTools>>,
    /// Tool names switched off for this project.
    pub disabled_tools: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Harness executable to run instead of the one on `PATH`. The updater
    /// uses it to test a staged version.
    pub executable: Option<PathBuf>,
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConfig")
            .field("session_id", &self.session_id)
            .field("node", &self.node)
            .field("cwd", &self.cwd)
            .field("project", &self.project)
            .field("model", &self.model)
            .field("mode", &self.mode)
            .field("host_tools", &self.host_tools.len())
            .field("mcp_servers", &self.mcp_servers.len())
            .finish_non_exhaustive()
    }
}

/// A file or image sent along with a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub path: PathBuf,
    pub mime: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prompt {
    pub text: String,
    pub attachments: Vec<Attachment>,
    /// Node key or user that wrote it, recorded as `<message by>`.
    pub by: String,
}

/// The response to a `<decision>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    /// Picked options, typed text, or a constant from [`loa_core::answer`].
    /// A form answer is one JSON object as text.
    pub values: Vec<String>,
    /// Deny reason, reject feedback, or the edited plan.
    pub reason: Option<String>,
    /// Node key or user that answered, recorded as `<decision by>`.
    pub by: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSelection {
    pub model: String,
    /// Model role to assign (`default`, `smol`, `slow`, …). `None` sets the
    /// session's current model.
    pub role: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    /// A turn is running.
    Running,
    /// A turn is blocked on a `<decision>`.
    Waiting,
    Closed,
}

/// One harness.
#[async_trait]
pub trait Adapter: Send + Sync {
    /// Stable id written to `<session harness>`: `omp`, `omp2`, `claude-code`,
    /// `codex`, `opencode`, `acp`.
    fn harness(&self) -> &'static str;

    /// What the harness does natively. The app provides the rest on top.
    fn capabilities(&self) -> Capabilities;

    /// Starts a new session. Inserts the `<session>` root into `sink`, which
    /// holds an empty DOM.
    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>>;

    /// Reattaches to a session whose DOM, `dom`, was loaded from its journal.
    /// `sink` continues that DOM.
    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>>;

    /// Starts the harness side of a fork. `dom` is the forked DOM, already cut
    /// at the fork point, and `sink` continues it. Only called when
    /// [`Capabilities::fork`] is set; otherwise the app seeds a fresh session
    /// with the translated history.
    async fn fork(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let _ = (config, dom, sink);
        Err(AdapterError::Unsupported { harness: self.harness(), what: "fork" })
    }

    /// Reads a native session file or directory into `sink` (an empty DOM),
    /// keeping original timestamps through [`PatchSink::commit_at`].
    fn import(&self, native: &Path, sink: &dyn PatchSink) -> Result<()> {
        let _ = (native, sink);
        Err(AdapterError::Unsupported { harness: self.harness(), what: "import" })
    }

    /// Writes `dom` as a native session at `out`. Importing the result gives
    /// an equal DOM, and exporting an imported session reproduces the file.
    fn export(&self, dom: &Dom, out: &Path) -> Result<()> {
        let _ = (dom, out);
        Err(AdapterError::Unsupported { harness: self.harness(), what: "export" })
    }
}

/// One running harness session. Methods take `&self` so the UI can interrupt
/// or answer while a turn streams.
#[async_trait]
pub trait HarnessSession: Send + Sync {
    /// Starts a turn. Returns once the harness accepted the prompt; progress
    /// arrives as patches. `Busy` when a turn runs and the harness can't queue.
    async fn send(&self, prompt: Prompt) -> Result<()>;

    /// Stops the running turn. No-op when idle.
    async fn interrupt(&self) -> Result<()>;

    /// Answers the waiting `<decision>` node and records the answer in the DOM.
    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()>;

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        let _ = selection;
        Err(AdapterError::Unsupported { harness: self.harness(), what: "set_model" })
    }

    async fn set_mode(&self, mode: SessionMode) -> Result<()> {
        let _ = mode;
        Err(AdapterError::Unsupported { harness: self.harness(), what: "set_mode" })
    }

    /// Asks the harness to compact. It records a `<compaction>` element.
    async fn compact(&self, instructions: Option<String>) -> Result<()> {
        let _ = instructions;
        Err(AdapterError::Unsupported { harness: self.harness(), what: "compact" })
    }

    fn harness(&self) -> &'static str;

    fn state(&self) -> SessionState;

    /// Resolves when no turn is running or waiting.
    async fn wait_idle(&self) -> Result<()>;

    /// Stops the harness process. The session can be resumed later.
    async fn shutdown(&self) -> Result<()>;
}

/// The adapters built into the app, by harness id.
#[derive(Default)]
pub struct Registry {
    adapters: BTreeMap<&'static str, Arc<dyn Adapter>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `adapter`, replacing one with the same harness id.
    pub fn register(&mut self, adapter: Arc<dyn Adapter>) {
        self.adapters.insert(adapter.harness(), adapter);
    }

    pub fn get(&self, harness: &str) -> Option<Arc<dyn Adapter>> {
        self.adapters.get(harness).cloned()
    }

    pub fn harnesses(&self) -> impl Iterator<Item = &'static str> {
        self.adapters.keys().copied()
    }
}
