//! The typed config schema and its validation.

use crate::codec::UnknownKey;
use crate::{Result, invalid};
use loa_adapter::{McpServerSpec, McpTransport};
use loa_core::{DecisionKind, answer};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Schema version this build reads and writes.
pub const CURRENT_VERSION: u32 = 1;

/// The p2p port from the networking design (UDP for iroh, TCP fallback).
pub const DEFAULT_PORT: u16 = 31415;

/// The whole central config file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Schema version of the file. Set by the loader; see [`CURRENT_VERSION`].
    pub version: u32,
    pub theme: Theme,
    /// Ids of the sessions pinned in the sidebar.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub pinned: BTreeSet<String>,
    /// Names of the projects pinned to the top of the sidebar project list.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub pinned_projects: BTreeSet<String>,
    /// The project whose folder is the active folder, restored at startup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_project: Option<ActiveProject>,
    pub harness: HarnessSettings,
    pub network: NetworkSettings,
    pub updater: UpdaterSettings,
    pub unattended: UnattendedSettings,
    pub surfaces: SurfaceSettings,
    /// MCP servers by name.
    pub mcp_servers: BTreeMap<String, McpServer>,
    /// SSH hosts the `remote` MCP server may reach, by name.
    pub remote_hosts: BTreeMap<String, RemoteHost>,
    /// Per project settings, by project id.
    pub projects: BTreeMap<String, ProjectSettings>,
    /// Keys from the file this build doesn't know, written back on save.
    #[serde(skip)]
    pub(crate) unknown: Vec<UnknownKey>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            theme: Theme::default(),
            pinned: BTreeSet::new(),
            pinned_projects: BTreeSet::new(),
            active_project: None,
            harness: HarnessSettings::default(),
            network: NetworkSettings::default(),
            updater: UpdaterSettings::default(),
            unattended: UnattendedSettings::default(),
            surfaces: SurfaceSettings::default(),
            mcp_servers: BTreeMap::new(),
            remote_hosts: BTreeMap::new(),
            projects: BTreeMap::new(),
            unknown: Vec::new(),
        }
    }
}

/// The project the user last selected and the folder it runs in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveProject {
    pub name: String,
    pub cwd: PathBuf,
}

/// UI color scheme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the OS light or dark setting.
    #[default]
    System,
    Dark,
    Light,
}

/// The Surfaces panel: how live surfaces stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SurfaceSettings {
    /// Live surfaces (browser, VMs) stream H.264 or HEVC video, hardware
    /// encoded when the machine can.
    pub video: bool,
    /// How browser surfaces are captured.
    pub browser_capture: BrowserCapture,
}

impl Default for SurfaceSettings {
    fn default() -> Self {
        Self { video: true, browser_capture: BrowserCapture::Screencast }
    }
}

/// Where browser surface frames come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowserCapture {
    /// Headless Chrome's own screencast.
    #[default]
    Screencast,
    /// Headed Chrome on an Xvfb virtual display, captured raw.
    VirtualDisplay,
}

/// Which harness new sessions use and where each harness's executable lives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HarnessSettings {
    /// Harness name as registered in `loa_adapter::Registry`.
    pub default: String,
    /// Executable per harness name, used instead of the one on `PATH`.
    pub executables: BTreeMap<String, PathBuf>,
}

impl Default for HarnessSettings {
    fn default() -> Self {
        Self { default: "omp".to_owned(), executables: BTreeMap::new() }
    }
}

impl HarnessSettings {
    /// The executable override for `harness`, if one is set.
    pub fn executable(&self, harness: &str) -> Option<&Path> {
        self.executables.get(harness).map(PathBuf::as_path)
    }
}

/// The p2p endpoint settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkSettings {
    /// UDP (iroh) and TCP fallback port.
    pub port: u16,
    pub relay: Relay,
    pub discovery: Discovery,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self { port: DEFAULT_PORT, relay: Relay::Default, discovery: Discovery::default() }
    }
}

/// Relay used when no direct path to a peer exists.
///
/// Written as `"default"`, `"off"` or an `http(s)` URL.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Relay {
    /// iroh's public relays.
    #[default]
    Default,
    /// Direct connections only.
    Off,
    /// A self hosted relay.
    Url(String),
}

impl TryFrom<String> for Relay {
    type Error = String;

    fn try_from(value: String) -> Result<Self, String> {
        match value.as_str() {
            "default" => Ok(Self::Default),
            "off" => Ok(Self::Off),
            url if is_http_url(url) => Ok(Self::Url(value)),
            _ => Err(format!("expected \"default\", \"off\" or an http(s) URL, found {value:?}")),
        }
    }
}

impl From<Relay> for String {
    fn from(relay: Relay) -> Self {
        match relay {
            Relay::Default => "default".to_owned(),
            Relay::Off => "off".to_owned(),
            Relay::Url(url) => url,
        }
    }
}

/// How peers are found without typing their key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Discovery {
    /// mDNS on the local network.
    pub mdns: bool,
    /// Tailscale MagicDNS names.
    pub tailscale: bool,
}

impl Default for Discovery {
    fn default() -> Self {
        Self { mdns: true, tailscale: true }
    }
}

/// Update checks for the app, omp and the harnesses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdaterSettings {
    /// Seconds between release feed checks.
    pub check_interval_secs: u64,
    /// Version pinned per component (`app`, `omp`, a harness name). A pinned
    /// component is never updated.
    pub pins: BTreeMap<String, String>,
    /// GitHub token for release feed checks; raises the rate limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_token: Option<String>,
    /// Hex ed25519 public keys app releases must be signed with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_keys: Vec<String>,
}

impl Default for UpdaterSettings {
    fn default() -> Self {
        Self { check_interval_secs: 6 * 60 * 60, pins: BTreeMap::new(), github_token: None, trusted_keys: Vec::new() }
    }
}

impl UpdaterSettings {
    pub fn check_interval(&self) -> Duration {
        Duration::from_secs(self.check_interval_secs)
    }

    /// The pinned version of `component`, if any.
    pub fn pin(&self, component: &str) -> Option<&str> {
        self.pins.get(component).map(String::as_str)
    }
}

/// Default answers for decisions nobody answers in time, one per kind.
/// A kind without an entry waits forever.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UnattendedSettings {
    /// Answer is one of `allow_once`, `allow_session`, `deny`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<DecisionDefault>,
    /// Answer is an option label or free text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<DecisionDefault>,
    /// Answer is `approve` or `reject`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<DecisionDefault>,
    /// Answer is a JSON object with the field values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form: Option<DecisionDefault>,
}

impl UnattendedSettings {
    /// The timeout and default answer for decisions of `kind`.
    pub fn for_kind(&self, kind: DecisionKind) -> Option<&DecisionDefault> {
        match kind {
            DecisionKind::Approval => self.approval.as_ref(),
            DecisionKind::Question => self.question.as_ref(),
            DecisionKind::Plan => self.plan.as_ref(),
            DecisionKind::Form => self.form.as_ref(),
        }
    }
}

/// What to answer when a decision has waited `timeout_secs`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionDefault {
    pub timeout_secs: u64,
    pub answer: String,
    /// Recorded as the decision's `reason`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl DecisionDefault {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

/// A registered MCP server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "McpServerRepr", into = "McpServerRepr")]
pub struct McpServer {
    pub transport: McpTransport,
    /// A disabled server stays registered but isn't started or given to harnesses.
    pub enabled: bool,
}

impl McpServer {
    /// The server as the adapter contract describes it.
    pub fn spec(&self, name: &str) -> McpServerSpec {
        McpServerSpec { name: name.to_owned(), transport: self.transport.clone() }
    }
}

/// File shape of [`McpServer`]: `command` (stdio) or `url` (HTTP), never both.
#[derive(Serialize, Deserialize)]
struct McpServerRepr {
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<String, String>,
}

fn yes() -> bool {
    true
}

impl TryFrom<McpServerRepr> for McpServer {
    type Error = String;

    fn try_from(repr: McpServerRepr) -> Result<Self, String> {
        let transport = match (repr.command, repr.url) {
            (Some(command), None) => {
                if !repr.headers.is_empty() {
                    return Err("`headers` only applies to servers with a `url`".to_owned());
                }
                if command.trim().is_empty() {
                    return Err("`command` must not be empty".to_owned());
                }
                McpTransport::Stdio { command, args: repr.args, env: repr.env }
            }
            (None, Some(url)) => {
                if !repr.args.is_empty() || !repr.env.is_empty() {
                    return Err("`args` and `env` only apply to servers with a `command`".to_owned());
                }
                if !is_http_url(&url) {
                    return Err(format!("`url` must be an http(s) URL, found {url:?}"));
                }
                McpTransport::Http { url, headers: repr.headers }
            }
            (Some(_), Some(_)) => return Err("set `command` or `url`, not both".to_owned()),
            (None, None) => return Err("set `command` (stdio) or `url` (HTTP)".to_owned()),
        };
        Ok(Self { transport, enabled: repr.enabled })
    }
}

impl From<McpServer> for McpServerRepr {
    fn from(server: McpServer) -> Self {
        let mut repr = Self {
            enabled: server.enabled,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
        };
        match server.transport {
            McpTransport::Stdio { command, args, env } => {
                repr.command = Some(command);
                repr.args = args;
                repr.env = env;
            }
            McpTransport::Http { url, headers } => {
                repr.url = Some(url);
                repr.headers = headers;
            }
        }
        repr
    }
}

/// An SSH host and the limits on what the agent may do there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHost {
    /// Host name or address, or a `Host` alias from `~/.ssh/config`.
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// `ProxyJump` value, as in `~/.ssh/config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_jump: Option<String>,
    /// Absolute remote paths the agent may touch, each with everything below
    /// it. Empty means no file access.
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default = "yes")]
    pub read_only: bool,
    /// Whether `remote_bash` may run commands.
    #[serde(default)]
    pub commands: bool,
    /// Writes and commands run without an approval card.
    #[serde(default)]
    pub auto_approve: bool,
}

impl RemoteHost {
    /// Whether `path` lies inside one of the allowed paths. Relative paths and
    /// paths with `..` are refused, since they could escape the allowed tree.
    pub fn allows_read(&self, path: &str) -> bool {
        let Some(path) = remote_components(path) else { return false };
        self.allowed_paths.iter().filter_map(|allowed| remote_components(allowed)).any(|allowed| {
            allowed.len() <= path.len() && allowed.iter().zip(&path).all(|(a, p)| a == p)
        })
    }

    /// Whether `path` may be written: the host isn't read only and the path is allowed.
    pub fn allows_write(&self, path: &str) -> bool {
        !self.read_only && self.allows_read(path)
    }
}

/// Components of an absolute POSIX path with `.` and empty parts dropped, or
/// `None` when the path is relative or contains `..`.
fn remote_components(path: &str) -> Option<Vec<&str>> {
    let rest = path.strip_prefix('/')?;
    let mut parts = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            part => parts.push(part),
        }
    }
    Some(parts)
}

/// Settings shared by every session of one project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectSettings {
    /// Shared instructions given to every chat in the project.
    pub instructions: String,
    /// Files always included in the project context.
    pub pinned_files: Vec<PathBuf>,
    /// omp model role that writes session digests.
    pub digest_role: String,
    /// Token budget the ranked digests are packed into.
    pub token_budget: u32,
    /// Built in tools switched off in this project.
    pub disabled_tools: BTreeSet<String>,
    /// MCP tools switched off in this project, by server name.
    pub disabled_mcp_tools: BTreeMap<String, BTreeSet<String>>,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            instructions: String::new(),
            pinned_files: Vec::new(),
            digest_role: "smol".to_owned(),
            token_budget: 8_000,
            disabled_tools: BTreeSet::new(),
            disabled_mcp_tools: BTreeMap::new(),
        }
    }
}

impl ProjectSettings {
    /// Whether the built in tool `tool` is on.
    pub fn tool_enabled(&self, tool: &str) -> bool {
        !self.disabled_tools.contains(tool)
    }

    /// Whether `tool` of MCP server `server` is on.
    pub fn mcp_tool_enabled(&self, server: &str, tool: &str) -> bool {
        self.disabled_mcp_tools.get(server).is_none_or(|tools| !tools.contains(tool))
    }
}

impl Config {
    /// Checks the rules serde can't express. The error names the offending key.
    pub fn validate(&self) -> Result<()> {
        if self.harness.default.trim().is_empty() {
            return Err(invalid("harness.default", "must name a harness"));
        }
        for (name, exe) in &self.harness.executables {
            if exe.as_os_str().is_empty() {
                return Err(invalid(format!("harness.executables.{name}"), "must not be empty"));
            }
        }
        if self.network.port == 0 {
            return Err(invalid("network.port", "must be between 1 and 65535"));
        }
        if self.updater.check_interval_secs == 0 {
            return Err(invalid("updater.check_interval_secs", "must be at least 1"));
        }
        for (component, version) in &self.updater.pins {
            if version.trim().is_empty() {
                return Err(invalid(format!("updater.pins.{component}"), "must be a version"));
            }
        }
        self.validate_unattended()?;
        for name in self.mcp_servers.keys() {
            if name.trim().is_empty() {
                return Err(invalid("mcp_servers", "server names must not be empty"));
            }
        }
        for (name, host) in &self.remote_hosts {
            let key = |field: &str| format!("remote_hosts.{name}.{field}");
            if host.host.trim().is_empty() {
                return Err(invalid(key("host"), "must not be empty"));
            }
            if host.port == Some(0) {
                return Err(invalid(key("port"), "must be between 1 and 65535"));
            }
            if let Some(bad) = host.allowed_paths.iter().find(|p| remote_components(p).is_none()) {
                return Err(invalid(
                    key("allowed_paths"),
                    format!("must hold absolute paths without `..`, found {bad:?}"),
                ));
            }
        }
        for (id, project) in &self.projects {
            let key = |field: &str| format!("projects.{id}.{field}");
            if project.digest_role.trim().is_empty() {
                return Err(invalid(key("digest_role"), "must name a model role"));
            }
            if project.token_budget == 0 {
                return Err(invalid(key("token_budget"), "must be at least 1"));
            }
            if project.pinned_files.iter().any(|p| p.as_os_str().is_empty()) {
                return Err(invalid(key("pinned_files"), "must not hold empty paths"));
            }
        }
        Ok(())
    }

    fn validate_unattended(&self) -> Result<()> {
        let u = &self.unattended;
        let kinds = [
            ("approval", &u.approval, AnswerRule::OneOf(&[answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY])),
            ("question", &u.question, AnswerRule::NonEmpty),
            ("plan", &u.plan, AnswerRule::OneOf(&[answer::APPROVE, answer::REJECT])),
            ("form", &u.form, AnswerRule::JsonObject),
        ];
        for (kind, default, rule) in kinds {
            let Some(default) = default else { continue };
            if default.timeout_secs == 0 {
                return Err(invalid(format!("unattended.{kind}.timeout_secs"), "must be at least 1"));
            }
            rule.check(&default.answer)
                .map_err(|reason| invalid(format!("unattended.{kind}.answer"), reason))?;
        }
        Ok(())
    }

    /// Settings of project `id`, or the defaults when it has none.
    pub fn project(&self, id: &str) -> ProjectSettings {
        self.projects.get(id).cloned().unwrap_or_default()
    }

    /// Enabled MCP servers as the adapter contract describes them.
    pub fn mcp_specs(&self) -> Vec<McpServerSpec> {
        self.mcp_servers
            .iter()
            .filter(|(_, server)| server.enabled)
            .map(|(name, server)| server.spec(name))
            .collect()
    }

    /// Adds the parts of a session's config the central config decides:
    /// enabled MCP servers, the project's disabled built in tools and the
    /// harness executable override. Servers, tools and an executable the
    /// caller already set are kept. Disabled MCP tools are filtered by the MCP
    /// client and written to native configs through [`crate::NativeConfigWriter`].
    pub fn apply_to_session(&self, harness: &str, session: &mut loa_adapter::SessionConfig) {
        session.mcp_servers.extend(self.mcp_specs());
        if let Some(project) = session.project.as_deref().and_then(|p| self.projects.get(p)) {
            session.disabled_tools.extend(project.disabled_tools.iter().cloned());
        }
        if session.executable.is_none() {
            session.executable = self.harness.executable(harness).map(Path::to_path_buf);
        }
    }
}

enum AnswerRule {
    OneOf(&'static [&'static str]),
    NonEmpty,
    JsonObject,
}

impl AnswerRule {
    fn check(&self, answer: &str) -> Result<(), String> {
        match self {
            Self::OneOf(allowed) if !allowed.contains(&answer) => {
                Err(format!("must be one of {}, found {answer:?}", allowed.join(", ")))
            }
            Self::NonEmpty if answer.trim().is_empty() => Err("must not be empty".to_owned()),
            Self::JsonObject => serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(answer)
                .map(drop)
                .map_err(|e| format!("must be a JSON object: {e}")),
            _ => Ok(()),
        }
    }
}

fn is_http_url(s: &str) -> bool {
    ["https://", "http://"]
        .iter()
        .any(|scheme| s.strip_prefix(scheme).is_some_and(|rest| !rest.is_empty()))
}
