//! The harness port: adapters start sessions that write the harness's activity into the DOM.

mod account;
mod capabilities;
mod host_tools;

pub use crate::availability::{Availability, AvailabilityStatus};
pub use crate::tools::OutputBlock;
pub use account::{
    CommandSpec, HarnessAccount, HarnessFactory, HarnessTraits, LoginControl, LoginKind, LoginStrategy, LoginUpdate,
    ModelSettings, SignInMethod, SignInState,
};
pub use capabilities::{Capabilities, CapsBuilder, ContextDelivery, Feature, ModelSwitch};
pub use host_tools::{DuplicateHostTool, HostToolRouter};

use async_trait::async_trait;
use loa_core::{Dom, NodeId, PatchSink, SessionMode};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Semver of this interface: major on breaking changes, minor on additions.
pub const INTERFACE_VERSION: &str = "0.3.0";

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("{harness} doesn't support {what}")]
    Unsupported { harness: &'static str, what: &'static str },
    #[error("a turn is already running")]
    Busy,
    #[error("the harness executable `{0}` wasn't found")]
    NotInstalled(String),
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

fn unsupported(harness: &'static str, what: &'static str) -> AdapterError {
    AdapterError::Unsupported { harness, what }
}

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

/// A tool the app serves to the harness; `input_schema` is JSON Schema text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: String,
}

/// Runs host tool calls coming from the harness; `Err` reaches the model as a tool error.
#[async_trait]
pub trait HostTools: Send + Sync {
    async fn call(&self, session_id: &str, tool: &str, args: &str) -> Result<String, String>;

    /// Like `call`, keeping images as image blocks.
    async fn call_content(&self, session_id: &str, tool: &str, args: &str) -> Result<Vec<OutputBlock>, String> {
        self.call(session_id, tool, args).await.map(|text| vec![OutputBlock::Text(text)])
    }
}

/// Everything needed to start or attach a session.
#[derive(Clone, Default)]
pub struct SessionConfig {
    /// App-wide session id, written to `<session id>`.
    pub session_id: String,
    /// Owner node key, written to `<session node>`.
    pub node: String,
    pub cwd: PathBuf,
    pub project: Option<String>,
    pub model: Option<String>,
    pub model_role: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<SessionMode>,
    /// Harness-specific approval mode (omp `--approval-mode`); `None` keeps the harness's.
    pub approval_mode: Option<String>,
    /// Project context, delivered as `Capabilities::context_delivery` declares.
    pub context: Option<String>,
    pub mcp_servers: Vec<McpServerSpec>,
    pub host_tools: Vec<HostToolSpec>,
    /// Required when `host_tools` isn't empty.
    pub host_tool_handler: Option<Arc<dyn HostTools>>,
    pub disabled_tools: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Executable to run instead of the one on `PATH` (the updater tests staged versions).
    pub executable: Option<PathBuf>,
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("SessionConfig");
        d.field("session_id", &self.session_id).field("node", &self.node).field("cwd", &self.cwd);
        d.field("project", &self.project).field("model", &self.model).field("mode", &self.mode);
        d.field("approval_mode", &self.approval_mode).field("host_tools", &self.host_tools.len());
        d.field("mcp_servers", &self.mcp_servers.len()).finish_non_exhaustive()
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
    /// Recorded as `<message by>`.
    pub by: String,
}

/// The response to a `<decision>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    /// Picked options, typed text, a `loa_core::answer` constant, or one JSON form object.
    pub values: Vec<String>,
    /// Deny reason, reject feedback, or the edited plan.
    pub reason: Option<String>,
    /// Recorded as `<decision by>`.
    pub by: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSelection {
    pub model: String,
    /// Role to assign (`default`, `smol`, …); `None` sets the session's model.
    pub role: Option<String>,
}

/// A model the running session can switch to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessModel {
    /// What `set_model` takes (omp: `provider/id`).
    pub id: String,
    pub name: String,
    pub provider: String,
    /// Accepted effort levels, lowest first; empty without an effort setting.
    pub efforts: Vec<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Running,
    /// Blocked on a `<decision>`.
    Waiting,
    Closed,
}

/// One harness: starts, resumes and forks sessions and converts native session files.
#[async_trait]
pub trait Adapter: Send + Sync {
    /// Stable id written to `<session harness>` (`omp`, `claude-code`, `codex`, …).
    fn harness(&self) -> &'static str;

    async fn availability(&self) -> Availability {
        Availability::unavailable(AvailabilityStatus::Unknown, format!("{} doesn't report availability", self.harness()))
    }

    fn capabilities(&self) -> Capabilities;

    /// Starts a session, inserting the `<session>` root into the empty `sink`.
    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>>;

    /// Reattaches to a session whose journal loaded into `dom`; `sink` continues it.
    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>>;

    /// Starts the harness side of a fork of `dom`, already cut at the fork point.
    async fn fork(&self, _config: SessionConfig, _dom: &Dom, _sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        Err(unsupported(self.harness(), "fork"))
    }

    /// Reads a native session into the empty `sink`, keeping original timestamps.
    fn import(&self, _native: &Path, _sink: &dyn PatchSink) -> Result<()> { Err(unsupported(self.harness(), "import")) }

    /// Writes `dom` as a native session that imports back to an equal DOM.
    fn export(&self, _dom: &Dom, _out: &Path) -> Result<()> { Err(unsupported(self.harness(), "export")) }
}

/// One running session; `&self` methods so the UI can interrupt or answer mid turn.
#[async_trait]
pub trait HarnessSession: Send + Sync {
    /// Starts a turn once the harness accepts it; `Busy` if it can't queue.
    async fn send(&self, prompt: Prompt) -> Result<()>;

    /// Stops the running turn; no-op when idle.
    async fn interrupt(&self) -> Result<()>;

    /// Answers the waiting `<decision>` and records the answer.
    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()>;

    async fn set_model(&self, _selection: ModelSelection) -> Result<()> { Err(unsupported(self.harness(), "set_model")) }

    async fn set_mode(&self, _mode: SessionMode) -> Result<()> { Err(unsupported(self.harness(), "set_mode")) }

    /// Models this session can switch to, listed by the harness without spending tokens.
    async fn models(&self) -> Result<Vec<HarnessModel>> { Err(unsupported(self.harness(), "listing models")) }

    /// Sets the effort for following turns and records it on the session.
    async fn set_effort(&self, _effort: String) -> Result<()> { Err(unsupported(self.harness(), "set_effort")) }

    /// Compacts the context, recording a `<compaction>`.
    async fn compact(&self, _instructions: Option<String>) -> Result<()> { Err(unsupported(self.harness(), "compact")) }

    fn harness(&self) -> &'static str;

    fn state(&self) -> SessionState;

    /// Resolves when no turn is running or waiting.
    async fn wait_idle(&self) -> Result<()>;

    /// Stops the harness process; the session can be resumed later.
    async fn shutdown(&self) -> Result<()>;
}

/// The adapters built into the app, by harness id.
pub type Registry = loa_util::registry::Registry<dyn Adapter>;
