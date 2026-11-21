//! The tools page: built in tools, MCP servers, remote hosts and peer grants with per project toggles.
//! It reads and emits the plain types of [`loa_view::panel`].

pub mod catalog;
pub mod form;
mod view;

pub use catalog::{Catalog, Filter, PeerMatrix, Toggle};
pub use form::{AddHostForm, AddServerForm, FieldId, LimitsDraft, TransportKind};

use iced::Task;
use loa_ui_kit::widgets::{Form, FormSpec, FormState};
use loa_view::panel::{
    AuditRow, BuiltinTools, ImportCandidate, ImportOutcome, ImportSource, McpServer, PeerTools, ProjectRef, PromptAnswer, RemoteHost, RemotePrompt, ServerAction,
    ServerKind, ToolId, ToolsIntent,
};
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section { #[default] Builtin, Servers, Hosts, Peers }
/// The page's forms; `Limits` edits the limits of the host being edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormId { Server, Host, Limits }
/// A host limit switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit { ReadOnly, Commands, AutoApprove }
/// An import candidate; `exists` when a server with its name is registered already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportEntry { pub candidate: ImportCandidate, pub selected: bool, pub exists: bool, pub outcome: Option<ImportOutcome> }
/// The peers view's filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerChoice { All, Peer { key: String, name: String } }
impl std::fmt::Display for PeerChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(if let Self::Peer { name, .. } = self { name } else { "All peers" }) }
}

/// `BlockOpen` opens or closes a block (tool group, server tools, hosts) by key; `ToolExpanded` toggles details.
#[derive(Clone, Debug)]
pub enum Message {
    SectionSelected(Section), FilterChanged(String), ProjectSelected(ProjectRef), ToolToggled(ToolId, bool),
    ServerToggled(String), Server(String, ServerAction), TransportPicked(TransportKind),
    FormOpen(FormId), FormEdit(FormId, FieldId, String), FormSubmit(FormId), FormCancel(FormId),
    ImportScan, ImportSelect(usize, bool), ImportSubmit, ImportClose, ImportSshHosts,
    RemoveHost(String), EditLimits(String), LimitSet(Limit, bool), LimitsAddPath, LimitsRemovePath(usize),
    OpenTerminal(String), ShowAudit(String), CloseAudit, AnswerPrompt(u64, PromptAnswer),
    PeerFilter(PeerChoice), BlockOpen(String, bool), ToolExpanded(ToolId),
}

/// `open_blocks` holds blocks the user opened or closed; `prompts` wait for the user, oldest
/// first; `search` and `rows` cache the lowercased tool texts and the shown rows.
#[derive(Debug, Default)]
pub struct State {
    section: Section, filter_text: String, filter: Filter, builtin: BuiltinTools, servers: Vec<McpServer>,
    projects: Vec<ProjectRef>, project: Option<String>, disabled: HashMap<String, BTreeSet<ToolId>>,
    add_server: Option<FormState<AddServerForm>>, imports: Option<Vec<ImportEntry>>, import_scanning: bool,
    hosts: Vec<RemoteHost>, add_host: Option<FormState<AddHostForm>>, limits: Option<LimitsDraft>, audit: Option<(String, Vec<AuditRow>)>,
    prompts: Vec<RemotePrompt>, peers: Vec<PeerTools>, peer_filter: Option<String>, open_blocks: HashMap<String, bool>, expanded_tool: Option<ToolId>,
    search: catalog::Search, rows: catalog::Rows,
}

impl State {
    pub fn new() -> Self { Self::default() }
    pub fn set_builtin(&mut self, builtin: BuiltinTools) {
        self.builtin = builtin;
        self.rebuild_search();
    }
    pub fn set_hosts(&mut self, hosts: Vec<RemoteHost>) { self.hosts = hosts }
    pub fn set_audit(&mut self, host: impl Into<String>, rows: Vec<AuditRow>) { self.audit = Some((host.into(), rows)) }
    pub fn set_prompts(&mut self, prompts: Vec<RemotePrompt>) { self.prompts = prompts }
    /// Selects a project without emitting [`ToolsIntent::ProjectSelected`].
    pub fn select_project(&mut self, id: impl Into<String>) {
        self.project = Some(id.into());
        self.rebuild();
    }
    /// The tools switched off in a project, as stored in the config.
    pub fn set_disabled_tools(&mut self, project: impl Into<String>, tools: impl IntoIterator<Item = ToolId>) {
        self.disabled.insert(project.into(), tools.into_iter().collect());
        self.rebuild();
    }
    pub fn set_servers(&mut self, servers: Vec<McpServer>) {
        self.servers = servers;
        let servers = &self.servers;
        self.imports.iter_mut().flatten().for_each(|e| e.exists = servers.iter().any(|s| s.name == e.candidate.server.name));
        self.rebuild_search();
    }
    /// Projects for the selector; the selection stays while it exists.
    pub fn set_projects(&mut self, projects: Vec<ProjectRef>) {
        self.project = self.project.take().filter(|id| projects.iter().any(|p| &p.id == id));
        self.projects = projects;
        self.rebuild();
    }
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
    /// The peer filter resets when its peer disappears.
    pub fn set_peers(&mut self, peers: Vec<PeerTools>) {
        self.peer_filter = self.peer_filter.take().filter(|k| peers.iter().any(|p| &p.key == k));
        self.peers = peers;
    }
    pub fn prompts(&self) -> &[RemotePrompt] { &self.prompts }
    pub fn hosts(&self) -> &[RemoteHost] { &self.hosts }
    pub fn audit(&self) -> Option<(&str, &[AuditRow])> { self.audit.as_ref().map(|(h, r)| (h.as_str(), r.as_slice())) }
    pub fn imports(&self) -> Option<&[ImportEntry]> { self.imports.as_deref() }
    /// The tools switched off in the selected project.
    pub fn disabled_tools(&self) -> Option<&BTreeSet<ToolId>> { self.project.as_ref().and_then(|id| self.disabled.get(id)) }
    /// Built in and MCP sections as currently shown.
    pub fn catalog(&self) -> Catalog<'_> { self.rows.catalog(&self.builtin, &self.servers) }
    /// Lowercases the tool texts again after the builtin or server lists changed.
    fn rebuild_search(&mut self) {
        self.search = catalog::Search::new(&self.builtin, &self.servers);
        self.rebuild();
    }
    /// Refilters and retoggles the rows after the filter, the project or its disabled tools changed.
    fn rebuild(&mut self) {
        let empty = BTreeSet::new();
        self.rows = catalog::Rows::new(&self.builtin, &self.servers, &self.search, self.disabled_tools().unwrap_or(&empty), &self.filter);
    }
    pub fn peer_matrix(&self) -> PeerMatrix<'_> { catalog::peer_matrix(&self.builtin, &self.servers, &self.peers, self.peer_filter.as_deref(), &self.filter) }
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<ToolsIntent>) { (Task::none(), self.handle(message)) }
    fn handle(&mut self, message: Message) -> Option<ToolsIntent> {
        match message {
            Message::SectionSelected(s) => self.section = s,
            Message::FilterChanged(text) => {
                (self.filter, self.filter_text) = (Filter::new(&text), text);
                self.rebuild();
            }
            Message::ProjectSelected(p) if self.project.as_ref() != Some(&p.id) => {
                self.project = Some(p.id.clone());
                self.rebuild();
                return Some(ToolsIntent::ProjectSelected(p.id));
            }
            Message::ProjectSelected(_) | Message::FormOpen(FormId::Limits) => {}
            Message::ToolToggled(tool, on) => return self.set_enabled(vec![tool], on),
            Message::ServerToggled(name) => {
                let s = self.servers.iter().find(|s| s.name == name)?;
                let on = catalog::server_toggle(s, self.disabled_tools().unwrap_or(&BTreeSet::new())) != Toggle::On;
                return self.set_enabled(s.tools.iter().map(|t| ToolId::mcp(&s.name, &t.name)).collect(), on);
            }
            Message::Server(name, action) => {
                let s = self.servers.iter().find(|s| s.name == name)?;
                return (action != ServerAction::Remove || s.kind != ServerKind::AppHosted).then_some(ToolsIntent::Server { name, action });
            }
            Message::FormOpen(FormId::Server) => { self.add_server.get_or_insert_default(); }
            Message::FormOpen(FormId::Host) => { self.add_host.get_or_insert_default(); }
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
                return Some(ToolsIntent::AddServer(spec));
            }
            Message::FormSubmit(FormId::Host) => {
                let aliases: Vec<_> = self.hosts.iter().map(|h| h.alias.clone()).collect();
                let host = self.add_host.as_mut()?.submit(&aliases)?;
                self.add_host = None;
                return Some(ToolsIntent::AddHost(host));
            }
            Message::FormSubmit(FormId::Limits) => return self.limits.take().map(|d| ToolsIntent::SaveHostLimits { host: d.host, limits: d.limits }),
            Message::FormCancel(FormId::Server) => self.add_server = None,
            Message::FormCancel(FormId::Host) => self.add_host = None,
            Message::FormCancel(FormId::Limits) => self.limits = None,
            Message::ImportScan => {
                (self.imports, self.import_scanning) = (Some(Vec::new()), true);
                return Some(ToolsIntent::ScanImports);
            }
            Message::ImportSelect(i, on) => self.imports.as_mut()?.get_mut(i)?.selected = on,
            Message::ImportSubmit => {
                let chosen: Vec<_> = self.imports.as_ref()?.iter().filter(|e| e.selected).map(|e| e.candidate.clone()).collect();
                return (!chosen.is_empty()).then_some(ToolsIntent::ImportServers(chosen));
            }
            Message::ImportClose => (self.imports, self.import_scanning) = (None, false),
            Message::ImportSshHosts => return Some(ToolsIntent::ImportSshHosts),
            Message::RemoveHost(alias) => {
                self.limits = self.limits.take().filter(|d| d.host != alias);
                return Some(ToolsIntent::RemoveHost(alias));
            }
            Message::EditLimits(alias) => self.limits = Some(LimitsDraft::new(self.hosts.iter().find(|h| h.alias == alias)?)),
            Message::LimitSet(limit, on) => {
                let l = &mut self.limits.as_mut()?.limits;
                *[&mut l.read_only, &mut l.commands_allowed, &mut l.auto_approve][limit as usize] = on;
            }
            Message::LimitsAddPath => self.limits.as_mut()?.add_path(),
            Message::LimitsRemovePath(i) => {
                let paths = &mut self.limits.as_mut()?.limits.allowed_paths;
                if i < paths.len() { paths.remove(i); }
            }
            Message::OpenTerminal(alias) => return Some(ToolsIntent::OpenTerminal(alias)),
            Message::ShowAudit(alias) => {
                self.audit = Some((alias.clone(), Vec::new()));
                return Some(ToolsIntent::LoadAudit(alias));
            }
            Message::CloseAudit => self.audit = None,
            Message::AnswerPrompt(id, answer) => {
                let i = self.prompts.iter().position(|p| p.id == id)?;
                self.prompts.remove(i);
                return Some(ToolsIntent::AnswerPrompt { id, answer });
            }
            Message::PeerFilter(choice) => self.peer_filter = if let PeerChoice::Peer { key, .. } = choice { Some(key) } else { None },
            Message::BlockOpen(key, open) => { self.open_blocks.insert(key, open); }
            Message::ToolExpanded(tool) => self.expanded_tool = (self.expanded_tool.as_ref() != Some(&tool)).then_some(tool),
        }
        None
    }
    /// Applies a toggle at once and asks the app to persist it; needs a project.
    fn set_enabled(&mut self, tools: Vec<ToolId>, enabled: bool) -> Option<ToolsIntent> {
        let project = self.project.clone().filter(|_| !tools.is_empty())?;
        let disabled = self.disabled.entry(project.clone()).or_default();
        for t in &tools {
            if enabled { disabled.remove(t); } else { disabled.insert(t.clone()); }
        }
        self.rebuild();
        Some(ToolsIntent::SetToolsEnabled { project, tools, enabled })
    }
}

/// Sets a field and clears its error.
fn edit<F: Form + FormSpec<Field = FieldId>>(form: &mut FormState<F>, key: FieldId, value: &str) {
    let _ = form.spec.set(key, value);
    form.errors.remove(key);
}

panel_pane!("tools", "Tools", Wrench);

#[cfg(test)]
mod tests;
