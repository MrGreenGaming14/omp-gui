//! Right panel panes: diff, files, session tree, agents and tools; their read models and what they ask the app to do.

pub use loa_contracts::harness::{McpServerSpec, McpTransport};

use loa_contracts::harness::HostToolSpec;
use loa_core::NodeId;
use std::path::PathBuf;

/// What the diff pane asks: open this path, the new one, or the old one of a deleted file.
#[derive(Clone, Debug, PartialEq)]
pub enum DiffIntent {
    OpenFile(String),
}

/// What the files pane asks: open the file at this absolute path.
#[derive(Clone, Debug, PartialEq)]
pub enum FilesIntent {
    Open(PathBuf),
}

/// A session of a fork family: `parent` is `parent_session`, `forked_at` the parent's sequence the fork starts after.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub parent: Option<String>,
    pub forked_at: Option<u64>,
    pub title: String,
    pub turns: Vec<TurnInfo>,
}

/// A turn element, the target of rewind and fork, and the sequence that inserted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnInfo {
    pub node: NodeId,
    pub seq: u64,
    pub first_line: String,
}

/// What the session tree asks; `Rewind` rewinds `session` to just before `turn`.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionTreeIntent {
    Rewind { session: String, turn: NodeId },
    Fork { session: String, turn: NodeId },
    Open(String),
}

/// What the agents pane asks; `Cancel` stops a running job, `id` is the harness's job id when known.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentsIntent {
    OpenSession(String),
    DetachSession(String),
    Cancel { node: NodeId, id: Option<String> },
}

/// A tool as listed: a name, what it does, and readiness details (e.g. one per sandbox provider).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub details: Vec<ToolDetail>,
}

impl ToolInfo {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self { name: name.into(), description: description.into(), details: Vec::new() }
    }

    pub fn with_details(self, details: Vec<ToolDetail>) -> Self {
        Self { details, ..self }
    }
}

impl From<&HostToolSpec> for ToolInfo {
    fn from(spec: &HostToolSpec) -> Self {
        Self::new(spec.name.clone(), spec.description.clone())
    }
}

/// Readiness of one part a tool depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailState {
    Ready,
    NotConfigured,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDetail {
    pub label: String,
    pub state: DetailState,
    pub text: String,
}

/// One tool for toggles and grants: a harness or app tool, or an MCP tool scoped to its server.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolId {
    Builtin(String),
    Mcp { server: String, tool: String },
}

impl ToolId {
    pub fn builtin(name: impl Into<String>) -> Self {
        Self::Builtin(name.into())
    }

    pub fn mcp(server: impl Into<String>, tool: impl Into<String>) -> Self {
        Self::Mcp { server: server.into(), tool: tool.into() }
    }

    /// The tool's own name, without its server.
    pub fn name(&self) -> &str {
        let (Self::Builtin(name) | Self::Mcp { tool: name, .. }) = self;
        name
    }
}

/// The two groups of the Built in section: the active harness's tools and the app's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinGroup {
    Harness,
    App,
}

/// `harness` names the active harness; `harness_note` shows instead of its tools, e.g. while they load.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuiltinTools {
    pub harness: String,
    pub harness_tools: Vec<ToolInfo>,
    pub app_tools: Vec<ToolInfo>,
    pub harness_note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerStatus {
    Running,
    Starting,
    Stopped,
    Error(String),
}

/// Who registered a server; app hosted ones (`remote`) can't be removed and have no run controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerKind {
    External,
    Surface,
    AppHosted,
}

/// A registered MCP server and the tools it declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub transport: McpTransport,
    pub status: ServerStatus,
    pub kind: ServerKind,
    pub tools: Vec<ToolInfo>,
}

/// A project the toggles are scoped to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRef {
    pub id: String,
    pub name: String,
}

impl std::fmt::Display for ProjectRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// The tool config an import candidate was found in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImportSource {
    Claude,
    Cursor,
    Codex,
}

impl ImportSource {
    pub fn label(self) -> &'static str {
        [".claude", ".cursor", ".codex"][self as usize]
    }
}

/// A server definition found in another tool's config file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportCandidate {
    pub source: ImportSource,
    pub path: String,
    pub server: McpServerSpec,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportOutcome {
    Imported,
    Skipped(String),
    Failed(String),
}

/// Limits on one SSH host: paths `remote_file` may touch, commands for `remote_bash`, and skipping approval cards.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostLimits {
    pub allowed_paths: Vec<String>,
    pub read_only: bool,
    pub commands_allowed: bool,
    pub auto_approve: bool,
}

/// An SSH host the `remote` server reaches by its ssh config `alias`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteHost {
    pub alias: String,
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub proxy_jump: Option<String>,
    pub limits: HostLimits,
}

/// What a logged remote action did; `Terminal` is the user opening a terminal to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditAction {
    Read,
    List,
    Write,
    Edit,
    Command,
    Terminal,
}

impl AuditAction {
    pub fn label(self) -> &'static str {
        ["read", "list", "write", "edit", "command", "terminal"][self as usize]
    }
}

/// One audit log entry: `when` formatted by the app, `detail` a path or command, `outcome` `ok`, `denied`, ...
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRow {
    pub when: String,
    pub session: String,
    pub action: AuditAction,
    pub detail: String,
    pub outcome: String,
}

/// A host key to trust, or a write or command the agent wants to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    HostKey,
    Approval,
}

/// A question from the `remote` server waiting for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemotePrompt {
    pub id: u64,
    pub kind: PromptKind,
    pub title: String,
    pub body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptAnswer {
    Trust,
    Reject,
    AllowOnce,
    AllowSession,
    Deny,
}

/// One clause of a peer's grant that allows tools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolRule {
    All,
    AllBuiltin,
    Server(String),
    Tool(ToolId),
}

impl ToolRule {
    pub fn allows(&self, tool: &ToolId) -> bool {
        match (self, tool) {
            (Self::All, _) | (Self::AllBuiltin, ToolId::Builtin(_)) => true,
            (Self::Server(name), ToolId::Mcp { server, .. }) => name == server,
            (Self::Tool(id), _) => id == tool,
            _ => false,
        }
    }
}

/// A peer and its grant's tool clauses; anything no rule allows is denied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerTools {
    pub key: String,
    pub name: String,
    pub rules: Vec<ToolRule>,
}

impl PeerTools {
    pub fn allows(&self, tool: &ToolId) -> bool {
        self.rules.iter().any(|r| r.allows(tool))
    }
}

/// A run control on a registered MCP server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerAction {
    Start,
    Stop,
    Restart,
    Remove,
}

/// What the tools page asks; the app answers through the page's setters (disabled tools, imports, hosts, audit).
#[derive(Clone, Debug, PartialEq)]
pub enum ToolsIntent {
    ProjectSelected(String),
    SetToolsEnabled { project: String, tools: Vec<ToolId>, enabled: bool },
    Server { name: String, action: ServerAction },
    AddServer(McpServerSpec),
    AddHost(RemoteHost),
    SaveHostLimits { host: String, limits: HostLimits },
    ImportServers(Vec<ImportCandidate>),
    AnswerPrompt { id: u64, answer: PromptAnswer },
    ScanImports,
    ImportSshHosts,
    RemoveHost(String),
    OpenTerminal(String),
    LoadAudit(String),
}

/// Ports the panel panes need and the app implements.
pub mod port {
    use std::path::Path;

    /// A directory entry the files pane lists.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Entry {
        pub name: String,
        pub is_dir: bool,
    }

    /// Port the files tree reads directories through.
    pub trait FileSystem: Send + Sync {
        /// The direct children of `dir`, already filtered; the tree sorts them. The error is shown under the directory.
        fn list(&self, dir: &Path) -> Result<Vec<Entry>, String>;
    }
}
