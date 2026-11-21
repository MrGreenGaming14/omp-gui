//! Tools panel: built in tools, MCP servers, remote hosts and what each
//! peer may use, with per project tool toggles.
//!
//! The app feeds plain data in through the `set_*` methods and acts on the
//! [`Event`]s that [`State::update`] returns. The panel never talks to a
//! server, a host or the config itself.

pub mod catalog;
pub mod form;
pub mod model;
mod view;

use std::collections::{BTreeSet, HashMap};

use iced::Task;
use loa_contracts::harness::McpServerSpec;

pub use catalog::{Catalog, Filter, PeerMatrix, Toggle};
pub use form::{
    AddHostErrors, AddHostForm, AddServerErrors, AddServerForm, FieldError, LimitsDraft,
    TransportKind,
};
pub use model::{
    AuditAction, AuditRow, BuiltinGroup, BuiltinTools, DetailState, HostLimits, ImportCandidate,
    ImportOutcome, ImportSource, McpServer, PeerTools, ProjectRef, PromptAnswer, PromptKind,
    RemoteHost, RemotePrompt, ServerKind, ServerStatus, ToolDetail, ToolId, ToolInfo, ToolRule,
};

/// The panel's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Builtin,
    Servers,
    Hosts,
    Peers,
}

/// Run control actions on one MCP server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerAction {
    Start,
    Stop,
    Restart,
    Remove,
}

/// What the app must act on.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The project selector changed. The app answers with [`State::set_disabled_tools`].
    ProjectSelected(String),
    /// Turn these tools on or off in this project.
    SetToolsEnabled {
        project: String,
        tools: Vec<ToolId>,
        enabled: bool,
    },
    Server {
        name: String,
        action: ServerAction,
    },
    AddServer(McpServerSpec),
    /// Look for server configs in `.claude`, `.cursor` and `.codex`. The app
    /// answers with [`State::set_import_candidates`].
    ScanImports,
    /// Import these candidates. The app answers with [`State::set_import_outcome`] per entry.
    ImportServers(Vec<ImportCandidate>),
    /// Read hosts from `~/.ssh/config`. The app answers with [`State::set_hosts`].
    ImportSshHosts,
    AddHost(RemoteHost),
    RemoveHost(String),
    SaveHostLimits {
        host: String,
        limits: HostLimits,
    },
    /// Open a terminal pane to this host.
    OpenTerminal(String),
    /// Load this host's audit log. The app answers with [`State::set_audit`].
    LoadAudit(String),
    /// The user answered a host key or approval question.
    AnswerPrompt { id: u64, answer: PromptAnswer },
}

/// One entry of the import list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportEntry {
    pub candidate: ImportCandidate,
    pub selected: bool,
    /// A server with the same name is already registered.
    pub exists: bool,
    pub outcome: Option<ImportOutcome>,
}

/// Peer filter of the peers view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerChoice {
    All,
    Peer { key: String, name: String },
}

impl std::fmt::Display for PeerChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::All => f.write_str("All peers"),
            Self::Peer { name, .. } => f.write_str(name),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    SectionSelected(Section),
    FilterChanged(String),
    ProjectSelected(ProjectRef),
    ToolToggled(ToolId, bool),
    ServerToggled(String),
    Server(String, ServerAction),

    AddServerOpen,
    AddServerCancel,
    AddServerName(String),
    AddServerKind(TransportKind),
    AddServerCommand(String),
    AddServerUrl(String),
    AddServerSubmit,

    ImportScan,
    ImportSelect(usize, bool),
    ImportSubmit,
    ImportClose,

    ImportSshHosts,
    AddHostOpen,
    AddHostCancel,
    AddHostAlias(String),
    AddHostHostname(String),
    AddHostUser(String),
    AddHostPort(String),
    AddHostSubmit,
    RemoveHost(String),
    EditLimits(String),
    LimitsReadOnly(bool),
    LimitsCommands(bool),
    LimitsNewPath(String),
    LimitsAddPath,
    LimitsRemovePath(usize),
    LimitsSave,
    LimitsCancel,
    OpenTerminal(String),
    ShowAudit(String),
    CloseAudit,
    LimitsAutoApprove(bool),
    AnswerPrompt(u64, PromptAnswer),

    PeerFilter(PeerChoice),

    /// Open or close a collapsible block: a tool group, a server's tool
    /// list or the hosts list, by its key.
    BlockOpen(String, bool),
    /// Show or hide one tool's details inline.
    ToolExpanded(ToolId),
}

/// Tools panel state.
#[derive(Debug, Default)]
pub struct State {
    section: Section,
    filter_text: String,
    filter: Filter,
    builtin: BuiltinTools,
    servers: Vec<McpServer>,
    projects: Vec<ProjectRef>,
    project: Option<String>,
    disabled: HashMap<String, BTreeSet<ToolId>>,
    add_server: Option<(AddServerForm, AddServerErrors)>,
    imports: Option<Vec<ImportEntry>>,
    /// A scan was asked for and its candidates haven't arrived yet.
    import_scanning: bool,
    hosts: Vec<RemoteHost>,
    add_host: Option<(AddHostForm, AddHostErrors)>,
    limits: Option<LimitsDraft>,
    audit: Option<(String, Vec<AuditRow>)>,
    /// Host key and approval questions waiting for the user.
    prompts: Vec<RemotePrompt>,
    peers: Vec<PeerTools>,
    peer_filter: Option<String>,
    /// Blocks the user opened or closed, by key. Blocks not listed use
    /// their default: closed when long.
    open_blocks: HashMap<String, bool>,
    /// The tool whose details are shown inline.
    expanded_tool: Option<ToolId>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// Harness tools of the active adapter and the app tools.
    pub fn set_builtin(&mut self, builtin: BuiltinTools) {
        self.builtin = builtin;
    }

    pub fn set_servers(&mut self, servers: Vec<McpServer>) {
        self.servers = servers;
        if let Some(entries) = &mut self.imports {
            for entry in entries {
                entry.exists = self
                    .servers
                    .iter()
                    .any(|s| s.name == entry.candidate.server.name);
            }
        }
    }

    /// Projects for the selector. Keeps the selection when it still exists.
    pub fn set_projects(&mut self, projects: Vec<ProjectRef>) {
        if let Some(id) = &self.project
            && !projects.iter().any(|p| &p.id == id)
        {
            self.project = None;
        }
        self.projects = projects;
    }

    /// Selects a project without emitting [`Event::ProjectSelected`].
    pub fn select_project(&mut self, id: impl Into<String>) {
        self.project = Some(id.into());
    }

    /// The tools switched off in a project, as stored in the config.
    pub fn set_disabled_tools(
        &mut self,
        project: impl Into<String>,
        tools: impl IntoIterator<Item = ToolId>,
    ) {
        self.disabled
            .insert(project.into(), tools.into_iter().collect());
    }

    pub fn set_import_candidates(&mut self, candidates: Vec<ImportCandidate>) {
        let entries = candidates
            .into_iter()
            .map(|candidate| {
                let exists = self.servers.iter().any(|s| s.name == candidate.server.name);
                ImportEntry {
                    candidate,
                    selected: !exists,
                    exists,
                    outcome: None,
                }
            })
            .collect();
        self.imports = Some(entries);
        self.import_scanning = false;
    }

    /// Records the result of importing one candidate.
    pub fn set_import_outcome(&mut self, source: ImportSource, name: &str, outcome: ImportOutcome) {
        let Some(entries) = &mut self.imports else {
            return;
        };
        if let Some(entry) = entries
            .iter_mut()
            .find(|e| e.candidate.source == source && e.candidate.server.name == name)
        {
            if outcome == ImportOutcome::Imported {
                entry.selected = false;
            }
            entry.outcome = Some(outcome);
        }
    }

    pub fn set_hosts(&mut self, hosts: Vec<RemoteHost>) {
        self.hosts = hosts;
    }

    pub fn set_audit(&mut self, host: impl Into<String>, rows: Vec<AuditRow>) {
        self.audit = Some((host.into(), rows));
    }

    /// The questions the `remote` server is waiting on, oldest first.
    pub fn set_prompts(&mut self, prompts: Vec<RemotePrompt>) {
        self.prompts = prompts;
    }

    pub fn prompts(&self) -> &[RemotePrompt] {
        &self.prompts
    }

    pub fn hosts(&self) -> &[RemoteHost] {
        &self.hosts
    }

    /// The host whose audit log is shown, and its rows.
    pub fn audit(&self) -> Option<(&str, &[AuditRow])> {
        self.audit.as_ref().map(|(host, rows)| (host.as_str(), rows.as_slice()))
    }

    pub fn set_peers(&mut self, peers: Vec<PeerTools>) {
        if let Some(key) = &self.peer_filter
            && !peers.iter().any(|p| &p.key == key)
        {
            self.peer_filter = None;
        }
        self.peers = peers;
    }

    /// The tools switched off in the selected project.
    pub fn disabled_tools(&self) -> Option<&BTreeSet<ToolId>> {
        self.project.as_ref().and_then(|id| self.disabled.get(id))
    }

    /// Built in and MCP sections as currently shown.
    pub fn catalog(&self) -> Catalog<'_> {
        let empty = BTreeSet::new();
        catalog::catalog(
            &self.builtin,
            &self.servers,
            self.disabled_tools().unwrap_or(&empty),
            &self.filter,
        )
    }

    /// The peers view as currently shown.
    pub fn peer_matrix(&self) -> PeerMatrix<'_> {
        catalog::peer_matrix(
            &self.builtin,
            &self.servers,
            &self.peers,
            self.peer_filter.as_deref(),
            &self.filter,
        )
    }

    pub fn imports(&self) -> Option<&[ImportEntry]> {
        self.imports.as_deref()
    }

    pub fn add_server_errors(&self) -> Option<&AddServerErrors> {
        self.add_server.as_ref().map(|(_, errors)| errors)
    }

    pub fn add_host_errors(&self) -> Option<&AddHostErrors> {
        self.add_host.as_ref().map(|(_, errors)| errors)
    }

    pub fn limits_draft(&self) -> Option<&LimitsDraft> {
        self.limits.as_ref()
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        (Task::none(), self.handle(message))
    }

    fn handle(&mut self, message: Message) -> Option<Event> {
        match message {
            Message::SectionSelected(section) => self.section = section,
            Message::FilterChanged(text) => {
                self.filter = Filter::new(&text);
                self.filter_text = text;
            }
            Message::ProjectSelected(project) => {
                if self.project.as_ref() == Some(&project.id) {
                    return None;
                }
                self.project = Some(project.id.clone());
                return Some(Event::ProjectSelected(project.id));
            }
            Message::ToolToggled(tool, enabled) => return self.set_enabled(vec![tool], enabled),
            Message::ServerToggled(name) => {
                let server = self.servers.iter().find(|s| s.name == name)?;
                let empty = BTreeSet::new();
                let toggle =
                    catalog::server_toggle(server, self.disabled_tools().unwrap_or(&empty));
                let tools = server
                    .tools
                    .iter()
                    .map(|t| ToolId::mcp(&server.name, &t.name))
                    .collect();
                return self.set_enabled(tools, toggle.next_enabled());
            }
            Message::Server(name, action) => {
                let server = self.servers.iter().find(|s| s.name == name)?;
                if action == ServerAction::Remove && !server.can_remove() {
                    return None;
                }
                return Some(Event::Server { name, action });
            }

            Message::AddServerOpen => {
                self.add_server.get_or_insert_with(Default::default);
            }
            Message::AddServerCancel => self.add_server = None,
            Message::AddServerName(name) => {
                let (form, errors) = self.add_server.as_mut()?;
                form.name = name;
                errors.name = None;
            }
            Message::AddServerKind(kind) => {
                let (form, errors) = self.add_server.as_mut()?;
                form.kind = kind;
                errors.target = None;
            }
            Message::AddServerCommand(command) => {
                let (form, errors) = self.add_server.as_mut()?;
                form.command = command;
                errors.target = None;
            }
            Message::AddServerUrl(url) => {
                let (form, errors) = self.add_server.as_mut()?;
                form.url = url;
                errors.target = None;
            }
            Message::AddServerSubmit => {
                let (form, errors) = self.add_server.as_mut()?;
                match form.validate(self.servers.iter().map(|s| s.name.as_str())) {
                    Ok(spec) => {
                        self.add_server = None;
                        return Some(Event::AddServer(spec));
                    }
                    Err(found) => *errors = found,
                }
            }

            Message::ImportScan => {
                self.imports = Some(Vec::new());
                self.import_scanning = true;
                return Some(Event::ScanImports);
            }
            Message::ImportSelect(index, selected) => {
                if let Some(entry) = self.imports.as_mut()?.get_mut(index) {
                    entry.selected = selected;
                }
            }
            Message::ImportSubmit => {
                let chosen: Vec<_> = self
                    .imports
                    .as_ref()?
                    .iter()
                    .filter(|e| e.selected)
                    .map(|e| e.candidate.clone())
                    .collect();
                if chosen.is_empty() {
                    return None;
                }
                return Some(Event::ImportServers(chosen));
            }
            Message::ImportClose => {
                self.imports = None;
                self.import_scanning = false;
            }

            Message::ImportSshHosts => return Some(Event::ImportSshHosts),
            Message::AddHostOpen => {
                self.add_host.get_or_insert_with(Default::default);
            }
            Message::AddHostCancel => self.add_host = None,
            Message::AddHostAlias(alias) => {
                let (form, errors) = self.add_host.as_mut()?;
                form.alias = alias;
                errors.alias = None;
            }
            Message::AddHostHostname(hostname) => {
                let (form, errors) = self.add_host.as_mut()?;
                form.hostname = hostname;
                errors.hostname = None;
            }
            Message::AddHostUser(user) => {
                let (form, errors) = self.add_host.as_mut()?;
                form.user = user;
                errors.user = None;
            }
            Message::AddHostPort(port) => {
                let (form, errors) = self.add_host.as_mut()?;
                form.port = port;
                errors.port = None;
            }
            Message::AddHostSubmit => {
                let (form, errors) = self.add_host.as_mut()?;
                match form.validate(self.hosts.iter().map(|h| h.alias.as_str())) {
                    Ok(host) => {
                        self.add_host = None;
                        return Some(Event::AddHost(host));
                    }
                    Err(found) => *errors = found,
                }
            }
            Message::RemoveHost(alias) => {
                if self.limits.as_ref().is_some_and(|d| d.host == alias) {
                    self.limits = None;
                }
                return Some(Event::RemoveHost(alias));
            }
            Message::EditLimits(alias) => {
                let host = self.hosts.iter().find(|h| h.alias == alias)?;
                self.limits = Some(LimitsDraft::new(host));
            }
            Message::LimitsReadOnly(read_only) => {
                self.limits.as_mut()?.limits.read_only = read_only
            }
            Message::LimitsCommands(allowed) => {
                self.limits.as_mut()?.limits.commands_allowed = allowed
            }
            Message::LimitsNewPath(path) => {
                let draft = self.limits.as_mut()?;
                draft.new_path = path;
                draft.path_error = None;
            }
            Message::LimitsAddPath => self.limits.as_mut()?.add_path(),
            Message::LimitsRemovePath(index) => {
                let paths = &mut self.limits.as_mut()?.limits.allowed_paths;
                if index < paths.len() {
                    paths.remove(index);
                }
            }
            Message::LimitsSave => {
                let draft = self.limits.take()?;
                return Some(Event::SaveHostLimits {
                    host: draft.host,
                    limits: draft.limits,
                });
            }
            Message::LimitsCancel => self.limits = None,
            Message::OpenTerminal(alias) => return Some(Event::OpenTerminal(alias)),
            Message::ShowAudit(alias) => {
                self.audit = Some((alias.clone(), Vec::new()));
                return Some(Event::LoadAudit(alias));
            }
            Message::CloseAudit => self.audit = None,
            Message::LimitsAutoApprove(on) => self.limits.as_mut()?.limits.auto_approve = on,
            Message::AnswerPrompt(id, answer) => {
                let index = self.prompts.iter().position(|p| p.id == id)?;
                self.prompts.remove(index);
                return Some(Event::AnswerPrompt { id, answer });
            }

            Message::BlockOpen(key, open) => {
                self.open_blocks.insert(key, open);
            }
            Message::ToolExpanded(tool) => {
                self.expanded_tool = if self.expanded_tool.as_ref() == Some(&tool) {
                    None
                } else {
                    Some(tool)
                };
            }

            Message::PeerFilter(choice) => {
                self.peer_filter = match choice {
                    PeerChoice::All => None,
                    PeerChoice::Peer { key, .. } => Some(key),
                };
            }
        }
        None
    }

    /// Optimistically applies a toggle and asks the app to persist it.
    fn set_enabled(&mut self, tools: Vec<ToolId>, enabled: bool) -> Option<Event> {
        let project = self.project.clone()?;
        if tools.is_empty() {
            return None;
        }
        let disabled = self.disabled.entry(project.clone()).or_default();
        for tool in &tools {
            if enabled {
                disabled.remove(tool);
            } else {
                disabled.insert(tool.clone());
            }
        }
        Some(Event::SetToolsEnabled {
            project,
            tools,
            enabled,
        })
    }
}

#[cfg(test)]
mod tests;
