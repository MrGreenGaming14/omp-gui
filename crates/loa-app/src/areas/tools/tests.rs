//! The Tools area driven through `App::update` the way the running app
//! drives it: tasks run on a multi-thread runtime and their messages come
//! back through `update`, session feeds, MCP manager events and remote
//! questions are delivered as the subscriptions would.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::Task;
use iced::futures::StreamExt;
use iced::widget::text_editor::{Action as EditAction, Edit};
use iced_runtime::Action as RuntimeAction;
use loa_adapter::{Adapter, Capabilities, HarnessSession, Registry as Adapters, SessionConfig};
use loa_bus::{Policy, SessionId};
use loa_core::{DecisionStatus, Dom, ElementKind, PatchSink, TurnStatus, attr};
use loa_grants::{GrantSpec, NodeKey, Resource, Scope};
use loa_identity::NodeIdentity;
use loa_remote::connection::ConnectorConfig;
use loa_ui_composer as composer;
use loa_ui_decisions::desk;
use tokio::sync::{broadcast, mpsc, watch};

use super::*;
use crate::app::tests::FakeAdapter;
use crate::app::{App, Message as AppMessage};
use crate::areas::decisions;
use crate::services::{Dirs, Harnesses, Node, Services};
use crate::session::{Feed, dom_turns};

#[path = "../../../../loa-remote/tests/support/mod.rs"]
#[allow(clippy::all)]
mod ssh;

const WAIT: Duration = Duration::from_secs(30);
const TURN_TIMEOUT: Duration = Duration::from_secs(600);
const PYTHON: &str = if cfg!(windows) { "python" } else { "python3" };

/// Harness ids of the rig: one connects to MCP servers itself, one takes
/// host tools (like omp), one takes neither.
const MCP_HARNESS: &str = "mcp-harness";
const HOST_HARNESS: &str = "host-harness";
const PLAIN_HARNESS: &str = "plain-harness";

/// A fake harness with the given capabilities that records the config of
/// every session it starts.
struct Recording {
    inner: FakeAdapter,
    capabilities: Capabilities,
    configs: Arc<Mutex<Vec<SessionConfig>>>,
}

#[async_trait::async_trait]
impl Adapter for Recording {
    fn harness(&self) -> &'static str {
        self.inner.harness()
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        self.configs.lock().unwrap().push(config.clone());
        self.inner.start(config, sink).await
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        self.configs.lock().unwrap().push(config.clone());
        self.inner.resume(config, dom, sink).await
    }
}

/// The app with its tasks on a multi-thread runtime, the way iced runs
/// them, and the subscriptions of the Tools area fed by hand.
struct Rig {
    app: App,
    runtime: tokio::runtime::Runtime,
    sender: mpsc::UnboundedSender<AppMessage>,
    receiver: mpsc::UnboundedReceiver<AppMessage>,
    feeds: Vec<(SessionId, loa_bus::Subscription)>,
    mcp: broadcast::Receiver<loa_mcp::Event>,
    prompts: watch::Receiver<u64>,
    configs: Arc<Mutex<Vec<SessionConfig>>>,
    /// `$HOME` of the area: `~/.ssh/config` and the import sources.
    home: PathBuf,
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        Self::with(|_| {})
    }

    /// A rig whose harnesses `add` may extend before the app is built.
    fn with(add: impl FnOnce(&mut Harnesses)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let configs = Arc::new(Mutex::new(Vec::new()));
        let mut harnesses = Harnesses { registry: Adapters::new(), list: Vec::new() };
        for (id, mcp_servers, host_tools) in [(MCP_HARNESS, true, false), (HOST_HARNESS, false, true), (PLAIN_HARNESS, false, false)] {
            let capabilities = Capabilities { mcp_servers, host_tools, ..Capabilities::default() };
            let adapter = Recording { inner: FakeAdapter::new(id), capabilities, configs: Arc::clone(&configs) };
            harnesses.add(Arc::new(adapter), id, true, None, None);
        }
        add(&mut harnesses);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let node = Node::new(NodeIdentity::from_secret_bytes(&[9; 32]), "toolhost".into());
        let services = Services::new(node, Dirs::new(dir.path().join("loa")), harnesses, work).unwrap();
        let (sender, receiver) = mpsc::unbounded_channel();
        let (mut app, task) = {
            let _entered = runtime.enter();
            App::build(services)
        };
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        app.areas.tools.ssh = SshPaths { home: home.clone(), user: "tester".into() };
        let mcp = app.ctx.services.mcp.events();
        let prompts = app.areas.tools.remote.queue.subscribe();
        let rig = Self { app, runtime, sender, receiver, feeds: Vec::new(), mcp, prompts, configs, home, dir };
        rig.spawn(task);
        rig
    }

    fn spawn(&self, task: Task<AppMessage>) {
        let Some(stream) = iced_runtime::task::into_stream(task) else { return };
        let sender = self.sender.clone();
        self.runtime.spawn(stream.for_each(move |action| {
            if let RuntimeAction::Output(message) = action {
                let _ = sender.send(message);
            }
            std::future::ready(())
        }));
    }

    fn update(&mut self, message: AppMessage) {
        let task = {
            let _entered = self.runtime.enter();
            self.app.update(message)
        };
        self.spawn(task);
        self.drain();
    }

    fn tools(&mut self, message: Message) {
        self.update(AppMessage::Tools(message));
    }

    fn panel(&mut self, message: tools::Message) {
        self.tools(Message::Panel(message));
    }

    /// Delivers what is ready: task outputs, session feeds, MCP events and
    /// remote questions.
    fn drain(&mut self) {
        loop {
            let mut messages = Vec::new();
            while let Ok(message) = self.receiver.try_recv() {
                messages.push(message);
            }
            for (id, feed) in &mut self.feeds {
                while let Ok(event) = feed.try_recv() {
                    messages.push(AppMessage::Session(id.clone(), Feed::Event(event)));
                }
            }
            let mut servers = false;
            while let Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) = self.mcp.try_recv() {
                servers = true;
            }
            if servers {
                messages.push(AppMessage::Tools(Message::ServersChanged));
            }
            if self.prompts.has_changed().unwrap_or(false) {
                self.prompts.mark_unchanged();
                messages.push(AppMessage::Tools(Message::PromptsChanged));
            }
            if messages.is_empty() {
                return;
            }
            for message in messages {
                let task = {
                    let _entered = self.runtime.enter();
                    self.app.update(message)
                };
                self.spawn(task);
            }
        }
    }

    fn until(&mut self, what: &str, timeout: Duration, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain();
            if done(&self.app) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting until {what}; notice: {:?}", self.app.ctx.notice());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits for the startup tasks: the servers loaded and `remote` serving.
    fn started(mut self) -> Self {
        self.until("the servers load and remote serves", WAIT, |app| {
            app.areas.tools.servers.is_some() && app.areas.tools.remote.http.is_some()
        });
        self
    }

    /// Starts a session on `harness` and follows its feed.
    fn session(&mut self, harness: &str) -> SessionId {
        self.update(AppMessage::Composer(composer::Message::HarnessPicked(harness.to_owned())));
        let before = self.app.ctx.sessions.active_id().cloned();
        self.update(AppMessage::NewSession);
        self.until("the session starts", WAIT, |app| app.ctx.sessions.active_id().is_some_and(|id| Some(id) != before.as_ref()));
        let session = self.app.ctx.sessions.active().unwrap();
        let id = session.id().clone();
        let (dom, feed) = session.writer().subscribe(Policy::every_patch(4096));
        self.feeds.push((id.clone(), feed));
        self.update(AppMessage::Session(id.clone(), Feed::Synced(Arc::new(dom))));
        id
    }

    fn last_config(&self) -> SessionConfig {
        self.configs.lock().unwrap().last().cloned().expect("a session started")
    }

    fn dom(&self, id: &SessionId) -> Dom {
        self.app.ctx.sessions.get(id.as_str()).unwrap().writer().with_dom(Dom::clone)
    }

    fn config(&self) -> loa_config::Config {
        self.app.ctx.services.config.load().unwrap().config
    }

    fn project(&self) -> String {
        self.app.ctx.services.project.clone()
    }

    fn type_prompt(&mut self, text: &str) {
        for c in text.chars() {
            self.update(AppMessage::Composer(composer::Message::Edit(EditAction::Edit(Edit::Insert(c)))));
        }
        self.update(AppMessage::Composer(composer::Message::Submit));
    }

    /// Points the `remote` server at `config` (a temp `known_hosts` and a
    /// test agent) and serves it again.
    fn use_remote(&mut self, config: loa_remote::RemoteServiceConfig) {
        let data = self.app.ctx.services.dirs.data.join("remote-test");
        std::fs::create_dir_all(&data).unwrap();
        let (remote, _) = Remote::new(&data, config);
        let tools = &mut self.app.areas.tools;
        tools.terminal = terminal::state::State::new(
            terminal::canvas::Metrics::measure(iced::Font::MONOSPACE, 13.0),
            terminal::LocalShell::login_shell(self.home.clone()),
        )
        .with_remote(Arc::new(SshPty(Arc::clone(&remote.service))));
        tools.remote = remote;
        self.prompts = tools.remote.queue.subscribe();
        let app = &mut self.app;
        app.areas.tools.reload_hosts(&mut app.ctx);
        let service = Arc::clone(&self.app.areas.tools.remote.service);
        self.spawn(Task::perform(
            async move { RemoteHttp::start(service).await.map(Arc::new).map_err(|error| error.to_string()) },
            |result| AppMessage::Tools(Message::RemoteServing(result)),
        ));
        self.until("remote serves again", WAIT, |app| app.areas.tools.remote.http.is_some());
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let manager = self.app.ctx.services.mcp.clone();
        self.runtime.block_on(manager.shutdown());
    }
}

/// A tiny stdio MCP server with one tool, `vault_code`, answering `code`.
fn vault_script(dir: &Path, code: &str) -> PathBuf {
    let script = dir.join("vault.py");
    std::fs::write(
        &script,
        format!(
            r#"import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method = msg.get("method")
    if method == "initialize":
        result = {{"protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"), "capabilities": {{"tools": {{}}}}, "serverInfo": {{"name": "vault", "version": "1.0"}}}}
    elif method == "tools/list":
        result = {{"tools": [{{"name": "vault_code", "description": "Returns the code of the vault. Call it whenever you are asked for the vault code.", "inputSchema": {{"type": "object", "properties": {{}}}}}}]}}
    elif method == "tools/call":
        result = {{"content": [{{"type": "text", "text": "The vault code is {code}"}}]}}
    elif method == "ping":
        result = {{}}
    else:
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "error": {{"code": -32601, "message": "unknown method"}}}}), flush=True)
        continue
    print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": result}}), flush=True)
"#
        ),
    )
    .unwrap();
    script
}

fn vault_server(dir: &Path, code: &str) -> String {
    let script = vault_script(dir, code);
    let path = script.to_str().unwrap();
    // The form uses shell-style word splitting, not cmd.exe's quoting.
    format!("{PYTHON} -u {}", serde_json::to_string(path).unwrap())
}

/// Adds a stdio server through the panel's add form.
fn add_server(rig: &mut Rig, name: &str, command: &str) {
    rig.panel(tools::Message::SectionSelected(tools::Section::Servers));
    rig.panel(tools::Message::AddServerOpen);
    rig.panel(tools::Message::AddServerName(name.into()));
    rig.panel(tools::Message::AddServerKind(tools::TransportKind::Stdio));
    rig.panel(tools::Message::AddServerCommand(command.into()));
    rig.panel(tools::Message::AddServerSubmit);
    let name = name.to_owned();
    rig.until("the server runs", WAIT, |app| {
        app.areas.tools.panel.catalog().servers.iter().any(|view| {
            view.server.name == name && view.server.status == tools::ServerStatus::Running && !view.server.tools.is_empty()
        })
    });
}

fn server<'a>(rig: &'a Rig, name: &str) -> Option<&'a tools::McpServer> {
    rig.app.areas.tools.panel.catalog().servers.into_iter().map(|view| view.server).find(|server| server.name == name)
}

#[test]
fn the_tools_panel_lists_harness_and_app_tools_and_the_app_hosted_servers() {
    let mut rig = Rig::new().started();
    rig.tools(Message::Open);
    assert_eq!(rig.app.ctx.shell.active_tab(), Some(TAB));

    // Another area publishes an app tool with per provider details (the
    // other areas publish theirs at startup too, so look tools up by name).
    let detail = tools::ToolDetail { label: "docker".into(), state: tools::DetailState::Unavailable, text: "not installed".into() };
    rig.app.ctx.set_app_tool(tools::ToolInfo::new("sandbox_task", "Run a task in a sandbox").with_details(vec![detail]));
    rig.tools(Message::Open);
    fn app_tools(rig: &Rig) -> Vec<tools::catalog::ToolRow<'_>> {
        let catalog = rig.app.areas.tools.panel.catalog();
        catalog.builtin.into_iter().find(|group| group.group == tools::BuiltinGroup::App).unwrap().tools
    }
    let listed = app_tools(&rig);
    let sandbox = listed.iter().find(|tool| tool.info.name == "sandbox_task").expect("sandbox_task is listed");
    assert_eq!(sandbox.info.details[0].label, "docker");
    assert_eq!(listed.len(), rig.app.ctx.app_tools.len());

    // The app tools follow ctx.app_tools on any later update too.
    rig.app.ctx.set_app_tool(tools::ToolInfo::new("test_only_tool", "A tool published after the panel opened"));
    rig.tools(Message::ServersChanged);
    let listed = app_tools(&rig);
    assert!(listed.iter().any(|tool| tool.info.name == "test_only_tool"));
    assert_eq!(listed.len(), rig.app.ctx.app_tools.len());

    let remote = server(&rig, "remote").expect("remote is listed");
    assert_eq!((remote.kind, &remote.status), (tools::ServerKind::AppHosted, &tools::ServerStatus::Running));
    assert!(remote.tools.iter().any(|tool| tool.name == "remote_bash"));
    let McpTransport::Http { url, .. } = &remote.transport else { panic!("remote serves over HTTP") };
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    let bridge = server(&rig, "omp").expect("the omp bridge is listed");
    assert_eq!(bridge.kind, tools::ServerKind::AppHosted);
    assert_eq!(bridge.tools.len(), loa_omp_bridge::DEFAULT_TOOLS.len());
}

#[test]
fn an_mcp_server_added_in_the_panel_runs_is_controlled_and_reaches_every_session() {
    let mut rig = Rig::new().started();
    let command = vault_server(rig.dir.path(), "1234");
    add_server(&mut rig, "vault", &command);
    assert!(rig.config().mcp_servers.contains_key("vault"), "saved in the central config");

    // A harness that connects to MCP servers gets vault, remote and (with
    // omp installed) the omp bridge.
    rig.session(MCP_HARNESS);
    let config = rig.last_config();
    let names: Vec<&str> = config.mcp_servers.iter().map(|spec| spec.name.as_str()).collect();
    assert!(names.contains(&"vault") && names.contains(&"remote"), "{names:?}");
    let remote = config.mcp_servers.iter().find(|spec| spec.name == "remote").unwrap();
    let McpTransport::Http { headers, .. } = &remote.transport else { panic!("remote over HTTP") };
    assert!(headers["Authorization"].starts_with("Bearer "));
    assert_eq!(names.contains(&"omp"), rig.app.ctx.services.harnesses.runnable("omp").is_some(), "{names:?}");
    assert!(config.host_tools.is_empty());

    // A harness that takes host tools gets the same tools that way, and
    // they run.
    rig.session(HOST_HARNESS);
    let config = rig.last_config();
    let names: Vec<&str> = config.host_tools.iter().map(|spec| spec.name.as_str()).collect();
    assert!(names.contains(&"vault__vault_code") && names.contains(&"remote_bash"), "{names:?}");
    assert!(config.mcp_servers.is_empty());
    let handler = config.host_tool_handler.clone().unwrap();
    let out = rig.runtime.block_on(handler.call(&config.session_id, "vault__vault_code", "{}")).unwrap();
    assert_eq!(out, "The vault code is 1234");

    // A harness that takes neither gets no tool config at all.
    rig.session(PLAIN_HARNESS);
    let config = rig.last_config();
    assert!(config.mcp_servers.is_empty() && config.host_tools.is_empty() && config.disabled_tools.is_empty());

    // Stop, start, restart and remove from the panel.
    rig.panel(tools::Message::Server("vault".into(), tools::ServerAction::Stop));
    rig.until("vault stops", WAIT, |app| {
        app.areas.tools.panel.catalog().servers.iter().any(|v| v.server.name == "vault" && v.server.status == tools::ServerStatus::Stopped)
    });
    assert!(!rig.config().mcp_servers["vault"].enabled, "the stop is saved");
    rig.panel(tools::Message::Server("vault".into(), tools::ServerAction::Start));
    rig.until("vault runs again", WAIT, |app| {
        app.areas.tools.panel.catalog().servers.iter().any(|v| v.server.name == "vault" && v.server.status == tools::ServerStatus::Running)
    });
    rig.panel(tools::Message::Server("vault".into(), tools::ServerAction::Restart));
    rig.until("vault runs after the restart", WAIT, |app| {
        app.areas.tools.panel.catalog().servers.iter().any(|v| v.server.name == "vault" && v.server.status == tools::ServerStatus::Running)
    });
    rig.panel(tools::Message::Server("vault".into(), tools::ServerAction::Remove));
    rig.until("vault is gone", WAIT, |app| !app.areas.tools.panel.catalog().servers.iter().any(|v| v.server.name == "vault"));
    assert!(!rig.config().mcp_servers.contains_key("vault"));

    // The app hosted server can't be removed.
    rig.panel(tools::Message::Server("remote".into(), tools::ServerAction::Remove));
    assert!(server(&rig, "remote").is_some());
}

#[test]
fn servers_import_from_claude_cursor_and_codex_configs() {
    let mut rig = Rig::new().started();
    let script = vault_script(rig.dir.path(), "77");
    let script = serde_json::to_string(script.to_str().unwrap()).unwrap();
    std::fs::write(
        rig.home.join(".claude.json"),
        format!(r#"{{"mcpServers": {{"claude-vault": {{"command": "{PYTHON}", "args": ["-u", {script}]}}}}}}"#),
    )
    .unwrap();
    std::fs::create_dir_all(rig.home.join(".cursor")).unwrap();
    std::fs::write(rig.home.join(".cursor/mcp.json"), r#"{"mcpServers": {"cursor-docs": {"url": "http://127.0.0.1:9/mcp"}}}"#).unwrap();
    std::fs::create_dir_all(rig.home.join(".codex")).unwrap();
    std::fs::write(
        rig.home.join(".codex/config.toml"),
        format!("[mcp_servers.codex-vault]\ncommand = \"{PYTHON}\"\nargs = [\"-u\", {script}, \"codex\"]\n"),
    )
    .unwrap();

    rig.panel(tools::Message::ImportScan);
    rig.until("the scan lists the candidates", WAIT, |app| app.areas.tools.panel.imports().is_some_and(|entries| entries.len() == 3));
    let entries = rig.app.areas.tools.panel.imports().unwrap().to_vec();
    let sources: Vec<_> = entries.iter().map(|entry| entry.candidate.source).collect();
    assert!(sources.contains(&tools::ImportSource::Claude) && sources.contains(&tools::ImportSource::Cursor) && sources.contains(&tools::ImportSource::Codex));
    // Import only the two stdio servers.
    let cursor = entries.iter().position(|entry| entry.candidate.source == tools::ImportSource::Cursor).unwrap();
    rig.panel(tools::Message::ImportSelect(cursor, false));
    rig.panel(tools::Message::ImportSubmit);
    rig.until("both are imported", WAIT, |app| {
        app.areas.tools.panel.imports().unwrap().iter().filter(|entry| entry.outcome == Some(tools::ImportOutcome::Imported)).count() == 2
    });
    let saved = rig.config().mcp_servers;
    assert!(saved.contains_key("claude-vault") && saved.contains_key("codex-vault") && !saved.contains_key("cursor-docs"), "{saved:?}");
    rig.until("the imported servers run", WAIT, |app| {
        ["claude-vault", "codex-vault"].iter().all(|name| {
            app.areas.tools.panel.catalog().servers.iter().any(|v| v.server.name == *name && v.server.status == tools::ServerStatus::Running)
        })
    });
}

#[test]
fn project_toggles_are_saved_and_switch_tools_off_in_new_sessions() {
    let mut rig = Rig::new().started();
    let command = vault_server(rig.dir.path(), "5");
    add_server(&mut rig, "vault", &command);
    let project = rig.project();
    assert_eq!(rig.app.areas.tools.panel.disabled_tools().map(|tools| tools.len()), Some(0), "the current project is selected");

    rig.panel(tools::Message::ToolToggled(tools::ToolId::builtin("bash"), false));
    rig.panel(tools::Message::ToolToggled(tools::ToolId::mcp("vault", "vault_code"), false));
    rig.panel(tools::Message::ToolToggled(tools::ToolId::mcp("remote", "remote_file"), false));
    let settings = rig.config().projects[&project].clone();
    assert!(settings.disabled_tools.contains("bash"));
    assert!(settings.disabled_mcp_tools["vault"].contains("vault_code"));

    rig.session(MCP_HARNESS);
    let config = rig.last_config();
    for name in ["bash", "mcp__vault__vault_code", "mcp__remote__remote_file"] {
        assert!(config.disabled_tools.iter().any(|tool| tool == name), "{name} in {:?}", config.disabled_tools);
    }
    rig.session(HOST_HARNESS);
    let config = rig.last_config();
    let names: Vec<&str> = config.host_tools.iter().map(|spec| spec.name.as_str()).collect();
    assert!(!names.contains(&"vault__vault_code") && !names.contains(&"remote_file") && names.contains(&"remote_bash"), "{names:?}");
    assert!(config.disabled_tools.iter().any(|tool| tool == "bash"));

    // On again.
    rig.panel(tools::Message::ToolToggled(tools::ToolId::mcp("vault", "vault_code"), true));
    assert!(!rig.config().projects[&project].disabled_mcp_tools.contains_key("vault"));
    // Another project keeps its own toggles.
    rig.panel(tools::Message::ProjectSelected(tools::ProjectRef { id: "other".into(), name: "other".into() }));
    assert_eq!(rig.app.areas.tools.panel.disabled_tools().map(|tools| tools.len()), Some(0));
}

#[test]
fn the_peers_section_shows_what_each_issued_grant_allows() {
    let mut rig = Rig::new().started();
    rig.panel(tools::Message::AddHostOpen);
    rig.panel(tools::Message::AddHostAlias("box".into()));
    rig.panel(tools::Message::AddHostHostname("10.0.0.5".into()));
    rig.panel(tools::Message::AddHostSubmit);
    let issuer = NodeKey::from_secret(&[9; 32]).unwrap();
    let driver = NodeKey::from_secret(&[21; 32]).unwrap();
    let viewer = NodeKey::from_secret(&[22; 32]).unwrap();
    let grants = Arc::clone(&rig.app.ctx.services.grants);
    let drive = Scope::new(Resource::Session { id: "s1".into() }, [loa_grants::Action::Drive]).unwrap();
    let exec = Scope::new(Resource::Ssh { host: "box".into() }, [loa_grants::Action::Exec]).unwrap();
    let view = Scope::new(Resource::Session { id: "s1".into() }, [loa_grants::Action::View]).unwrap();
    let spec = |scopes| GrantSpec { scopes, ttl_ms: 60_000, reshare: false, until_ms: None };
    let now = loa_core::now_ms();
    grants.issue(&issuer, driver.public(), spec(vec![drive, exec]), now).unwrap();
    grants.issue(&issuer, viewer.public(), spec(vec![view]), now).unwrap();

    rig.tools(Message::Open);
    rig.panel(tools::Message::SectionSelected(tools::Section::Peers));
    let matrix = rig.app.areas.tools.panel.peer_matrix();
    assert_eq!(matrix.peers.len(), 2);
    let column = |key: String| matrix.peers.iter().position(|peer| peer.key == key).unwrap();
    let (driver, viewer) = (column(driver.public().to_hex()), column(viewer.public().to_hex()));
    let row = |tool: tools::ToolId| matrix.rows.iter().find(|row| row.tool == tool).unwrap().allowed.clone();
    let bash = row(tools::ToolId::mcp("remote", "remote_bash"));
    let file = row(tools::ToolId::mcp("remote", "remote_file"));
    assert!(bash[driver] && !bash[viewer]);
    assert!(!file[driver], "exec doesn't give file reads");
    let read = row(tools::ToolId::mcp("omp", "read"));
    assert!(read[driver] && !read[viewer], "drive allows the other tools, view nothing");
}

#[test]
fn remote_hosts_import_add_limit_and_remove_through_the_central_config() {
    let mut rig = Rig::new().started();
    std::fs::write(
        rig.home.join(".ssh/config"),
        "Host bastion\n  HostName 10.0.0.1\n  User jump\nHost db\n  HostName db.internal\n  ProxyJump bastion\n",
    )
    .unwrap();
    rig.panel(tools::Message::SectionSelected(tools::Section::Hosts));
    rig.panel(tools::Message::ImportSshHosts);
    let hosts: Vec<String> = rig.app.areas.tools.panel.hosts().iter().map(|host| host.alias.clone()).collect();
    assert_eq!(hosts, ["bastion", "db"]);
    let db = rig.app.areas.tools.remote.service.hosts().get("db").expect("db resolves");
    assert_eq!(db.target.hostname, "db.internal");
    assert_eq!(db.jumps.len(), 1, "ProxyJump bastion");
    assert!(db.limits.read_only && !db.limits.commands, "imported hosts start closed");

    rig.panel(tools::Message::AddHostOpen);
    rig.panel(tools::Message::AddHostAlias("build".into()));
    rig.panel(tools::Message::AddHostHostname("build.example".into()));
    rig.panel(tools::Message::AddHostUser("ci".into()));
    rig.panel(tools::Message::AddHostPort("2222".into()));
    rig.panel(tools::Message::AddHostSubmit);
    let build = &rig.config().remote_hosts["build"];
    assert_eq!((build.host.as_str(), build.user.as_deref(), build.port), ("build.example", Some("ci"), Some(2222)));

    rig.panel(tools::Message::EditLimits("build".into()));
    rig.panel(tools::Message::LimitsReadOnly(false));
    rig.panel(tools::Message::LimitsCommands(true));
    rig.panel(tools::Message::LimitsNewPath("/srv".into()));
    rig.panel(tools::Message::LimitsAddPath);
    rig.panel(tools::Message::LimitsAutoApprove(true));
    rig.panel(tools::Message::LimitsSave);
    let build = &rig.config().remote_hosts["build"];
    assert!(!build.read_only && build.commands && build.auto_approve && build.allowed_paths == ["/srv"]);
    let limits = rig.app.areas.tools.remote.service.hosts().get("build").unwrap().limits;
    assert!(limits.commands && limits.auto_approve_commands && !limits.read_only);

    rig.panel(tools::Message::RemoveHost("bastion".into()));
    assert!(!rig.config().remote_hosts.contains_key("bastion"));
    assert!(rig.app.areas.tools.remote.service.hosts().get("bastion").is_none());

    // One palette command per host opens a shell on it.
    rig.update(AppMessage::Palette(loa_ui_palette::Message::Open));
    assert!(rig.app.palette.commands().iter().any(|command| command.id.as_str() == "tools.shell.build"));
}

/// Hosts the in-process SSH server as `box` with `limits`, the agent and
/// an empty `known_hosts`.
fn ssh_box(rig: &mut Rig, commands: bool) -> (ssh::FakeAgent, ssh::TestServer, PathBuf) {
    let (agent, server) = rig.runtime.block_on(async {
        let agent = ssh::FakeAgent::start(rig.dir.path()).await;
        let server = ssh::TestServer::start(&agent.public()).await;
        (agent, server)
    });
    let known_hosts = rig.dir.path().join("known_hosts");
    std::fs::write(&known_hosts, "").unwrap();
    rig.use_remote(loa_remote::RemoteServiceConfig {
        known_hosts: vec![known_hosts.clone()],
        known_hosts_write: known_hosts.clone(),
        connector: ConnectorConfig::with_agent(Some(agent.socket.clone())),
    });
    rig.panel(tools::Message::AddHostOpen);
    rig.panel(tools::Message::AddHostAlias("box".into()));
    rig.panel(tools::Message::AddHostHostname("127.0.0.1".into()));
    rig.panel(tools::Message::AddHostUser(ssh::USER.into()));
    rig.panel(tools::Message::AddHostPort(server.port().to_string()));
    rig.panel(tools::Message::AddHostSubmit);
    rig.panel(tools::Message::EditLimits("box".into()));
    rig.panel(tools::Message::LimitsReadOnly(false));
    rig.panel(tools::Message::LimitsCommands(commands));
    rig.panel(tools::Message::LimitsNewPath("/".into()));
    rig.panel(tools::Message::LimitsAddPath);
    rig.panel(tools::Message::LimitsSave);
    (agent, server, known_hosts)
}

/// The waiting remote card of `session`.
fn waiting_card(rig: &Rig, session: &SessionId) -> Option<loa_core::NodeId> {
    let dom = rig.dom(session);
    dom.iter()
        .find(|node| is_remote_decision(&dom, node.id) && node.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str()))
        .map(|node| node.id)
}

#[test]
fn remote_questions_raise_decision_cards_answered_in_the_chat_or_the_panel() {
    let mut rig = Rig::new().started();
    let (_agent, server, known_hosts) = ssh_box(&mut rig, true);
    let session = rig.session(HOST_HARNESS);
    rig.type_prompt("hello");
    // The harness calls the tool during its turn: keep the turn running.
    rig.until("the turn ends", WAIT, |app| app.ctx.sessions.get(session.as_str()).is_some_and(|open| open.has_turns() && !open.is_busy()));
    let turn = dom_turns(&rig.dom(&session)).last().unwrap().id;
    rig.app.ctx.sessions.get(session.as_str()).unwrap().writer().commit(loa_core::Draft::set(turn, attr::STATUS, TurnStatus::Running)).unwrap();
    rig.drain();
    let config = rig.last_config();
    let handler = config.host_tool_handler.clone().unwrap();
    let call = rig.runtime.spawn(async move {
        handler.call(&config.session_id, "remote_bash", r#"{"host": "box", "command": "echo from-box"}"#).await
    });

    // The command is approved before connecting: an approval card in the
    // chat, answered from the panel.
    rig.until("the approval shows", WAIT, |app| {
        app.areas.tools.panel.prompts().iter().any(|prompt| prompt.kind == tools::PromptKind::Approval)
    });
    let approval = waiting_card(&rig, &session).expect("an approval card in the session");
    assert_eq!(rig.app.ctx.shell.active_tab(), Some(TAB), "the Tools tab opens on the question");
    assert_eq!(rig.dom(&session).get(approval).unwrap().str_attr(attr::KIND), Some("approval"));
    let id = rig.app.areas.tools.panel.prompts()[0].id;
    rig.panel(tools::Message::AnswerPrompt(id, tools::PromptAnswer::AllowOnce));
    let node = rig.dom(&session).get(approval).cloned().unwrap();
    assert_eq!((node.str_attr(attr::STATUS), node.attr(attr::ANSWER).cloned()), (Some("answered"), Some(loa_core::Value::List(vec!["allow_once".into()]))));

    // Then the unknown host key: a card in the chat, answered there.
    rig.until("the host key card shows", WAIT, |app| {
        app.areas.tools.panel.prompts().iter().any(|prompt| prompt.kind == tools::PromptKind::HostKey)
    });
    let card = waiting_card(&rig, &session).expect("a host key card in the session");
    let node = rig.dom(&session).get(card).cloned().unwrap();
    assert_eq!(node.str_attr(attr::KIND), Some("question"));
    assert!(node.text.contains("presented"), "{}", node.text);
    rig.until("the card reaches the desk", WAIT, |app| app.areas.decisions.focused() == Some(card));
    let choose = desk::Message::Card(session.to_string(), loa_ui_decisions::Message::Choose(card, 0));
    rig.update(AppMessage::Decisions(decisions::Message::Desk(choose)));
    let node = rig.dom(&session).get(card).cloned().unwrap();
    assert_eq!(node.str_attr(attr::STATUS), Some("answered"));
    let out = rig.runtime.block_on(async { tokio::time::timeout(WAIT, call).await }).unwrap().unwrap().unwrap();
    assert!(out.contains("from-box"), "{out}");
    assert!(std::fs::read_to_string(&known_hosts).unwrap().contains(&format!("[127.0.0.1]:{}", server.port())), "the key is trusted");

    // The audit view lists the command with its session.
    rig.panel(tools::Message::ShowAudit("box".into()));
    rig.until("the audit rows load", WAIT, |app| app.areas.tools.panel.audit().is_some_and(|(_, rows)| !rows.is_empty()));
    let (_, rows) = rig.app.areas.tools.panel.audit().unwrap();
    let row = rows.iter().find(|row| row.action == tools::AuditAction::Command).unwrap();
    assert_eq!((row.session.as_str(), row.detail.as_str()), (session.as_str(), "echo from-box"));
    assert!(row.outcome.starts_with("ok"), "{}", row.outcome);

    // A refused question: denied from the chat.
    let config = rig.last_config();
    let handler = config.host_tool_handler.clone().unwrap();
    let call = rig.runtime.spawn(async move {
        handler.call(&config.session_id, "remote_bash", r#"{"host": "box", "command": "rm -rf /tmp/x"}"#).await
    });
    rig.until("the second approval shows", WAIT, |app| !app.areas.tools.panel.prompts().is_empty());
    let card = waiting_card(&rig, &session).unwrap();
    rig.until("the card reaches the desk", WAIT, |app| app.areas.decisions.focused() == Some(card));
    rig.update(AppMessage::Decisions(decisions::State::dismiss()));
    let denied = rig.runtime.block_on(async { tokio::time::timeout(WAIT, call).await }).unwrap().unwrap();
    assert!(denied.is_err(), "{denied:?}");
    assert_eq!(rig.dom(&session).get(card).unwrap().str_attr(attr::STATUS), Some("answered"));

    // A question whose call is cancelled closes its card.
    let config = rig.last_config();
    let handler = config.host_tool_handler.clone().unwrap();
    let call = rig.runtime.spawn(async move { handler.call(&config.session_id, "remote_bash", r#"{"host": "box", "command": "echo x"}"#).await });
    rig.until("the third approval shows", WAIT, |app| !app.areas.tools.panel.prompts().is_empty());
    let card = waiting_card(&rig, &session).unwrap();
    call.abort();
    rig.until("the card is cancelled", WAIT, |app| app.areas.tools.panel.prompts().is_empty());
    assert_eq!(rig.dom(&session).get(card).unwrap().str_attr(attr::STATUS), Some("cancelled"));
    assert_eq!(rig.app.ctx.notice(), None, "no stale \"question waiting\" notice once none waits");
}

#[test]
fn a_remote_shell_opens_on_a_host_in_the_remote_shell_tab() {
    let mut rig = Rig::new().started();
    let (_agent, server, _) = ssh_box(&mut rig, false);
    std::fs::write(rig.dir.path().join("known_hosts"), server.known_hosts_line()).unwrap();
    rig.panel(tools::Message::OpenTerminal("box".into()));
    assert_eq!(rig.app.ctx.shell.active_tab(), Some(SHELL_TAB));
    rig.until("the shell runs", WAIT, |app| {
        let terminal = &app.areas.tools.terminal;
        terminal.active().and_then(|tab| terminal.status(tab)) == Some(&terminal::Status::Running)
    });
    let terminal = &rig.app.areas.tools.terminal;
    assert_eq!(terminal.target(terminal.active().unwrap()), Some(&terminal::Target::Remote { host: "box".into() }));
    let records = rig.app.areas.tools.remote.service.audit().query(&loa_remote::audit::AuditQuery { host: Some("box".into()), limit: 10, ..Default::default() }).unwrap();
    assert!(records.iter().any(|record| record.action == loa_remote::audit::Action::Terminal));
}

// ---------------------------------------------------------------- live

fn live_model() -> String {
    std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".into())
}

/// omp as the app registers it (approval mode always-ask), pinned to the
/// live model.
struct LiveOmp(loa_adapter_omp::OmpAdapter);

#[async_trait::async_trait]
impl Adapter for LiveOmp {
    fn harness(&self) -> &'static str {
        self.0.harness()
    }

    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }

    async fn start(&self, mut config: SessionConfig, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        config.model = Some(live_model());
        self.0.start(config, sink).await
    }

    async fn resume(&self, mut config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        config.model = Some(live_model());
        self.0.resume(config, dom, sink).await
    }
}

fn live_rig() -> Rig {
    Rig::with(|harnesses| {
        let data = tempfile::tempdir().unwrap().keep();
        let mut omp = loa_adapter_omp::OmpAdapterConfig::new("live-node", data.join("omp"));
        omp.approval_mode = Some("always-ask".into());
        harnesses.add(Arc::new(LiveOmp(loa_adapter_omp::OmpAdapter::new(omp))), "omp", true, None, None);
    })
    .started()
}

/// Runs the active session's turn to its end, answering every card it
/// raises with its first choice (allow once, trust).
fn finish_turn(rig: &mut Rig, session: &SessionId) -> Dom {
    let deadline = Instant::now() + TURN_TIMEOUT;
    let mut answered = Vec::new();
    loop {
        rig.drain();
        if let Some(focused) = rig.app.areas.decisions.focused()
            && !answered.contains(&focused)
        {
            answered.push(focused);
            let choose = desk::Message::Card(session.to_string(), loa_ui_decisions::Message::Choose(focused, 0));
            rig.update(AppMessage::Decisions(decisions::Message::Desk(choose)));
        }
        let dom = rig.dom(session);
        let ended = dom_turns(&dom)
            .last()
            .and_then(|turn| turn.str_attr(attr::STATUS))
            .and_then(TurnStatus::parse)
            .is_some_and(|status| !matches!(status, TurnStatus::Running | TurnStatus::Waiting));
        if ended {
            return dom;
        }
        assert!(Instant::now() < deadline, "the turn didn't end in time");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn call_result(dom: &Dom, tool: &str) -> String {
    let call = dom
        .iter()
        .find(|node| node.kind == ElementKind::Call && node.str_attr(attr::TOOL) == Some(tool))
        .unwrap_or_else(|| panic!("the agent called {tool}"));
    dom.children_of_kind(call.id, ElementKind::Result).next().expect("the call has a result").text.clone()
}

#[test]
#[ignore = "runs the installed omp with real model calls"]
fn live_an_omp_session_calls_a_tool_of_a_server_registered_in_the_panel() {
    let mut rig = live_rig();
    let command = vault_server(rig.dir.path(), "LOA-8841");
    add_server(&mut rig, "vault", &command);
    let session = rig.session("omp");
    rig.type_prompt("What is the vault code? Use your vault__vault_code tool to find out, then tell me the code.");
    let dom = finish_turn(&mut rig, &session);
    let result = call_result(&dom, "vault__vault_code");
    assert!(result.contains("LOA-8841"), "{result}");
    if let Some(harness) = rig.app.ctx.sessions.get(session.as_str()).and_then(|s| s.harness().cloned()) {
        rig.runtime.block_on(harness.shutdown()).unwrap();
    }
}

#[test]
#[ignore = "runs the installed omp with real model calls"]
fn live_an_omp_session_runs_remote_bash_on_a_remote_host_through_cards() {
    let mut rig = live_rig();
    // No sshd or agent identity on this machine: the in-process SSH server
    // plays the host, with its test agent.
    let (_agent, server, known_hosts) = ssh_box(&mut rig, true);
    let session = rig.session("omp");
    rig.type_prompt("Use your remote_bash tool with host \"box\" to run the command `echo loa-remote-5521` and tell me what it printed.");
    let dom = finish_turn(&mut rig, &session);
    let result = call_result(&dom, "remote_bash");
    assert!(result.contains("loa-remote-5521"), "{result}");
    assert!(server.stats.commands.lock().unwrap().iter().any(|command| command == "echo loa-remote-5521"));
    let cards: Vec<_> = dom.iter().filter(|node| is_remote_decision(&dom, node.id)).map(|node| node.str_attr(attr::KIND).unwrap_or("").to_owned()).collect();
    assert_eq!(cards, ["approval", "question"], "an approval card, then (connecting) a host key card");
    assert!(std::fs::read_to_string(known_hosts).unwrap().contains("127.0.0.1"));
    if let Some(harness) = rig.app.ctx.sessions.get(session.as_str()).and_then(|s| s.harness().cloned()) {
        rig.runtime.block_on(harness.shutdown()).unwrap();
    }
}
