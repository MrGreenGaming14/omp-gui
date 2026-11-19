//! The sandbox area driven through `App::update`: a scripted harness with
//! host tools calls `sandbox_task`, which runs the real stub with the local
//! provider in an isolated git worktree; the run, its patch and applying it
//! go through the area like in the running app.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use loa_adapter::{Adapter, AdapterError, Answer, Capabilities, HarnessSession, HostTools, Prompt, Registry, SessionConfig, SessionState};
use loa_core::{Dom, Draft, ElementKind, NodeId, PatchSink, Role, TurnStatus, Value, now_ms};
use loa_identity::NodeIdentity;
use loa_sandbox::provider::local::find_stub;
use loa_ui_palette as palette;
use loa_ui_tools::DetailState;

use super::{Field, Message, TAB, View};
use crate::app::tests::{prompt, update};
use crate::app::{App, Message as AppMessage};
use crate::services::{Dirs, Harnesses, Node, Services};

/// A harness with host tools. Sessions offered `sandbox_task` call it with
/// the prompt as the task; worker sessions write two files through the
/// sandbox tools.
struct Scripted {
    /// The host tools of every parent session started.
    parent_tools: Arc<Mutex<Vec<Vec<String>>>>,
    /// Run on this peer instead of locally.
    peer: Peer,
}

/// The peer a scripted parent session runs its task on, if any.
type Peer = Arc<Mutex<Option<String>>>;

#[async_trait::async_trait]
impl Adapter for Scripted {
    fn harness(&self) -> &'static str {
        "scripted"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { host_tools: true, ..Capabilities::default() }
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        let names: Vec<String> = config.host_tools.iter().map(|tool| tool.name.clone()).collect();
        let worker = !names.iter().any(|name| name == "sandbox_task");
        if !worker {
            self.parent_tools.lock().unwrap().push(names);
        }
        let attrs: [(&str, Value); 4] = [
            ("id", config.session_id.clone().into()),
            ("node", config.node.into()),
            ("harness", "scripted".into()),
            ("created", Value::Int(now_ms())),
        ];
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?.node;
        let tools = config.host_tool_handler.ok_or_else(|| AdapterError::Format("no host tools".into()))?;
        Ok(Box::new(ScriptedSession { sink, root, tools, id: config.session_id, worker, peer: Arc::clone(&self.peer) }))
    }

    async fn resume(&self, _: SessionConfig, _: &Dom, _: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        Err(AdapterError::Unsupported { harness: "scripted", what: "resume" })
    }
}

struct ScriptedSession {
    sink: Arc<dyn PatchSink>,
    root: NodeId,
    tools: Arc<dyn HostTools>,
    id: String,
    worker: bool,
    peer: Peer,
}

impl ScriptedSession {
    async fn work(&self, task: &str) -> Result<String, String> {
        if self.worker {
            self.tools.call(&self.id, "sandbox_write", r#"{"path":"notes/hello.txt","content":"from the sandbox\n"}"#).await?;
            self.tools.call(&self.id, "sandbox_bash", r#"{"command":"printf 'changed\\n' > tracked.txt"}"#).await?;
            Ok("Wrote the notes".into())
        } else {
            let peer = self.peer.lock().unwrap().clone();
            let args = match peer {
                Some(peer) => serde_json::json!({ "task": task, "kind": "peer", "isolation": true, "options": { "peer": peer } }),
                None => serde_json::json!({ "task": task, "kind": "local", "isolation": true }),
            }
            .to_string();
            self.tools.call(&self.id, "sandbox_task", &args).await
        }
    }
}

#[async_trait::async_trait]
impl HarnessSession for ScriptedSession {
    async fn send(&self, prompt: Prompt) -> loa_adapter::Result<()> {
        let turn = self.sink.commit(Draft::insert(self.root, ElementKind::Turn, [("status", Value::from(TurnStatus::Running))], ""))?.node;
        self.sink.commit(Draft::insert(turn, ElementKind::Message, [("role", Value::from(Role::User))], prompt.text.as_str()))?;
        let reply = self.work(&prompt.text).await.unwrap_or_else(|error| format!("failed: {error}"));
        self.sink.commit(Draft::insert(turn, ElementKind::Message, [("role", Value::from(Role::Assistant))], reply.as_str()))?;
        self.sink.commit(Draft::set(turn, "status", TurnStatus::Done))?;
        Ok(())
    }

    async fn interrupt(&self) -> loa_adapter::Result<()> {
        Ok(())
    }

    async fn answer(&self, decision: NodeId, _: Answer) -> loa_adapter::Result<()> {
        Err(AdapterError::UnknownDecision(decision))
    }

    fn harness(&self) -> &'static str {
        "scripted"
    }

    fn state(&self) -> SessionState {
        SessionState::Idle
    }

    async fn wait_idle(&self) -> loa_adapter::Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> loa_adapter::Result<()> {
        Ok(())
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git").args(args).current_dir(dir).status().unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A git repository with one committed file, `tracked.txt`.
pub(super) fn repository(dir: &Path) -> std::path::PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("tracked.txt"), "original\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["-c", "user.name=Test", "-c", "user.email=t@example.org", "commit", "-qm", "baseline"]);
    repo
}

/// The app on the scripted harness, working in a fresh repository.
pub(super) fn scripted_app(dir: &Path) -> (App, Arc<Mutex<Vec<Vec<String>>>>) {
    let (app, parent_tools, _) = scripted_app_as(dir, 5, "testhost");
    (app, parent_tools)
}

/// [`scripted_app`] on its own node key; the returned cell picks the peer
/// its sessions run their tasks on.
fn scripted_app_as(dir: &Path, seed: u8, name: &str) -> (App, Arc<Mutex<Vec<Vec<String>>>>, Peer) {
    assert!(find_stub().is_some(), "loa-sandbox-stub is built (cargo build -p loa-sandbox-stub)");
    let repo = repository(dir);
    let parent_tools = Arc::default();
    let peer = Peer::default();
    let mut harnesses = Harnesses { registry: Registry::new(), list: Vec::new() };
    harnesses.add(Arc::new(Scripted { parent_tools: Arc::clone(&parent_tools), peer: Arc::clone(&peer) }), "Scripted", true, None, None);
    let node = Node::new(NodeIdentity::from_secret_bytes(&[seed; 32]), name.into());
    let services = Services::new(node, Dirs::new(dir.join("loa")), harnesses, repo).unwrap();
    (App::build(services).0, parent_tools, peer)
}

fn sandbox(app: &mut App, message: Message) {
    update(app, AppMessage::Sandbox(message));
}

#[test]
fn a_session_runs_sandbox_task_and_the_patch_is_reviewed_and_applied() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, parent_tools) = scripted_app(dir.path());
    let repo = app.ctx.services.cwd.clone();
    let mut events = app.areas.sandbox.subscribe();

    update(&mut app, AppMessage::NewSession);
    prompt(&mut app, "Write the notes");
    let offered = parent_tools.lock().unwrap().clone();
    assert!(offered.len() == 1 && offered[0].iter().any(|name| name == "sandbox_task"), "the session got the tool: {offered:?}");
    let parent = app.ctx.sessions.active_id().unwrap().to_string();
    // The run feed, as the area's subscription delivers it.
    while let Ok(event) = events.try_recv() {
        sandbox(&mut app, Message::Event(event));
    }

    let entries = app.areas.sandbox.runs().entries();
    assert_eq!(entries.len(), 1);
    let entry = entries[0].clone();
    assert_eq!((entry.run.parent.as_str(), entry.run.task.as_str(), entry.run.isolated), (parent.as_str(), "Write the notes", true));
    let loa_sandbox::RunState::Finished { final_message, files, applied: false, .. } = &entry.state else {
        panic!("the run finished: {:?}", entry.state)
    };
    assert_eq!(final_message.as_deref(), Some("Wrote the notes"));
    let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["notes/hello.txt", "tracked.txt"]);
    assert!(!repo.join("notes").exists(), "the workspace is untouched until the patch is applied");

    // The patch comes up for review in the Sandbox tab.
    assert_eq!(app.ctx.shell.active_tab(), Some(TAB));
    assert_eq!(app.areas.sandbox.selected, Some(entry.run.run));
    let shown: Vec<_> = app.areas.sandbox.diff.files().iter().map(|file| file.path().to_owned()).collect();
    assert_eq!(shown, ["notes/hello.txt", "tracked.txt"]);

    // The worker was journaled in the library and released when it ended.
    let worker = entry.run.worker.clone();
    assert!(app.ctx.services.library.live(&worker).is_none(), "the worker journal is closed");
    assert!(app.ctx.services.library.load(&worker).unwrap().root().is_some(), "the worker session is journaled");

    sandbox(&mut app, Message::Apply(entry.run.run));
    assert_eq!(std::fs::read_to_string(repo.join("notes/hello.txt")).unwrap(), "from the sandbox\n");
    assert_eq!(std::fs::read_to_string(repo.join("tracked.txt")).unwrap(), "changed\n");
    let applied = app.areas.sandbox.runs().get(entry.run.run).unwrap();
    assert!(matches!(applied.state, loa_sandbox::RunState::Finished { applied: true, .. }));
    assert_eq!(applied.pending_patch(), None);

    // Its worker session opens like any other.
    sandbox(&mut app, Message::OpenWorker(worker.clone()));
    assert_eq!(app.ctx.sessions.active_id().map(|id| id.to_string()), Some(worker));
}

#[test]
fn a_failed_run_is_listed_with_its_error() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = scripted_app(dir.path());
    // Not a git repository: isolation can't make a checkout.
    let plain = dir.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    app.ctx.services.cwd = plain;
    let mut events = app.areas.sandbox.subscribe();
    update(&mut app, AppMessage::NewSession);
    prompt(&mut app, "Write the notes");
    while let Ok(event) = events.try_recv() {
        sandbox(&mut app, Message::Event(event));
    }
    let entry = &app.areas.sandbox.runs().entries()[0];
    let loa_sandbox::RunState::Failed(error) = &entry.state else { panic!("{:?}", entry.state) };
    assert!(error.starts_with("isolation:"), "{error}");
    assert_ne!(app.ctx.shell.active_tab(), Some(TAB), "nothing to review");
}

#[test]
fn the_tools_panel_lists_each_provider_with_its_availability() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = scripted_app(dir.path());
    let tool = app.ctx.app_tools.iter().find(|tool| tool.name == "sandbox_task").expect("listed from the start");
    assert!(tool.details.is_empty(), "no availability before the probe");

    sandbox(&mut app, Message::Refresh);
    let tool = app.ctx.app_tools.iter().find(|tool| tool.name == "sandbox_task").unwrap();
    let labels: Vec<_> = tool.details.iter().map(|detail| detail.label.as_str()).collect();
    assert_eq!(labels, ["Local", "Docker", "Firecracker", "VM (qemu)", "SSH host", "Peer"]);
    assert_eq!(tool.details[0].state, DetailState::Ready, "the stub is here: {:?}", tool.details[0]);
    let peer = &tool.details[5];
    assert_eq!((peer.state, peer.text.as_str()), (DetailState::Unavailable, "Peer networking is not running"), "the node isn't started");
    assert!(tool.details.iter().all(|detail| !detail.text.is_empty()), "every state says why");
}

#[test]
fn settings_are_saved_and_used_for_new_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = scripted_app(dir.path());
    for message in [palette::Message::Open, palette::Message::QueryChanged("Sandbox settings".into()), palette::Message::Submit] {
        update(&mut app, AppMessage::Palette(message));
    }
    assert_eq!((app.ctx.shell.active_tab(), app.areas.sandbox.view), (Some(TAB), View::Settings));

    sandbox(&mut app, Message::Edit(Field::DockerImage, "alpine:3".into()));
    sandbox(&mut app, Message::Edit(Field::VmDisk, "/images/guest.img".into()));
    sandbox(&mut app, Message::Edit(Field::SshHosts, "build, gpu".into()));
    sandbox(&mut app, Message::Network(true));
    sandbox(&mut app, Message::Save);

    let saved = loa_sandbox::SandboxConfig::load(&app.ctx.services.dirs.data.join("sandbox.json")).unwrap();
    assert_eq!(saved.docker_image, "alpine:3");
    assert_eq!(saved.vm_disk.as_deref(), Some(Path::new("/images/guest.img")));
    assert_eq!(saved.ssh_hosts, ["build", "gpu"]);
    assert!(saved.docker_network);
    assert_eq!(app.areas.sandbox.service.config(), &saved, "new sessions use the saved settings");
    let ssh = app.ctx.app_tools.iter().find(|tool| tool.name == "sandbox_task").unwrap().details.iter().find(|d| d.label == "SSH host").cloned();
    assert!(ssh.is_some_and(|ssh| ssh.state != DetailState::Ready || ssh.text.starts_with("build, gpu")), "probed again with the new hosts");

    // A restart reads them back.
    let services = Services::new(
        Node::new(NodeIdentity::from_secret_bytes(&[5; 32]), "testhost".into()),
        Dirs::new(dir.path().join("loa")),
        Harnesses { registry: Registry::new(), list: Vec::new() },
        dir.path().to_owned(),
    )
    .unwrap();
    drop(app);
    let app = App::build(services).0;
    assert_eq!(app.areas.sandbox.service.config(), &saved);
}

/// Peer runs: two apps in one process, each with its own node listening on
/// loopback, driven through `App::update` on a shared runtime.
mod peer {
    use std::time::{Duration, Instant};

    use iced::Task;
    use iced::futures::StreamExt;
    use iced_runtime::Action as RuntimeAction;
    use loa_node::Endpoint;
    use loa_ui_peers as panel;
    use tokio::runtime::Runtime;

    use super::*;
    use crate::areas::peers;

    fn run(rt: &Runtime, app: &mut App, task: Task<AppMessage>) {
        let mut pending = vec![task];
        while let Some(task) = pending.pop() {
            let Some(stream) = iced_runtime::task::into_stream(task) else { continue };
            let outputs = rt.block_on(async {
                let outputs = stream.filter_map(|action| async move {
                    match action {
                        RuntimeAction::Output(message) => Some(message),
                        _ => None,
                    }
                });
                tokio::time::timeout(Duration::from_secs(30), outputs.collect::<Vec<_>>()).await.expect("task finished")
            });
            for message in outputs {
                pending.push(app.update(message));
            }
        }
    }

    fn update(rt: &Runtime, app: &mut App, message: AppMessage) {
        let task = app.update(message);
        run(rt, app, task);
    }

    fn panel(rt: &Runtime, app: &mut App, message: panel::Message) {
        update(rt, app, AppMessage::Peers(peers::Message::Panel(message)));
    }

    /// The scripted app with its node on a free loopback port, started; the
    /// UDP port it listens on.
    fn networked(rt: &Runtime, dir: &Path, seed: u8, name: &str) -> (App, Peer, u16) {
        let (mut app, _, peer) = scripted_app_as(dir, seed, name);
        let mut settings = app.ctx.services.config.load().unwrap().config;
        settings.network.discovery.mdns = false;
        settings.network.discovery.tailscale = false;
        settings.network.port = 0;
        let config = peers::node_config(&app.ctx, &settings.network);
        app.areas.peers = peers::State::with_config(&mut app.ctx, config).0;
        let node = app.areas.peers.node().expect("node opened").clone();
        let started = rt.block_on(node.start()).map_err(|error| error.to_string());
        update(rt, &mut app, AppMessage::Peers(peers::Message::Started(started)));
        let Endpoint::Listening { udp, .. } = node.status().endpoint else { panic!("not listening: {:?}", node.status().endpoint) };
        (app, peer, udp[0].port())
    }

    fn add_peer(rt: &Runtime, app: &mut App, other: &App, port: u16) {
        panel(rt, app, panel::Message::KeyChanged(other.ctx.services.node.key.clone()));
        panel(rt, app, panel::Message::AddressChanged(format!("127.0.0.1:{port}")));
        panel(rt, app, panel::Message::AddPeer);
    }

    fn peer_detail(app: &App) -> loa_ui_tools::ToolDetail {
        let tool = app.ctx.app_tools.iter().find(|tool| tool.name == "sandbox_task").unwrap();
        tool.details.iter().find(|detail| detail.label == "Peer").unwrap().clone()
    }

    #[test]
    fn a_session_runs_sandbox_task_on_a_peer_that_granted_this_device() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
        let _runtime = rt.enter();
        let (owner_dir, guest_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (mut owner, _, _) = networked(&rt, owner_dir.path(), 21, "owner-box");
        let (mut guest, guest_peer, guest_port) = networked(&rt, guest_dir.path(), 22, "guest-box");
        let owner_port = match owner.areas.peers.node().unwrap().status().endpoint {
            Endpoint::Listening { udp, .. } => udp[0].port(),
            other => panic!("{other:?}"),
        };
        add_peer(&rt, &mut guest, &owner, owner_port);
        add_peer(&rt, &mut owner, &guest, guest_port);

        // Networking runs, but nobody granted the guest a sandbox yet.
        update(&rt, &mut guest, AppMessage::Sandbox(Message::Refresh));
        let detail = peer_detail(&guest);
        assert_eq!((detail.state, detail.text.as_str()), (DetailState::NotConfigured, "No peer has granted this device sandbox access"));

        // The owner grants "Sandbox on this node" from the Peers panel.
        let owner_key = owner.ctx.services.node.key.clone();
        update(&rt, &mut owner, AppMessage::Peers(peers::Message::Open));
        panel(&rt, &mut owner, panel::Message::SelectPeer(guest.ctx.services.node.key.clone()));
        panel(&rt, &mut owner, panel::Message::SelectResource("sandbox".into()));
        panel(&rt, &mut owner, panel::Message::SelectPermission(panel::Permission::Drive));
        panel(&rt, &mut owner, panel::Message::TtlChanged("2".into()));
        panel(&rt, &mut owner, panel::Message::SubmitGrant);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            update(&rt, &mut guest, AppMessage::Sandbox(Message::Refresh));
            if peer_detail(&guest).state == DetailState::Ready {
                break;
            }
            assert!(Instant::now() < deadline, "the grant reached the guest: {:?} {:?}", peer_detail(&guest), owner.ctx.notice());
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(peer_detail(&guest).text, "owner-box", "named as the user knows it");

        // A session on the guest runs its task on the owner, isolated.
        *guest_peer.lock().unwrap() = Some(owner_key);
        let repo = guest.ctx.services.cwd.clone();
        let mut events = guest.areas.sandbox.subscribe();
        update(&rt, &mut guest, AppMessage::NewSession);
        for c in "Write the notes".chars() {
            use iced::widget::text_editor::{Action, Edit};
            update(&rt, &mut guest, AppMessage::Composer(loa_ui_composer::Message::Edit(Action::Edit(Edit::Insert(c)))));
        }
        update(&rt, &mut guest, AppMessage::Composer(loa_ui_composer::Message::Submit));
        let deadline = Instant::now() + Duration::from_secs(30);
        while guest.areas.sandbox.runs().entries().first().is_none_or(|entry| matches!(entry.state, loa_sandbox::RunState::Running)) {
            while let Ok(event) = events.try_recv() {
                update(&rt, &mut guest, AppMessage::Sandbox(Message::Event(event)));
            }
            assert!(Instant::now() < deadline, "the peer run ended");
            std::thread::sleep(Duration::from_millis(25));
        }
        let entry = guest.areas.sandbox.runs().entries()[0].clone();
        assert_eq!((entry.run.kind, entry.run.isolated), (loa_sandbox::SandboxKind::Peer, true));
        let loa_sandbox::RunState::Finished { files, .. } = &entry.state else { panic!("the run finished: {:?}", entry.state) };
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["notes/hello.txt", "tracked.txt"]);
        assert!(!repo.join("notes").exists(), "the workspace is untouched until the patch is applied");
        assert_eq!(guest.ctx.shell.active_tab(), Some(TAB));
        update(&rt, &mut guest, AppMessage::Sandbox(Message::Apply(entry.run.run)));
        assert_eq!(std::fs::read_to_string(repo.join("notes/hello.txt")).unwrap(), "from the sandbox\n");
        assert!(owner.ctx.notice().is_none_or(|notice| !notice.contains("sandbox run")), "{:?}", owner.ctx.notice());
    }
}
