//! The tools page: built in tools, MCP servers, remote hosts and what each peer may use, with
//! per project tool toggles. The app feeds data in through the `set_*` methods and acts on the
//! [`Event`]s; the page never talks to a server, a host or the config itself.

pub mod catalog;
pub mod form;
pub mod model;
mod view;

pub use catalog::{Catalog, Filter, PeerMatrix, Toggle};
pub use form::{AddHostForm, AddServerForm, FieldError, FieldId, LimitsDraft, TransportKind};
pub use model::*;

use iced::{Element, Task};
use loa_contracts::harness::McpServerSpec;
use loa_ui_kit::Icon;
use loa_ui_kit::widgets::{Form, FormSpec, FormState, PanelPane};
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Builtin,
    Servers,
    Hosts,
    Peers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerAction {
    Start,
    Stop,
    Restart,
    Remove,
}

/// The page's forms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormId {
    Server,
    Host,
    /// The limits of the host being edited.
    Limits,
}

/// A host limit switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    ReadOnly,
    Commands,
    AutoApprove,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The app answers with [`State::set_disabled_tools`].
    ProjectSelected(String),
    SetToolsEnabled { project: String, tools: Vec<ToolId>, enabled: bool },
    Server { name: String, action: ServerAction },
    AddServer(McpServerSpec),
    /// Look for server configs in `.claude`, `.cursor` and `.codex`; the app answers with
    /// [`State::set_import_candidates`].
    ScanImports,
    /// The app answers with [`State::set_import_outcome`] per entry.
    ImportServers(Vec<ImportCandidate>),
    /// Read hosts from `~/.ssh/config`; the app answers with [`State::set_hosts`].
    ImportSshHosts,
    AddHost(RemoteHost),
    RemoveHost(String),
    SaveHostLimits { host: String, limits: HostLimits },
    OpenTerminal(String),
    /// The app answers with [`State::set_audit`].
    LoadAudit(String),
    AnswerPrompt { id: u64, answer: PromptAnswer },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportEntry {
    pub candidate: ImportCandidate,
    pub selected: bool,
    /// A server with this name is registered already.
    pub exists: bool,
    pub outcome: Option<ImportOutcome>,
}

/// The peers view's filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerChoice {
    All,
    Peer { key: String, name: String },
}

impl std::fmt::Display for PeerChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(if let Self::Peer { name, .. } = self { name } else { "All peers" }) }
}

#[derive(Clone, Debug)]
pub enum Message {
    SectionSelected(Section),
    FilterChanged(String),
    ProjectSelected(ProjectRef),
    ToolToggled(ToolId, bool),
    ServerToggled(String),
    Server(String, ServerAction),
    FormOpen(FormId),
    FormEdit(FormId, FieldId, String),
    FormSubmit(FormId),
    FormCancel(FormId),
    TransportPicked(TransportKind),
    ImportScan,
    ImportSelect(usize, bool),
    ImportSubmit,
    ImportClose,
    ImportSshHosts,
    RemoveHost(String),
    /// Open the limits form of this host.
    EditLimits(String),
    LimitSet(Limit, bool),
    LimitsAddPath,
    LimitsRemovePath(usize),
    OpenTerminal(String),
    ShowAudit(String),
    CloseAudit,
    AnswerPrompt(u64, PromptAnswer),
    PeerFilter(PeerChoice),
    /// Open or close a block (a tool group, a server's tools, the hosts list) by key.
    BlockOpen(String, bool),
    /// Show or hide one tool's details.
    ToolExpanded(ToolId),
}

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
    add_server: Option<FormState<AddServerForm>>,
    imports: Option<Vec<ImportEntry>>,
    /// A scan was asked for and its candidates haven't arrived.
    import_scanning: bool,
    hosts: Vec<RemoteHost>,
    add_host: Option<FormState<AddHostForm>>,
    limits: Option<LimitsDraft>,
    audit: Option<(String, Vec<AuditRow>)>,
    /// Host key and approval questions waiting for the user.
    prompts: Vec<RemotePrompt>,
    peers: Vec<PeerTools>,
    peer_filter: Option<String>,
    /// Blocks opened or closed by the user; others use their default.
    open_blocks: HashMap<String, bool>,
    expanded_tool: Option<ToolId>,
}

impl State {
    pub fn new() -> Self { Self::default() }

    /// The active harness's tools and the app tools.
    pub fn set_builtin(&mut self, builtin: BuiltinTools) { self.builtin = builtin }

    pub fn set_servers(&mut self, servers: Vec<McpServer>) {
        self.servers = servers;
        let servers = &self.servers;
        self.imports.iter_mut().flatten().for_each(|e| e.exists = servers.iter().any(|s| s.name == e.candidate.server.name));
    }

    /// Projects for the selector; the selection stays while it exists.
    pub fn set_projects(&mut self, projects: Vec<ProjectRef>) {
        if self.project.as_ref().is_some_and(|id| !projects.iter().any(|p| &p.id == id)) { self.project = None }
        self.projects = projects;
    }

    /// Selects a project without emitting [`Event::ProjectSelected`].
    pub fn select_project(&mut self, id: impl Into<String>) { self.project = Some(id.into()) }

    /// The tools switched off in a project, as stored in the config.
    pub fn set_disabled_tools(&mut self, project: impl Into<String>, tools: impl IntoIterator<Item = ToolId>) { self.disabled.insert(project.into(), tools.into_iter().collect()); }

    pub fn set_import_candidates(&mut self, candidates: Vec<ImportCandidate>) {
        let exists = |c: &ImportCandidate| self.servers.iter().any(|s| s.name == c.server.name);
        self.imports = Some(candidates.into_iter().map(|c| ImportEntry { selected: !exists(&c), exists: exists(&c), candidate: c, outcome: None }).collect());
        self.import_scanning = false;
    }

    /// Records how importing one candidate went; imported ones are deselected.
    pub fn set_import_outcome(&mut self, source: ImportSource, name: &str, outcome: ImportOutcome) {
        if let Some(e) = self.imports.iter_mut().flatten().find(|e| e.candidate.source == source && e.candidate.server.name == name) {
            e.selected &= outcome != ImportOutcome::Imported;
            e.outcome = Some(outcome);
        }
    }

    pub fn set_hosts(&mut self, hosts: Vec<RemoteHost>) { self.hosts = hosts }

    pub fn set_audit(&mut self, host: impl Into<String>, rows: Vec<AuditRow>) { self.audit = Some((host.into(), rows)) }

    /// The questions the `remote` server waits on, oldest first.
    pub fn set_prompts(&mut self, prompts: Vec<RemotePrompt>) { self.prompts = prompts }

    pub fn prompts(&self) -> &[RemotePrompt] { &self.prompts }

    pub fn hosts(&self) -> &[RemoteHost] { &self.hosts }

    pub fn audit(&self) -> Option<(&str, &[AuditRow])> { self.audit.as_ref().map(|(h, r)| (h.as_str(), r.as_slice())) }

    /// The peer filter resets when its peer disappears.
    pub fn set_peers(&mut self, peers: Vec<PeerTools>) {
        if self.peer_filter.as_ref().is_some_and(|k| !peers.iter().any(|p| &p.key == k)) { self.peer_filter = None }
        self.peers = peers;
    }

    /// The tools switched off in the selected project.
    pub fn disabled_tools(&self) -> Option<&BTreeSet<ToolId>> { self.project.as_ref().and_then(|id| self.disabled.get(id)) }

    pub fn catalog(&self) -> Catalog<'_> { catalog::catalog(&self.builtin, &self.servers, self.disabled_tools().unwrap_or(&BTreeSet::new()), &self.filter) }

    pub fn peer_matrix(&self) -> PeerMatrix<'_> { catalog::peer_matrix(&self.builtin, &self.servers, &self.peers, self.peer_filter.as_deref(), &self.filter) }

    pub fn imports(&self) -> Option<&[ImportEntry]> { self.imports.as_deref() }

    pub fn add_server(&self) -> Option<&FormState<AddServerForm>> { self.add_server.as_ref() }

    pub fn add_host(&self) -> Option<&FormState<AddHostForm>> { self.add_host.as_ref() }

    pub fn limits_draft(&self) -> Option<&LimitsDraft> { self.limits.as_ref() }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) { (Task::none(), self.handle(message)) }

    fn handle(&mut self, message: Message) -> Option<Event> {
        match message {
            Message::SectionSelected(s) => self.section = s,
            Message::FilterChanged(text) => (self.filter, self.filter_text) = (Filter::new(&text), text),
            Message::ProjectSelected(p) if self.project.as_ref() != Some(&p.id) => {
                self.project = Some(p.id.clone());
                return Some(Event::ProjectSelected(p.id));
            }
            Message::ProjectSelected(_) => {}
            Message::ToolToggled(tool, on) => return self.set_enabled(vec![tool], on),
            Message::ServerToggled(name) => {
                let s = self.servers.iter().find(|s| s.name == name)?;
                let on = catalog::server_toggle(s, self.disabled_tools().unwrap_or(&BTreeSet::new())).next_enabled();
                return self.set_enabled(s.tools.iter().map(|t| ToolId::mcp(&s.name, &t.name)).collect(), on);
            }
            Message::Server(name, action) => {
                let s = self.servers.iter().find(|s| s.name == name)?;
                return (action != ServerAction::Remove || s.can_remove()).then_some(Event::Server { name, action });
            }
            Message::FormOpen(FormId::Server) => { self.add_server.get_or_insert_default(); }
            Message::FormOpen(FormId::Host) => { self.add_host.get_or_insert_default(); }
            Message::FormOpen(FormId::Limits) => {}
            Message::FormEdit(FormId::Server, key, value) => edit(self.add_server.as_mut()?, key, &value),
            Message::FormEdit(FormId::Host, key, value) => edit(self.add_host.as_mut()?, key, &value),
            Message::FormEdit(FormId::Limits, _, value) => {
                let d = self.limits.as_mut()?;
                (d.new_path, d.path_error) = (value, None);
            }
            Message::TransportPicked(kind) => self.add_server.as_mut()?.spec.kind = kind,
            Message::FormSubmit(FormId::Server) => {
                let names: Vec<_> = self.servers.iter().map(|s| s.name.clone()).collect();
                let spec = self.add_server.as_mut()?.submit(&names)?;
                self.add_server = None;
                return Some(Event::AddServer(spec));
            }
            Message::FormSubmit(FormId::Host) => {
                let aliases: Vec<_> = self.hosts.iter().map(|h| h.alias.clone()).collect();
                let host = self.add_host.as_mut()?.submit(&aliases)?;
                self.add_host = None;
                return Some(Event::AddHost(host));
            }
            Message::FormSubmit(FormId::Limits) => {
                let d = self.limits.take()?;
                return Some(Event::SaveHostLimits { host: d.host, limits: d.limits });
            }
            Message::FormCancel(FormId::Server) => self.add_server = None,
            Message::FormCancel(FormId::Host) => self.add_host = None,
            Message::FormCancel(FormId::Limits) => self.limits = None,
            Message::ImportScan => {
                (self.imports, self.import_scanning) = (Some(Vec::new()), true);
                return Some(Event::ScanImports);
            }
            Message::ImportSelect(i, on) => self.imports.as_mut()?.get_mut(i)?.selected = on,
            Message::ImportSubmit => {
                let chosen: Vec<_> = self.imports.as_ref()?.iter().filter(|e| e.selected).map(|e| e.candidate.clone()).collect();
                return (!chosen.is_empty()).then_some(Event::ImportServers(chosen));
            }
            Message::ImportClose => (self.imports, self.import_scanning) = (None, false),
            Message::ImportSshHosts => return Some(Event::ImportSshHosts),
            Message::RemoveHost(alias) => {
                if self.limits.as_ref().is_some_and(|d| d.host == alias) { self.limits = None }
                return Some(Event::RemoveHost(alias));
            }
            Message::EditLimits(alias) => self.limits = Some(LimitsDraft::new(self.hosts.iter().find(|h| h.alias == alias)?)),
            Message::LimitSet(limit, on) => {
                let l = &mut self.limits.as_mut()?.limits;
                *match limit {
                    Limit::ReadOnly => &mut l.read_only,
                    Limit::Commands => &mut l.commands_allowed,
                    Limit::AutoApprove => &mut l.auto_approve,
                } = on;
            }
            Message::LimitsAddPath => self.limits.as_mut()?.add_path(),
            Message::LimitsRemovePath(i) => {
                let paths = &mut self.limits.as_mut()?.limits.allowed_paths;
                if i < paths.len() { paths.remove(i); }
            }
            Message::OpenTerminal(alias) => return Some(Event::OpenTerminal(alias)),
            Message::ShowAudit(alias) => {
                self.audit = Some((alias.clone(), Vec::new()));
                return Some(Event::LoadAudit(alias));
            }
            Message::CloseAudit => self.audit = None,
            Message::AnswerPrompt(id, answer) => {
                let i = self.prompts.iter().position(|p| p.id == id)?;
                self.prompts.remove(i);
                return Some(Event::AnswerPrompt { id, answer });
            }
            Message::PeerFilter(choice) => self.peer_filter = if let PeerChoice::Peer { key, .. } = choice { Some(key) } else { None },
            Message::BlockOpen(key, open) => { self.open_blocks.insert(key, open); }
            Message::ToolExpanded(tool) => self.expanded_tool = (self.expanded_tool.as_ref() != Some(&tool)).then_some(tool),
        }
        None
    }

    /// Applies a toggle at once and asks the app to persist it; needs a project.
    fn set_enabled(&mut self, tools: Vec<ToolId>, enabled: bool) -> Option<Event> {
        let project = self.project.clone().filter(|_| !tools.is_empty())?;
        let disabled = self.disabled.entry(project.clone()).or_default();
        for t in &tools {
            if enabled { disabled.remove(t); } else { disabled.insert(t.clone()); }
        }
        Some(Event::SetToolsEnabled { project, tools, enabled })
    }
}

/// Sets a field and clears its error.
fn edit<F: Form + FormSpec<Field = FieldId>>(form: &mut FormState<F>, key: FieldId, value: &str) {
    let _ = form.spec.set(key, value);
    form.errors.remove(key);
}

impl PanelPane for State {
    type Message = Message;
    fn id(&self) -> &str { "tools" }
    fn title(&self) -> &str { "Tools" }
    fn icon(&self) -> Icon { Icon::Wrench }
    fn view(&self) -> Element<'_, Message> { State::view(self) }
}

#[cfg(test)]
mod tests;
