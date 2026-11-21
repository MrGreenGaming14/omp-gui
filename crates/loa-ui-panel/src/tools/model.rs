//! Plain data the app feeds into the tools page.

use loa_contracts::harness::{HostToolSpec, McpServerSpec, McpTransport};

/// A tool as listed: a name, what it does, and readiness details shown under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    /// E.g. one per sandbox provider.
    pub details: Vec<ToolDetail>,
}

impl ToolInfo {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self { Self { name: name.into(), description: description.into(), details: Vec::new() } }
    pub fn with_details(self, details: Vec<ToolDetail>) -> Self { Self { details, ..self } }
}

impl From<&HostToolSpec> for ToolInfo {
    fn from(spec: &HostToolSpec) -> Self { Self::new(spec.name.clone(), spec.description.clone()) }
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

/// One tool for toggles and grants; MCP tools are scoped to their server, so they never
/// collide with built in ones.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolId {
    /// A harness tool (`bash`, `read`) or an app tool (`project_search`).
    Builtin(String),
    Mcp { server: String, tool: String },
}

impl ToolId {
    pub fn builtin(name: impl Into<String>) -> Self { Self::Builtin(name.into()) }
    pub fn mcp(server: impl Into<String>, tool: impl Into<String>) -> Self { Self::Mcp { server: server.into(), tool: tool.into() } }
    /// The tool's own name, without its server.
    pub fn name(&self) -> &str {
        match self {
            Self::Builtin(name) | Self::Mcp { tool: name, .. } => name,
        }
    }
}

/// The two groups of the Built in section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinGroup {
    /// Tools the active harness ships.
    Harness,
    /// Tools the app adds to every harness.
    App,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuiltinTools {
    /// The active harness's name, the group title.
    pub harness: String,
    pub harness_tools: Vec<ToolInfo>,
    pub app_tools: Vec<ToolInfo>,
    /// Shown instead of the harness tools, e.g. while they load.
    pub harness_note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerStatus {
    Running,
    /// Spawning or connecting, at first or after a failure.
    Starting,
    Stopped,
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerKind {
    /// Registered by the user or imported.
    External,
    /// A surface server exposing `surface_*` tools.
    Surface,
    /// Run by the app itself (`remote`); it cannot be removed and has no run controls.
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

impl McpServer {
    pub fn can_remove(&self) -> bool { self.kind != ServerKind::AppHosted }
}

/// A project the toggles are scoped to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRef {
    pub id: String,
    pub name: String,
}

impl std::fmt::Display for ProjectRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.name) }
}

/// The tool config an import candidate was found in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImportSource {
    Claude,
    Cursor,
    Codex,
}

impl ImportSource {
    pub fn label(self) -> &'static str { [".claude", ".cursor", ".codex"][self as usize] }
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

/// Limits the agent works under on one SSH host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostLimits {
    /// Paths `remote_file` may touch; empty means none.
    pub allowed_paths: Vec<String>,
    pub read_only: bool,
    /// `remote_bash` may run commands.
    pub commands_allowed: bool,
    /// Writes and commands run without an approval card.
    pub auto_approve: bool,
}

/// An SSH host the `remote` server reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteHost {
    /// The ssh config `Host`, the name agent and user use.
    pub alias: String,
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub proxy_jump: Option<String>,
    pub limits: HostLimits,
}

/// What a logged remote action did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditAction {
    Read,
    List,
    Write,
    Edit,
    Command,
    /// The user opened a terminal to the host.
    Terminal,
}

impl AuditAction {
    pub fn label(self) -> &'static str { ["read", "list", "write", "edit", "command", "terminal"][self as usize] }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRow {
    /// Already formatted by the app.
    pub when: String,
    pub session: String,
    pub action: AuditAction,
    /// The path or command.
    pub detail: String,
    /// `ok`, `denied`, `refused`, `error`, ...
    pub outcome: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// A new or changed host key to trust or not.
    HostKey,
    /// A write or command the agent wants to run.
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
    /// Every tool of one MCP server.
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
    pub fn allows(&self, tool: &ToolId) -> bool { self.rules.iter().any(|r| r.allows(tool)) }
}

/// The command line or the URL of a transport.
pub fn transport_label(t: &McpTransport) -> String {
    match t {
        McpTransport::Stdio { command, args, .. } => std::iter::once(command).chain(args).cloned().collect::<Vec<_>>().join(" "),
        McpTransport::Http { url, .. } => url.clone(),
    }
}
