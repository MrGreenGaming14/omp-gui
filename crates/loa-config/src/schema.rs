//! The typed config schema. Defaults live in serde attributes; the non-derivable
//! `Default`s deserialize an empty table, so the two can't drift apart.

use crate::codec::UnknownKey;
use loa_contracts::harness::{McpServerSpec, McpTransport, SessionConfig};
use loa_core::DecisionKind;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use toml::Table;

/// Schema version this build reads and writes.
pub const CURRENT_VERSION: u32 = 1;
/// The p2p port (UDP for iroh, TCP fallback). The one definition; loa-p2p re-exports it.
pub const DEFAULT_PORT: u16 = 31415;

macro_rules! serde_default {
    ($($ty:ty),*) => {$(
        impl Default for $ty {
            fn default() -> Self { Table::new().try_into().expect("serde defaults deserialize") }
        }
    )*};
}
serde_default!(SurfaceSettings, HarnessSettings, NetworkSettings, Discovery, UpdaterSettings, ProjectSettings);

fn yes() -> bool { true }
fn omp() -> String { "omp".to_owned() }
fn port() -> u16 { DEFAULT_PORT }
fn six_hours() -> u64 { 6 * 60 * 60 }
fn smol() -> String { "smol".to_owned() }
fn budget() -> u32 { 8_000 }

/// The whole central config file. `version` is checked by the loader and written by the encoder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(rename = "theme")]
    pub appearance: Appearance,
    /// Session ids pinned in the sidebar.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub pinned: BTreeSet<String>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub pinned_projects: BTreeSet<String>,
    /// Restored at startup while its folder exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_project: Option<ActiveProject>,
    /// Whether open projects show as tabs across the top.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub project_tabs: bool,
    pub harness: HarnessSettings,
    pub network: NetworkSettings,
    pub updater: UpdaterSettings,
    pub unattended: UnattendedSettings,
    pub surfaces: SurfaceSettings,
    pub mcp_servers: BTreeMap<String, McpServer>,
    /// SSH hosts the `remote` MCP server may reach, by name.
    pub remote_hosts: BTreeMap<String, RemoteHost>,
    /// By project id.
    pub projects: BTreeMap<String, ProjectSettings>,
    #[serde(skip)]
    pub(crate) unknown: Vec<UnknownKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveProject {
    pub name: String,
    pub cwd: PathBuf,
}

/// A theme by registry name or file path; the legacy `system`, `dark` and `light` are registry names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThemeRef(pub String);

impl ThemeRef {
    pub const SYSTEM: &str = "system";

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ThemeRef {
    fn default() -> Self {
        Self(Self::SYSTEM.to_owned())
    }
}

/// The UI theme plus token overrides; config names a theme, never its colors. Written as
/// `theme = "<name>"`, or as a `[theme]` table with `name` and the overrides.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "AppearanceRepr", into = "AppearanceRepr")]
pub struct Appearance {
    pub theme: ThemeRef,
    pub overrides: Table,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum AppearanceRepr {
    Name(ThemeRef),
    Table {
        #[serde(default)]
        name: ThemeRef,
        #[serde(flatten)]
        overrides: Table,
    },
}

impl From<AppearanceRepr> for Appearance {
    fn from(repr: AppearanceRepr) -> Self {
        match repr {
            AppearanceRepr::Name(theme) => Self { theme, overrides: Table::new() },
            AppearanceRepr::Table { name, overrides } => Self { theme: name, overrides },
        }
    }
}

impl From<Appearance> for AppearanceRepr {
    fn from(a: Appearance) -> Self {
        if a.overrides.is_empty() { Self::Name(a.theme) } else { Self::Table { name: a.theme, overrides: a.overrides } }
    }
}

/// How live surfaces stream: H.264/HEVC video, and where browser frames come from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceSettings {
    #[serde(default = "yes")] pub video: bool,
    #[serde(default)] pub browser_capture: BrowserCapture,
}

/// Headless Chrome's own screencast, or headed Chrome on an Xvfb display captured raw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowserCapture {
    #[default]
    Screencast,
    VirtualDisplay,
}

/// The harness new sessions use, executable overrides by harness name, and omp's approval mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSettings {
    #[serde(default = "omp")] pub default: String,
    #[serde(default)] pub executables: BTreeMap<String, PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub approval_mode: Option<String>,
}

impl HarnessSettings {
    pub fn executable(&self, harness: &str) -> Option<&Path> {
        self.executables.get(harness).map(PathBuf::as_path)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSettings {
    #[serde(default = "port")] pub port: u16,
    #[serde(default)] pub relay: Relay,
    #[serde(default)] pub discovery: Discovery,
}

/// Relay used when no direct path to a peer exists: `"default"` (iroh's), `"off"` or an `http(s)` URL.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Relay {
    #[default]
    Default,
    Off,
    #[serde(untagged)]
    Url(String),
}

/// How peers are found without typing their key: mDNS and Tailscale MagicDNS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    #[serde(default = "yes")] pub mdns: bool,
    #[serde(default = "yes")] pub tailscale: bool,
}

/// Update checks; `pins` holds a version per component, `trusted_keys` hex ed25519 release keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdaterSettings {
    #[serde(default = "six_hours")] pub check_interval_secs: u64,
    #[serde(default)] pub pins: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub github_token: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub trusted_keys: Vec<String>,
}

impl UpdaterSettings {
    pub fn check_interval(&self) -> Duration {
        Duration::from_secs(self.check_interval_secs)
    }
}

/// Default answers for decisions nobody answers in time; a kind without one waits forever.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UnattendedSettings {
    #[serde(skip_serializing_if = "Option::is_none")] pub approval: Option<DecisionDefault>,
    #[serde(skip_serializing_if = "Option::is_none")] pub question: Option<DecisionDefault>,
    #[serde(skip_serializing_if = "Option::is_none")] pub plan: Option<DecisionDefault>,
    #[serde(skip_serializing_if = "Option::is_none")] pub form: Option<DecisionDefault>,
}

impl UnattendedSettings {
    pub fn for_kind(&self, kind: DecisionKind) -> Option<&DecisionDefault> {
        match kind {
            DecisionKind::Approval => self.approval.as_ref(),
            DecisionKind::Question => self.question.as_ref(),
            DecisionKind::Plan => self.plan.as_ref(),
            DecisionKind::Form => self.form.as_ref(),
        }
    }
}

/// What to answer when a decision has waited `timeout_secs`; `reason` is recorded with it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionDefault {
    pub timeout_secs: u64,
    pub answer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub reason: Option<String>,
}

/// A registered MCP server; a disabled one stays registered but isn't started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "McpServerRepr", into = "McpServerRepr")]
pub struct McpServer {
    pub transport: McpTransport,
    pub enabled: bool,
}

impl McpServer {
    pub fn spec(&self, name: &str) -> McpServerSpec {
        McpServerSpec { name: name.to_owned(), transport: self.transport.clone() }
    }
}

/// File shape of [`McpServer`]: `command` (stdio) or `url` (HTTP), never both.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct McpServerRepr {
    #[serde(default = "yes")] enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")] command: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")] args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")] env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")] url: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")] headers: BTreeMap<String, String>,
}

impl TryFrom<McpServerRepr> for McpServer {
    type Error = &'static str;

    fn try_from(r: McpServerRepr) -> Result<Self, Self::Error> {
        let transport = match (r.command, r.url) {
            (Some(command), None) if r.headers.is_empty() => McpTransport::Stdio { command, args: r.args, env: r.env },
            (None, Some(url)) if r.args.is_empty() && r.env.is_empty() => McpTransport::Http { url, headers: r.headers },
            (Some(_), Some(_)) => return Err("set `command` or `url`, not both"),
            (None, None) => return Err("set `command` (stdio) or `url` (HTTP)"),
            _ => return Err("`args` and `env` only apply to `command` servers, `headers` only to `url` ones"),
        };
        Ok(Self { transport, enabled: r.enabled })
    }
}

impl From<McpServer> for McpServerRepr {
    fn from(s: McpServer) -> Self {
        let enabled = s.enabled;
        match s.transport {
            McpTransport::Stdio { command, args, env } => Self { enabled, command: Some(command), args, env, ..Self::default() },
            McpTransport::Http { url, headers } => Self { enabled, url: Some(url), headers, ..Self::default() },
        }
    }
}

/// An SSH host (name, address or `~/.ssh/config` alias) and what the agent may do there:
/// touch the absolute `allowed_paths` and below, run commands, skip approval cards.
/// loa-remote enforces the limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHost {
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub proxy_jump: Option<String>,
    #[serde(default)] pub allowed_paths: Vec<String>,
    #[serde(default = "yes")] pub read_only: bool,
    #[serde(default)] pub commands: bool,
    #[serde(default)] pub auto_approve: bool,
}

/// Settings shared by every session of one project: instructions, always-included files,
/// the omp role and token budget for session digests, and switched-off tools.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSettings {
    #[serde(default)] pub instructions: String,
    #[serde(default)] pub pinned_files: Vec<PathBuf>,
    #[serde(default = "smol")] pub digest_role: String,
    #[serde(default = "budget")] pub token_budget: u32,
    #[serde(default)] pub disabled_tools: BTreeSet<String>,
    /// By MCP server name.
    #[serde(default)] pub disabled_mcp_tools: BTreeMap<String, BTreeSet<String>>,
}

impl ProjectSettings {
    pub fn mcp_tool_enabled(&self, server: &str, tool: &str) -> bool {
        self.disabled_mcp_tools.get(server).is_none_or(|tools| !tools.contains(tool))
    }
}

impl Config {
    /// Settings of project `id`, or the defaults when it has none.
    pub fn project(&self, id: &str) -> ProjectSettings {
        self.projects.get(id).cloned().unwrap_or_default()
    }

    /// Adds enabled MCP servers, the project's disabled tools and the harness
    /// executable override to `session`, keeping whatever the caller set.
    pub fn apply_to_session(&self, harness: &str, session: &mut SessionConfig) {
        session.mcp_servers.extend(self.mcp_servers.iter().filter(|(_, s)| s.enabled).map(|(name, s)| s.spec(name)));
        if let Some(project) = session.project.as_deref().and_then(|p| self.projects.get(p)) {
            session.disabled_tools.extend(project.disabled_tools.iter().cloned());
        }
        if session.executable.is_none() {
            session.executable = self.harness.executable(harness).map(Path::to_path_buf);
        }
    }
}
