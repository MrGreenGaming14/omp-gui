//! Tools: the Tools panel tab (built in tools, MCP servers, Remote hosts,
//! what each peer may use, per project toggles), the tools every session
//! gets (registered MCP servers, the app-hosted `remote` server, the omp
//! tools bridge for other harnesses) and the Remote shell tab.

mod listing;
mod remote;
#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::futures::SinkExt;
use iced::{Element, Font, Subscription, Task};
use loa_adapter::{Answer, HostToolRouter, McpTransport, SessionConfig};
use loa_bus::SessionId;
use loa_config::ProjectSettings;
use loa_core::{Dom, ElementKind, NodeId, attr};
use loa_mcp::central::{self, ConfigServers, ProjectFilter};
use loa_mcp::host_tools::McpHostTools;
use loa_mcp::import::{self, ConfigFile, Found};
use loa_mcp::{AllowAll, Registry};
use loa_omp_bridge::OmpBridgeConfig;
use loa_remote::host_tools::RemoteHostTools;
use loa_remote::http::RemoteHttp;
use loa_remote::mcp::SERVER_NAME as REMOTE_SERVER;
use loa_remote::ports::PromptQueue;
use loa_remote::RemoteServiceConfig;
use loa_ui_palette as palette;
use loa_ui_terminal as terminal;
use loa_ui_tools as tools;

use self::remote::{Remote, SshPaths, SshPty};
use crate::area::{Area, AreaId, Ctx, HostToolSet, PanelTab, SessionEvent, Target};

/// Id of the Tools panel tab.
const TAB: &str = "tools";
/// Id of the Remote shell tab: terminals on Remote hosts.
const SHELL_TAB: &str = "remote-shell";

/// The harness whose built in tools the app can list, and which serves
/// them to the other harnesses through the bridge.
const OMP: &str = "omp";

/// Palette command names.
const SERVERS: &str = "servers";
const HOSTS: &str = "hosts";
const ADD_SERVER: &str = "add-server";
const IMPORT_SERVERS: &str = "import-servers";
const IMPORT_HOSTS: &str = "import-hosts";
const SHELL: &str = "shell.";

/// Whether `decision` is a card the `remote` server raised (a host key or
/// approval question), answered by this area rather than the harness.
pub fn is_remote_decision(dom: &Dom, decision: NodeId) -> bool {
    dom.get(decision).is_some_and(|node| {
        node.kind == ElementKind::Decision
            && node.str_attr(attr::REQUEST_ID).is_some_and(|id| id.starts_with(remote::REQUEST_PREFIX))
    })
}

/// A value moved through a message by reference.
pub struct Shared<T>(pub Arc<T>);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> std::fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(std::any::type_name::<T>())
    }
}

type Servers = Registry<ConfigServers>;

#[derive(Clone, Debug)]
pub enum Message {
    Panel(tools::Message),
    /// Show the Tools tab (the sidebar's Tools button).
    Open,
    /// A remote host question's card in a session was answered.
    AnswerDecision { session: SessionId, decision: NodeId, answer: Answer },
    /// The registered MCP servers were loaded and started.
    ServersOpened(Result<Shared<Servers>, String>),
    /// The `remote` server's loopback HTTP endpoint is up (or failed).
    RemoteServing(Result<Arc<RemoteHttp>, String>),
    /// A harness's built in tools were read.
    HarnessTools { harness: String, result: Result<Vec<tools::ToolInfo>, String> },
    /// A server's status or tools changed.
    ServersChanged,
    /// A remote host question arrived or went away.
    PromptsChanged,
    /// The `.claude`, `.cursor` and `.codex` configs were read.
    Scanned { found: Vec<Found>, errors: Vec<String> },
    Imported(Result<import::ImportReport, String>),
    Audit { host: String, result: Result<Vec<tools::AuditRow>, String> },
    /// A server action finished: a notice, or the error.
    Done(Result<Option<String>, String>),
    Terminal(terminal::state::Message),
}

pub struct State {
    pub(crate) panel: tools::State,
    mcp: loa_mcp::Manager,
    /// The servers of the central config, once loaded.
    servers: Option<Arc<Servers>>,
    pub(crate) remote: Remote,
    pub(crate) ssh: SshPaths,
    /// Built in tools per harness, as read (or why they couldn't be).
    harness_tools: HashMap<String, Result<Vec<tools::ToolInfo>, String>>,
    /// omp's tools are being read.
    omp_loading: bool,
    /// The harness and app tools the Built in section shows.
    shown: (Option<String>, Vec<tools::ToolInfo>),
    /// Entries of the last import scan.
    found: Vec<Found>,
    /// Project the toggles apply to.
    project: String,
    terminal: terminal::state::State,
}

impl State {
    pub fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let ssh = SshPaths::from_env();
        let (remote, warning) = Remote::new(&ctx.services.dirs.data, RemoteServiceConfig::from_home(&ssh.home));
        if let Some(warning) = warning {
            ctx.notify(warning);
        }
        let metrics = terminal::canvas::Metrics::measure(Font::MONOSPACE, 13.0);
        let terminal = terminal::state::State::new(metrics, terminal::LocalShell::login_shell(ctx.services.cwd.clone()))
            .with_remote(Arc::new(SshPty(Arc::clone(&remote.service))));
        let mut state = Self {
            panel: tools::State::new(),
            mcp: ctx.services.mcp.clone(),
            servers: None,
            remote,
            ssh,
            harness_tools: HashMap::new(),
            omp_loading: false,
            shown: (None, Vec::new()),
            found: Vec::new(),
            project: ctx.services.project.clone(),
            terminal,
        };
        state.reload_hosts(ctx);
        state.refresh_projects(ctx);
        let harness = state.refresh_builtin(ctx);
        state.refresh_servers(ctx);
        let store = ConfigServers::new(ctx.services.config.clone());
        let manager = state.mcp.clone();
        let open = Task::perform(
            async move { Registry::open(store, manager).await.map(|servers| Shared(Arc::new(servers))).map_err(|error| error.to_string()) },
            Message::ServersOpened,
        );
        let service = Arc::clone(&state.remote.service);
        let serve = Task::perform(
            async move { RemoteHttp::start(service).await.map(Arc::new).map_err(|error| error.to_string()) },
            Message::RemoteServing,
        );
        (state, Task::batch([open, serve, harness]))
    }

    /// The settings of `project` in the central config.
    fn settings(project: &str, ctx: &Ctx) -> ProjectSettings {
        ctx.services.config.load().ok().and_then(|loaded| loaded.config.projects.get(project).cloned()).unwrap_or_default()
    }

    /// The harness the Built in section describes: the active session's,
    /// else the one new sessions start on.
    fn harness(ctx: &Ctx) -> Option<String> {
        ctx.sessions.active().and_then(|session| session.harness_id()).map(str::to_owned).or_else(|| ctx.sessions.harness.clone())
    }

    /// The omp executable, when omp runs here.
    fn omp(ctx: &Ctx) -> Option<PathBuf> {
        ctx.services.harnesses.runnable(OMP)?;
        Some(ctx.services.harnesses.get(OMP).and_then(|harness| harness.executable.clone()).unwrap_or_else(|| PathBuf::from(OMP)))
    }

    /// Shows the harness and app tools; starts reading the harness's tools
    /// when they haven't been read.
    fn refresh_builtin(&mut self, ctx: &Ctx) -> Task<Message> {
        let harness = Self::harness(ctx);
        self.shown = (harness.clone(), ctx.app_tools.clone());
        let label = harness.as_deref().map(|id| ctx.services.harnesses.label(id).to_owned()).unwrap_or_default();
        let (harness_tools, harness_note, load) = match harness.as_deref() {
            None => (Vec::new(), Some("No harness is available".to_owned()), None),
            Some(OMP) => match self.harness_tools.get(OMP) {
                Some(Ok(tools)) => (tools.clone(), None, None),
                Some(Err(error)) => (Vec::new(), Some(format!("Can't read omp's tools: {error}")), None),
                None => match Self::omp(ctx) {
                    Some(program) => (Vec::new(), Some("Reading omp's tools…".to_owned()), Some(program)),
                    None => (Vec::new(), Some("omp isn't installed here".to_owned()), None),
                },
            },
            Some(_) => (Vec::new(), Some(format!("{label} doesn't report its built in tools to the app")), None),
        };
        self.panel.set_builtin(tools::BuiltinTools {
            harness: label,
            harness_tools,
            app_tools: ctx.app_tools.clone(),
            harness_note,
        });
        let Some(program) = load else { return Task::none() };
        if std::mem::replace(&mut self.omp_loading, true) {
            return Task::none();
        }
        let mut config = OmpBridgeConfig::new(ctx.services.cwd.clone(), ctx.services.dirs.data.join("omp-bridge").join("tools"));
        config.program = program;
        Task::perform(
            async move {
                loa_omp_bridge::list_omp_tools(&config)
                    .await
                    .map(|specs| {
                        specs.into_iter().map(|spec| tools::ToolInfo::new(spec.name, listing::short_description(&spec.description))).collect()
                    })
                    .map_err(|error| error.to_string())
            },
            |result| Message::HarnessTools { harness: OMP.to_owned(), result },
        )
    }

    /// Refreshes the Built in section when the harness or the app tools
    /// other areas publish changed.
    fn follow_builtin(&mut self, ctx: &Ctx) -> Task<Message> {
        if self.shown.0 != Self::harness(ctx) || self.shown.1 != ctx.app_tools {
            self.refresh_builtin(ctx)
        } else {
            Task::none()
        }
    }

    /// The `omp` bridge server row: omp's tools served to the harnesses
    /// that connect to MCP servers.
    fn bridge_row(&self, ctx: &Ctx) -> tools::McpServer {
        let described = self.harness_tools.get(OMP).and_then(|tools| tools.as_ref().ok());
        let tools = loa_omp_bridge::DEFAULT_TOOLS
            .iter()
            .map(|name| {
                let description = described
                    .and_then(|tools| tools.iter().find(|tool| tool.name == *name))
                    .map_or_else(|| format!("omp's `{name}` tool"), |tool| tool.description.clone());
                tools::ToolInfo::new(*name, description)
            })
            .collect();
        let status = match Self::omp(ctx) {
            Some(_) => tools::ServerStatus::Running,
            None => tools::ServerStatus::Error("omp isn't installed".to_owned()),
        };
        let command = std::env::current_exe().map_or_else(|_| "loa-app".to_owned(), |exe| exe.display().to_string());
        let transport = McpTransport::Stdio { command, args: vec!["omp-bridge".to_owned()], env: Default::default() };
        listing::app_server(loa_omp_bridge::session::SERVER_NAME, transport, status, tools)
    }

    fn remote_row(&self) -> tools::McpServer {
        let (status, url) = match (&self.remote.http, &self.remote.http_error) {
            (Some(http), _) => (tools::ServerStatus::Running, http.url().to_owned()),
            (None, Some(error)) => (tools::ServerStatus::Error(error.clone()), String::new()),
            (None, None) => (tools::ServerStatus::Starting, String::new()),
        };
        let tools = loa_remote::host_tools::specs()
            .iter()
            .map(|spec| tools::ToolInfo::new(spec.name.clone(), listing::short_description(&spec.description)))
            .collect();
        listing::app_server(REMOTE_SERVER, McpTransport::Http { url, headers: Default::default() }, status, tools)
    }

    /// Lists the app hosted servers and every registered one.
    fn refresh_servers(&mut self, ctx: &mut Ctx) {
        let mut servers = vec![self.remote_row(), self.bridge_row(ctx)];
        servers.extend(self.mcp.view(&AllowAll).iter().map(listing::server));
        self.refresh_peers(&servers, ctx);
        self.panel.set_servers(servers);
    }

    /// What each peer may use of `servers` and the built in tools, from
    /// the grants this node issued.
    fn refresh_peers(&mut self, servers: &[tools::McpServer], ctx: &mut Ctx) {
        let issued = match ctx.services.grants.issued() {
            Ok(issued) => issued,
            Err(error) => {
                self.panel.set_peers(Vec::new());
                ctx.notify(format!("Can't read the grants: {error}"));
                return;
            }
        };
        let hosts: Vec<String> =
            ctx.services.config.load().map(|loaded| loaded.config.remote_hosts.into_keys().collect()).unwrap_or_default();
        self.panel.set_peers(listing::peer_tools(&issued, servers, &hosts, loa_core::now_ms()));
    }

    /// The project selector and the toggles of the selected project.
    fn refresh_projects(&mut self, ctx: &Ctx) {
        let config = ctx.services.config.load().map(|loaded| loaded.config).unwrap_or_default();
        let mut ids: BTreeSet<String> = config.projects.keys().cloned().collect();
        ids.insert(ctx.services.project.clone());
        ids.insert(self.project.clone());
        self.panel.set_projects(ids.into_iter().map(|id| tools::ProjectRef { name: id.clone(), id }).collect());
        self.panel.select_project(self.project.clone());
        let settings = config.projects.get(&self.project).cloned().unwrap_or_default();
        self.panel.set_disabled_tools(self.project.clone(), disabled(&settings));
    }

    /// Rebuilds the `remote` server's hosts from the central config and
    /// `~/.ssh/config`, and lists them.
    pub(crate) fn reload_hosts(&mut self, ctx: &mut Ctx) {
        let entries = match ctx.services.config.load() {
            Ok(loaded) => loaded.config.remote_hosts,
            Err(error) => {
                ctx.notify(format!("Can't read the Remote hosts: {error}"));
                return;
            }
        };
        let ssh = match self.ssh.ssh_config() {
            Ok(ssh) => ssh,
            Err(error) => {
                ctx.notify(error);
                let Some(empty) = self.ssh.empty() else { return };
                empty
            }
        };
        let (hosts, failed) = loa_remote::config::load(&entries, &ssh, &self.ssh.user);
        self.remote.service.hosts().replace_all(hosts);
        self.panel.set_hosts(remote::panel_hosts(&entries));
        if !failed.is_empty() {
            let failed: Vec<String> = failed.iter().map(|(name, error)| format!("{name}: {error}")).collect();
            ctx.notify(format!("Some Remote hosts can't be reached as configured: {}", failed.join("; ")));
        }
    }

    /// Changes the Remote hosts table of the central config with `change`
    /// (re-read right before saving), then reloads the hosts.
    fn edit_hosts(
        &mut self,
        ctx: &mut Ctx,
        change: impl FnOnce(&mut std::collections::BTreeMap<String, loa_config::RemoteHost>) -> Result<Option<String>, String>,
    ) {
        let saved = ctx.services.config.load().map_err(|error| error.to_string()).and_then(|loaded| {
            let mut config = loaded.config;
            let notice = change(&mut config.remote_hosts)?;
            ctx.services.config.save(&config).map_err(|error| error.to_string())?;
            Ok(notice)
        });
        match saved {
            Ok(notice) => {
                if let Some(notice) = notice {
                    ctx.notify(notice);
                }
                self.reload_hosts(ctx);
            }
            Err(error) => ctx.notify(format!("Can't save the Remote hosts: {error}")),
        }
        self.refresh_servers(ctx);
    }

    /// Turns tools on or off in `project` and saves it.
    fn set_tools(&mut self, project: &str, ids: &[tools::ToolId], enabled: bool, ctx: &mut Ctx) {
        let saved = ctx.services.config.load().and_then(|loaded| {
            let mut config = loaded.config;
            let settings = config.projects.entry(project.to_owned()).or_default();
            for id in ids {
                match id {
                    tools::ToolId::Builtin(name) => {
                        if enabled {
                            settings.disabled_tools.remove(name);
                        } else {
                            settings.disabled_tools.insert(name.clone());
                        }
                    }
                    tools::ToolId::Mcp { server, tool } => {
                        let off = settings.disabled_mcp_tools.entry(server.clone()).or_default();
                        if enabled {
                            off.remove(tool);
                        } else {
                            off.insert(tool.clone());
                        }
                        if off.is_empty() {
                            settings.disabled_mcp_tools.remove(server);
                        }
                    }
                }
            }
            ctx.services.config.save(&config)
        });
        if let Err(error) = saved {
            ctx.notify(format!("Can't save the tool toggles: {error}"));
        }
        // The panel shows what is stored, also after a failed save.
        let settings = Self::settings(project, ctx);
        self.panel.set_disabled_tools(project.to_owned(), disabled(&settings));
    }

    fn servers_or_notice(&self, ctx: &mut Ctx) -> Option<Arc<Servers>> {
        if self.servers.is_none() {
            ctx.notify("The MCP servers are still loading");
        }
        self.servers.clone()
    }

    /// Shows the Tools tab on `section`.
    fn show(&mut self, section: tools::Section, ctx: &mut Ctx) -> Task<Message> {
        let _ = self.panel.update(tools::Message::SectionSelected(section));
        self.open(ctx)
    }

    /// Opens the Tools tab with fresh lists.
    fn open(&mut self, ctx: &mut Ctx) -> Task<Message> {
        ctx.open_tab(TAB);
        let task = self.refresh_builtin(ctx);
        self.refresh_servers(ctx);
        self.refresh_projects(ctx);
        task
    }

    /// Opens a shell on Remote host `host` in the Remote shell tab.
    fn open_shell(&mut self, host: String, ctx: &mut Ctx) -> Task<Message> {
        let (_, task) = self.terminal.open(terminal::Target::Remote { host });
        ctx.open_tab(SHELL_TAB);
        task.map(Message::Terminal)
    }

    fn panel_event(&mut self, event: tools::Event, ctx: &mut Ctx) -> Task<Message> {
        match event {
            tools::Event::ProjectSelected(project) => {
                self.project = project;
                self.refresh_projects(ctx);
                Task::none()
            }
            tools::Event::SetToolsEnabled { project, tools, enabled } => {
                self.set_tools(&project, &tools, enabled, ctx);
                Task::none()
            }
            tools::Event::Server { name, action } => {
                let Some(servers) = self.servers_or_notice(ctx) else { return Task::none() };
                Task::perform(
                    async move {
                        let result = match action {
                            tools::ServerAction::Start => servers.set_enabled(&name, true).await,
                            tools::ServerAction::Stop => servers.set_enabled(&name, false).await,
                            tools::ServerAction::Restart => servers.manager().restart(&name).await,
                            tools::ServerAction::Remove => servers.remove(&name).await,
                        };
                        result.map(|()| None).map_err(|error| format!("{name}: {error}"))
                    },
                    Message::Done,
                )
            }
            tools::Event::AddServer(spec) => {
                let Some(servers) = self.servers_or_notice(ctx) else { return Task::none() };
                ctx.notify(format!("Connecting to {}…", spec.name));
                Task::perform(
                    async move {
                        let name = spec.name.clone();
                        match servers.add(central::from_session_spec(&spec)).await {
                            Ok(tools) => Ok(Some(format!("Added {name} with {} tools", tools.len()))),
                            Err(error) => Err(format!("Can't add {name}: {error}")),
                        }
                    },
                    Message::Done,
                )
            }
            tools::Event::ScanImports => {
                let files = ConfigFile::defaults(&self.ssh.home, Some(&ctx.services.cwd));
                Task::perform(
                    async move {
                        let scan = tokio::task::spawn_blocking(move || import::scan(&files)).await;
                        let (found, errors) = scan.unwrap_or_else(|error| (Vec::new(), vec![loa_mcp::import::FileError {
                            file: PathBuf::new(),
                            message: error.to_string(),
                        }]));
                        let mut notes: Vec<String> =
                            errors.iter().map(|error| format!("{}: {}", error.file.display(), error.message)).collect();
                        notes.extend(found.iter().filter_map(|entry| {
                            entry.spec.as_ref().err().map(|reason| format!("{} in {}: {reason}", entry.name, entry.file.display()))
                        }));
                        (found, notes)
                    },
                    |(found, errors)| Message::Scanned { found, errors },
                )
            }
            tools::Event::ImportServers(chosen) => {
                let Some(servers) = self.servers_or_notice(ctx) else { return Task::none() };
                let picked = listing::picked(&self.found, &chosen);
                Task::perform(
                    async move { servers.import_found(picked).await.map_err(|error| error.to_string()) },
                    Message::Imported,
                )
            }
            tools::Event::ImportSshHosts => {
                let ssh = self.ssh.ssh_config();
                self.edit_hosts(ctx, |entries| {
                    let added = loa_remote::config::import_aliases(entries, &ssh?);
                    Ok(Some(match added.len() {
                        0 => "No new hosts in ~/.ssh/config".to_owned(),
                        n => format!("Imported {n} hosts from ~/.ssh/config, read only until you open their limits"),
                    }))
                });
                Task::none()
            }
            tools::Event::AddHost(host) => {
                self.edit_hosts(ctx, |entries| {
                    entries.insert(host.alias.clone(), remote::config_host(&host));
                    Ok(None)
                });
                Task::none()
            }
            tools::Event::RemoveHost(alias) => {
                self.edit_hosts(ctx, |entries| {
                    entries.remove(&alias);
                    Ok(None)
                });
                let service = Arc::clone(&self.remote.service);
                Task::future(async move { service.disconnect(&alias).await }).discard()
            }
            tools::Event::SaveHostLimits { host, limits } => {
                self.edit_hosts(ctx, |entries| {
                    let entry = entries.get_mut(&host).ok_or_else(|| format!("{host} isn't a Remote host"))?;
                    entry.allowed_paths = limits.allowed_paths;
                    entry.read_only = limits.read_only;
                    entry.commands = limits.commands_allowed;
                    entry.auto_approve = limits.auto_approve;
                    Ok(None)
                });
                Task::none()
            }
            tools::Event::OpenTerminal(host) => self.open_shell(host, ctx),
            tools::Event::LoadAudit(host) => {
                let audit = self.remote.audit(host.clone());
                Task::perform(audit, move |result| Message::Audit { host: host.clone(), result })
            }
            tools::Event::AnswerPrompt { id, answer } => {
                if !self.remote.answer_panel(id, answer, ctx) {
                    ctx.notify("That remote host question is no longer waiting");
                }
                Task::none()
            }
        }
    }

    /// The `omp` bridge for a session of another harness: omp's tools minus
    /// the ones the project switched off, with its state under the
    /// session's own directory.
    fn bridge_spec(config: &SessionConfig, settings: &ProjectSettings, ctx: &Ctx) -> Option<loa_adapter::McpServerSpec> {
        let program = Self::omp(ctx)?;
        let exe = std::env::current_exe().ok()?;
        let state = ctx.services.dirs.data.join("omp-bridge").join(&config.session_id);
        let mut bridge = OmpBridgeConfig::new(config.cwd.clone(), state);
        bridge.program = program;
        bridge.tools.retain(|tool| settings.mcp_tool_enabled(loa_omp_bridge::session::SERVER_NAME, tool));
        (!bridge.tools.is_empty()).then(|| loa_omp_bridge::session::session_spec(&exe, &["omp-bridge"], &bridge))
    }
}

/// The switched off tools of a project as panel ids.
fn disabled(settings: &ProjectSettings) -> Vec<tools::ToolId> {
    let mut ids: Vec<tools::ToolId> = settings.disabled_tools.iter().map(tools::ToolId::builtin).collect();
    for (server, names) in &settings.disabled_mcp_tools {
        ids.extend(names.iter().map(|tool| tools::ToolId::mcp(server, tool)));
    }
    ids
}

impl Area for State {
    type Message = Message;

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        let follow = self.follow_builtin(ctx);
        let task = match message {
            Message::Panel(message) => {
                let (task, event) = self.panel.update(message);
                let action = event.map_or_else(Task::none, |event| self.panel_event(event, ctx));
                Task::batch([task.map(Message::Panel), action])
            }
            Message::Open => self.open(ctx),
            Message::AnswerDecision { session, decision, answer } => {
                let result = self.remote.answer_card(&session, decision, &answer, ctx);
                if let Err(error) = &result {
                    ctx.notify(error.clone());
                }
                let answered =
                    crate::areas::decisions::Message::Answered { session: session.to_string(), decision, result };
                ctx.request(crate::area::Request::Forward(Box::new(crate::app::Message::Decisions(answered))));
                Task::none()
            }
            Message::ServersOpened(Ok(Shared(servers))) => {
                self.servers = Some(servers);
                self.refresh_servers(ctx);
                Task::none()
            }
            Message::ServersOpened(Err(error)) => {
                ctx.notify(format!("Can't load the MCP servers: {error}"));
                Task::none()
            }
            Message::RemoteServing(result) => {
                match result {
                    Ok(http) => self.remote.http = Some(http),
                    Err(error) => {
                        ctx.notify(format!("The remote server can't serve MCP clients: {error}"));
                        self.remote.http_error = Some(error);
                    }
                }
                self.refresh_servers(ctx);
                Task::none()
            }
            Message::HarnessTools { harness, result } => {
                self.omp_loading = false;
                self.harness_tools.insert(harness, result);
                let task = self.refresh_builtin(ctx);
                self.refresh_servers(ctx);
                task
            }
            Message::ServersChanged => {
                self.refresh_servers(ctx);
                Task::none()
            }
            Message::PromptsChanged => {
                const WAITING: &str = "Remote host question waiting: ";
                let (prompts, new) = self.remote.sync(ctx);
                if new && let Some(prompt) = prompts.last() {
                    ctx.notify(format!("{WAITING}{}", prompt.title));
                    let _ = self.panel.update(tools::Message::SectionSelected(tools::Section::Hosts));
                    ctx.open_tab(TAB);
                } else if prompts.is_empty() && ctx.notice().is_some_and(|notice| notice.starts_with(WAITING)) {
                    // Every question is answered or cancelled: the notice is stale.
                    ctx.clear_notice();
                }
                self.panel.set_prompts(prompts);
                Task::none()
            }
            Message::Scanned { found, errors } => {
                self.panel.set_import_candidates(listing::candidates(&found));
                self.found = found;
                if !errors.is_empty() {
                    ctx.notify(format!("Some entries can't be imported: {}", errors.join("; ")));
                }
                Task::none()
            }
            Message::Imported(Ok(report)) => {
                for (source, name, outcome) in listing::outcomes(&report) {
                    self.panel.set_import_outcome(source, &name, outcome);
                }
                self.refresh_servers(ctx);
                Task::none()
            }
            Message::Imported(Err(error)) => {
                ctx.notify(format!("Can't import the servers: {error}"));
                Task::none()
            }
            Message::Audit { host, result } => {
                match result {
                    Ok(rows) => self.panel.set_audit(host, rows),
                    Err(error) => ctx.notify(format!("Can't read the audit log of {host}: {error}")),
                }
                Task::none()
            }
            Message::Done(result) => {
                match result {
                    Ok(Some(notice)) | Err(notice) => ctx.notify(notice),
                    Ok(None) => {}
                }
                self.refresh_servers(ctx);
                Task::none()
            }
            Message::Terminal(message) => self.terminal.update(message).0.map(Message::Terminal),
        };
        Task::batch([follow, task])
    }

    fn palette_commands(&self, ctx: &Ctx) -> Vec<palette::Command> {
        let group = "Tools";
        let mut commands = vec![
            palette::Command::new(AreaId::Tools.command(SERVERS), "MCP servers", group),
            palette::Command::new(AreaId::Tools.command(ADD_SERVER), "Add MCP server", group),
            palette::Command::new(AreaId::Tools.command(IMPORT_SERVERS), "Import MCP servers from .claude, .cursor, .codex", group),
            palette::Command::new(AreaId::Tools.command(HOSTS), "Remote hosts", group),
            palette::Command::new(AreaId::Tools.command(IMPORT_HOSTS), "Import Remote hosts from ~/.ssh/config", group),
        ];
        if let Ok(loaded) = ctx.services.config.load() {
            commands.extend(loaded.config.remote_hosts.keys().map(|host| {
                palette::Command::new(AreaId::Tools.command(&format!("{SHELL}{host}")), format!("Open a shell on {host}"), group)
            }));
        }
        commands
    }

    fn run_command(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Message>> {
        let name = id.strip_prefix(AreaId::Tools.key())?.strip_prefix('.')?;
        Some(match name {
            SERVERS => self.show(tools::Section::Servers, ctx),
            HOSTS => self.show(tools::Section::Hosts, ctx),
            ADD_SERVER => {
                let task = self.show(tools::Section::Servers, ctx);
                let _ = self.panel.update(tools::Message::AddServerOpen);
                task
            }
            IMPORT_SERVERS => {
                let task = self.show(tools::Section::Servers, ctx);
                let (_, event) = self.panel.update(tools::Message::ImportScan);
                let scan = event.map_or_else(Task::none, |event| self.panel_event(event, ctx));
                Task::batch([task, scan])
            }
            IMPORT_HOSTS => {
                let task = self.show(tools::Section::Hosts, ctx);
                Task::batch([task, self.panel_event(tools::Event::ImportSshHosts, ctx)])
            }
            _ => self.open_shell(name.strip_prefix(SHELL)?.to_owned(), ctx),
        })
    }

    fn panel_tabs(&self) -> Vec<PanelTab> {
        vec![PanelTab::new(TAB, "Tools", true), PanelTab::new(SHELL_TAB, "Remote shell", false)]
    }

    fn view_tab<'a>(&'a self, id: &str, _: &'a Ctx) -> Option<Element<'a, Message>> {
        match id {
            TAB => Some(self.panel.view().map(Message::Panel)),
            SHELL_TAB => Some(self.terminal.view().map(Message::Terminal)),
            _ => None,
        }
    }

    fn contribute(&self, config: &mut SessionConfig, target: &Target<'_>, ctx: &Ctx) {
        let capabilities = target.capabilities;
        // A harness that takes neither MCP servers nor host tools runs
        // without the app's tools (omp2 refuses any tool config).
        if !capabilities.mcp_servers && !capabilities.host_tools {
            return;
        }
        let project = config.project.clone().unwrap_or_else(|| ctx.services.project.clone());
        let settings = Self::settings(&project, ctx);
        for name in central::session_disabled_tools(&settings) {
            if !config.disabled_tools.contains(&name) {
                config.disabled_tools.push(name);
            }
        }
        if !capabilities.mcp_servers {
            return;
        }
        config.mcp_servers.extend(self.mcp.specs().iter().filter(|spec| spec.enabled).map(central::session_spec));
        if let Some(spec) = self.remote.http.as_ref().and_then(|http| http.session_spec(&config.session_id).ok()) {
            config.mcp_servers.push(spec);
        }
        if target.harness != OMP
            && let Some(spec) = Self::bridge_spec(config, &settings, ctx)
        {
            config.mcp_servers.push(spec);
        }
    }

    fn host_tools(&self, config: &SessionConfig, target: &Target<'_>, ctx: &Ctx) -> Option<HostToolSet> {
        // Harnesses with MCP support reach the same tools as servers.
        if !target.capabilities.host_tools || target.capabilities.mcp_servers {
            return None;
        }
        let project = config.project.clone().unwrap_or_else(|| ctx.services.project.clone());
        let settings = Self::settings(&project, ctx);
        let mut router = HostToolRouter::new();
        let mcp = Arc::new(McpHostTools::new(self.mcp.clone(), Arc::new(ProjectFilter::new(settings.clone()))));
        for spec in mcp.specs() {
            let _ = router.add(spec.clone(), Arc::clone(&mcp) as _);
        }
        let remote = Arc::new(RemoteHostTools::new(Arc::clone(&self.remote.service)));
        for spec in loa_remote::host_tools::specs() {
            if settings.mcp_tool_enabled(REMOTE_SERVER, &spec.name) {
                let _ = router.add(spec, Arc::clone(&remote) as _);
            }
        }
        (!router.is_empty()).then(|| HostToolSet { specs: router.specs().to_vec(), handler: Arc::new(router) })
    }

    fn on_session_event(&mut self, event: &SessionEvent, ctx: &mut Ctx) -> Task<Message> {
        match event {
            SessionEvent::Loaded(id) => self.remote.cancel_stale(id, ctx),
            SessionEvent::Closed(id) => self.remote.forget(id.as_str()),
            _ => {}
        }
        self.follow_builtin(ctx)
    }

    fn project_activated(&mut self, project: &str, cwd: &Path, ctx: &mut Ctx) -> Task<Message> {
        self.terminal.set_new_tab(terminal::LocalShell::login_shell(cwd));
        project.clone_into(&mut self.project);
        self.refresh_projects(ctx);
        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run_with(McpWatch(self.mcp.clone()), mcp_events),
            Subscription::run_with(QueueWatch(Arc::clone(&self.remote.queue)), prompt_changes),
        ])
    }
}

/// The app's one MCP manager, as a subscription key.
struct McpWatch(loa_mcp::Manager);

impl std::hash::Hash for McpWatch {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "tools.mcp".hash(state);
    }
}

fn mcp_events(watch: &McpWatch) -> impl iced::futures::Stream<Item = Message> + use<> {
    let mut events = watch.0.events();
    iced::stream::channel(16, async move |mut out: iced::futures::channel::mpsc::Sender<Message>| {
        loop {
            match events.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if out.send(Message::ServersChanged).await.is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}

/// The remote question queue, as a subscription key.
struct QueueWatch(Arc<PromptQueue>);

impl std::hash::Hash for QueueWatch {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

fn prompt_changes(watch: &QueueWatch) -> impl iced::futures::Stream<Item = Message> + use<> {
    let mut changes = watch.0.subscribe();
    iced::stream::channel(16, async move |mut out: iced::futures::channel::mpsc::Sender<Message>| {
        while changes.changed().await.is_ok() {
            if out.send(Message::PromptsChanged).await.is_err() {
                return;
            }
        }
    })
}
