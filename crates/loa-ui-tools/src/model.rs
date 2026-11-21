//! Plain data the app feeds into the Tools panel.

use loa_contracts::harness::{HostToolSpec, McpServerSpec, McpTransport};

/// A tool as the panel lists it: a name, what it does, and optional
/// readiness details shown as sub rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    /// Sub rows under the tool, e.g. one per sandbox provider.
    pub details: Vec<ToolDetail>,
}

impl ToolInfo {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            details: Vec::new(),
        }
    }

    pub fn with_details(mut self, details: Vec<ToolDetail>) -> Self {
        self.details = details;
        self
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

/// One sub row under a tool: a label, its state and a short explanation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDetail {
    pub label: String,
    pub state: DetailState,
    pub text: String,
}

/// Identifies one tool for toggles and grants. MCP tools are always scoped
/// to their server, so a server tool never collides with a built in one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolId {
    /// A harness tool (`bash`, `read`, ...) or an app tool (`project_search`, ...).
    Builtin(String),
    Mcp {
        server: String,
        tool: String,
    },
}

impl ToolId {
    pub fn builtin(name: impl Into<String>) -> Self {
        Self::Builtin(name.into())
    }

    pub fn mcp(server: impl Into<String>, tool: impl Into<String>) -> Self {
        Self::Mcp {
            server: server.into(),
            tool: tool.into(),
        }
    }

    /// The tool's own name, without its server.
    pub fn name(&self) -> &str {
        match self {
            Self::Builtin(name) => name,
            Self::Mcp { tool, .. } => tool,
        }
    }
}

/// Where a built in tool comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinGroup {
    /// Tools the active harness ships (omp's `bash`, `read`, `edit`, `task`, ...).
    Harness,
    /// Tools the app adds to every harness (`project_search`, `project_read`, `sandbox_task`).
    App,
}

/// The two groups of the Built in section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuiltinTools {
    /// Display name of the active harness, shown as the group title.
    pub harness: String,
    pub harness_tools: Vec<ToolInfo>,
    pub app_tools: Vec<ToolInfo>,
    /// Shown under the harness group instead of its tools, e.g. while they
    /// load or when the harness doesn't report them.
    pub harness_note: Option<String>,
}

/// Run state of an MCP server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerStatus {
    Running,
    /// Spawning or connecting, at first or after a failure.
    Starting,
    Stopped,
    Error(String),
}

/// What kind of server this is, for marking and for which actions apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerKind {
    /// Registered by the user or imported.
    External,
    /// A surface server (browser, PDF, Office, VM) exposing `surface_*` tools.
    Surface,
    /// A server the app runs itself (`remote`). It cannot be removed.
    AppHosted,
}

/// One registered MCP server and the tools it declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub transport: McpTransport,
    pub status: ServerStatus,
    pub kind: ServerKind,
    pub tools: Vec<ToolInfo>,
}

impl McpServer {
    pub fn can_remove(&self) -> bool {
        self.kind != ServerKind::AppHosted
    }
}

/// A project the toggles can be scoped to.
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

/// Which tool config an import candidate was found in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImportSource {
    Claude,
    Cursor,
    Codex,
}

impl ImportSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => ".claude",
            Self::Cursor => ".cursor",
            Self::Codex => ".codex",
        }
    }
}

/// A server definition found in another tool's config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportCandidate {
    pub source: ImportSource,
    /// Config file it was read from, for display.
    pub path: String,
    pub server: McpServerSpec,
}

/// What happened to one import candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportOutcome {
    Imported,
    Skipped(String),
    Failed(String),
}

/// Limits the agent works under on one SSH host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostLimits {
    /// Paths `remote_file` may touch. Empty means none.
    pub allowed_paths: Vec<String>,
    pub read_only: bool,
    /// Whether `remote_bash` may run commands.
    pub commands_allowed: bool,
    /// Writes and commands run without an approval card.
    pub auto_approve: bool,
}

/// One SSH host the `remote` server can reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteHost {
    /// Name the agent and the user refer to the host by (the ssh config `Host`).
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
    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::List => "list",
            Self::Write => "write",
            Self::Edit => "edit",
            Self::Command => "command",
            Self::Terminal => "terminal",
        }
    }
}

/// One audit log row for a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRow {
    /// Time of the action, already formatted for display by the app.
    pub when: String,
    /// Session the action came from.
    pub session: String,
    pub action: AuditAction,
    /// The path or command.
    pub detail: String,
    /// How it ended: `ok`, `denied`, `refused`, `error`, ...
    pub outcome: String,
}

/// What a remote host question asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// A new or changed host key: trust it or not.
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

/// The user's answer to a [`RemotePrompt`].
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
    /// Every tool, built in and MCP.
    All,
    /// Every built in tool.
    AllBuiltin,
    /// Every tool of one MCP server.
    Server(String),
    /// One tool.
    Tool(ToolId),
}

impl ToolRule {
    pub fn allows(&self, tool: &ToolId) -> bool {
        match (self, tool) {
            (Self::All, _) => true,
            (Self::AllBuiltin, ToolId::Builtin(_)) => true,
            (Self::Server(name), ToolId::Mcp { server, .. }) => name == server,
            (Self::Tool(id), _) => id == tool,
            _ => false,
        }
    }
}

/// A peer and the tool clauses of its grant. Anything not allowed by a rule is denied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerTools {
    /// Node public key.
    pub key: String,
    pub name: String,
    pub rules: Vec<ToolRule>,
}

impl PeerTools {
    pub fn allows(&self, tool: &ToolId) -> bool {
        self.rules.iter().any(|rule| rule.allows(tool))
    }
}

/// Short text for a transport: the command line or the URL.
pub fn transport_label(transport: &McpTransport) -> String {
    match transport {
        McpTransport::Stdio { command, args, .. } => {
            let mut out = String::from(command);
            for arg in args {
                out.push(' ');
                out.push_str(arg);
            }
            out
        }
        McpTransport::Http { url, .. } => url.clone(),
    }
}
