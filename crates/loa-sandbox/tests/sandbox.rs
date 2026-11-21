//! The stub protocol, the worker's host boundary and `sandbox_task` as the app
//! wires it, against the real stub binary and a scripted harness standing in
//! for the model.

use async_trait::async_trait;
use loa_contracts::harness::{Adapter, AdapterError, Answer, Capabilities, HarnessSession, HostTools, Prompt, Result, SessionConfig, SessionState};
use loa_core::{Dom, Draft, ElementKind, NodeId, PatchSink, SessionMode, Value, attr};
use loa_sandbox::client::{ExecEvent, StubClient};
use loa_sandbox::isolation::{apply_patch, patch_files};
use loa_sandbox::proto::{Command, ExecRequest, FileKind};
use loa_sandbox::provider::local::LocalProvider;
use loa_sandbox::provider::{Sandbox, SandboxProvider, SandboxSpec};
use loa_sandbox::tool::{SandboxTask, SandboxTaskArgs};
use loa_sandbox::worker::MemoryJournal;
use loa_sandbox::{SandboxConfig, SandboxEvent, SandboxKind, SandboxService, SandboxTaskResult};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::Notify;

/// The stub binary, built on first use: it lives in its own package, so cargo
/// gives this one no `CARGO_BIN_EXE_` for it.
static STUB: LazyLock<PathBuf> = LazyLock::new(|| {
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "--quiet", "-p", "loa-sandbox-stub"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .unwrap();
    assert!(status.success(), "building the stub");
    // Tests run from <target>/<profile>/deps.
    let exe = std::env::current_exe().unwrap();
    exe.parent().and_then(Path::parent).unwrap().join("loa-sandbox-stub")
});

fn stub() -> PathBuf { STUB.clone() }

fn service() -> SandboxService {
    SandboxService::new(SandboxConfig::default(), Arc::new(MemoryJournal::default())).with_stub(Some(stub()))
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git").args(args).current_dir(dir).status().unwrap();
    assert!(status.success(), "git {args:?}");
}

fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("tracked.txt"), "original\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["-c", "user.name=Test", "-c", "user.email=t@example.org", "commit", "-qm", "baseline"]);
    dir
}

/// What a scripted session does with its sandbox tools: `(tool, arguments)`.
type Calls = &'static [(&'static str, &'static str)];

/// A harness whose "model" makes fixed tool calls when prompted, or, when
/// `asks`, first raises an approval and makes them once it is answered.
#[derive(Default)]
struct Scripted {
    calls: Calls,
    asks: bool,
    check: Option<fn(&SessionConfig)>,
    observed: Arc<Mutex<Vec<String>>>,
    answers: Arc<Mutex<Vec<Answer>>>,
}

struct Session {
    tools: Arc<dyn HostTools>,
    id: String,
    sink: Arc<dyn PatchSink>,
    calls: Calls,
    asks: bool,
    answered: Notify,
    observed: Arc<Mutex<Vec<String>>>,
    answers: Arc<Mutex<Vec<Answer>>>,
}

impl Session {
    async fn call(&self) -> Result<()> {
        for (tool, args) in self.calls {
            let output = self.tools.call(&self.id, tool, args).await.map_err(AdapterError::Protocol)?;
            self.observed.lock().unwrap().push(output);
        }
        Ok(())
    }
}

#[async_trait]
impl Adapter for Scripted {
    fn harness(&self) -> &'static str { "scripted" }
    fn capabilities(&self) -> Capabilities { Capabilities::default() }
    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        assert!(config.host_tools.iter().all(|tool| tool.name.starts_with("sandbox_") && tool.name != "sandbox_task"));
        if let Some(check) = self.check {
            check(&config);
        }
        let tools = config.host_tool_handler.ok_or_else(|| AdapterError::Format("worker has no sandbox tools".into()))?;
        Ok(Box::new(Session {
            tools, id: config.session_id, sink, calls: self.calls, asks: self.asks, answered: Notify::new(),
            observed: Arc::clone(&self.observed), answers: Arc::clone(&self.answers),
        }))
    }
    async fn resume(&self, _: SessionConfig, _: &Dom, _: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        Err(AdapterError::Format("unexpected resume".into()))
    }
}

#[async_trait]
impl HarnessSession for Session {
    async fn send(&self, _: Prompt) -> Result<()> {
        if !self.asks {
            return self.call().await;
        }
        let session_attrs = [
            (attr::ID, Value::Str(self.id.clone())),
            (attr::HARNESS, Value::Str("scripted".into())),
            (attr::NODE, Value::Str("host".into())),
            (attr::CREATED, Value::Int(0)),
        ];
        let session = self.sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, session_attrs, "")).unwrap();
        let attrs = [(attr::KIND, Value::Str("approval".into())), (attr::STATUS, Value::Str("waiting".into()))];
        self.sink.commit(Draft::insert(session.node, ElementKind::Decision, attrs, "Allow tool: sandbox_write")).unwrap();
        Ok(())
    }
    async fn interrupt(&self) -> Result<()> { Ok(()) }
    async fn answer(&self, _: NodeId, answer: Answer) -> Result<()> {
        self.answers.lock().unwrap().push(answer);
        self.answered.notify_one();
        Ok(())
    }
    async fn set_mode(&self, _: SessionMode) -> Result<()> { Ok(()) }
    fn harness(&self) -> &'static str { "scripted" }
    fn state(&self) -> SessionState { SessionState::Idle }
    async fn wait_idle(&self) -> Result<()> {
        if !self.asks {
            return Ok(());
        }
        self.answered.notified().await;
        self.call().await
    }
    async fn shutdown(&self) -> Result<()> { Ok(()) }
}

async fn connect(root: &Path) -> (StubClient, Box<dyn Sandbox>) {
    let spec = SandboxSpec { name: "protocol-test".into(), workspace: root.into(), env: Vec::new(), options: Default::default() };
    let mut sandbox = LocalProvider::new(stub()).start(&spec).await.unwrap();
    let client = StubClient::connect(sandbox.take_channel().unwrap()).await.unwrap();
    (client, sandbox)
}

#[tokio::test]
async fn the_stub_streams_output_and_reports_the_exit_status() {
    let root = tempfile::tempdir().unwrap();
    let (client, sandbox) = connect(root.path()).await;
    let script = "printf out; printf err >&2; exit 7";
    let mut command = client.spawn(ExecRequest::new(Command::Argv(vec!["/bin/sh".into(), "-c".into(), script.into()]))).await.unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let status = loop {
        match command.next().await.unwrap().unwrap() {
            ExecEvent::Stdout(bytes) => stdout.extend(bytes),
            ExecEvent::Stderr(bytes) => stderr.extend(bytes),
            ExecEvent::Exit(status) => break status,
        }
    };
    assert_eq!((stdout.as_slice(), stderr.as_slice(), status.code), (&b"out"[..], &b"err"[..], Some(7)));

    client.write_file("nested/data.txt", b"abc".to_vec(), Some(0o600)).await.unwrap();
    assert_eq!(client.read_file("nested/data.txt", None).await.unwrap(), b"abc");
    let entries = client.list_dir("nested").await.unwrap();
    assert_eq!(entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(), ["data.txt"]);
    assert_eq!(client.stat("nested/data.txt").await.unwrap().unwrap().kind, FileKind::File);
    assert!(client.stat("nested/missing").await.unwrap().is_none());

    let output = client.exec(ExecRequest { timeout_ms: Some(30), ..ExecRequest::new(Command::Shell("while :; do :; done".into())) }).await.unwrap();
    assert!(output.status.timed_out && !output.status.success());
    client.shutdown().await.unwrap();
    sandbox.stop().await.unwrap();
}

#[tokio::test]
async fn worker_secrets_stay_on_host_and_only_workspace_files_reach_the_stub() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("public.txt"), "safe-workspace-content").unwrap();
    let adapter = Arc::new(Scripted {
        calls: &[
            ("sandbox_bash", r#"{"command":"printf '%s' \"${HOST_API_KEY-unset}\"; ls /"}"#),
            ("sandbox_list", r#"{"path":"."}"#),
            ("sandbox_read", r#"{"path":"public.txt"}"#),
        ],
        check: Some(|config| {
            assert_eq!(config.env.get("HOST_API_KEY").map(String::as_str), Some("host-only-value"));
            assert!(config.mcp_servers.is_empty());
            assert!(config.disabled_tools.iter().any(|name| name == "bash"));
            assert!(!config.cwd.join("public.txt").exists(), "the harness cwd shows the workspace");
        }),
        ..Scripted::default()
    });
    let observed = Arc::clone(&adapter.observed);
    let mut template = SessionConfig { session_id: "parent".into(), cwd: workspace.path().into(), ..SessionConfig::default() };
    template.env.insert("HOST_API_KEY".into(), "host-only-value".into());
    let provider: Arc<dyn SandboxProvider> = Arc::new(LocalProvider::new(stub()));
    let tool = SandboxTask::new(adapter, template, Arc::new(MemoryJournal::default()), vec![provider]);
    let args: SandboxTaskArgs = serde_json::from_str(r#"{"task":"Inspect files","kind":"local"}"#).unwrap();
    tool.execute(args).await.unwrap();
    let observed = observed.lock().unwrap();
    assert!(observed[0].starts_with("unset"), "the sandbox inherited the host environment: {}", observed[0]);
    assert!(observed[1].contains("public.txt"), "{}", observed[1]);
    assert_eq!(observed[2], "safe-workspace-content");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_service_tool_runs_isolated_and_returns_the_patch() {
    let repo = repository();
    let service = service();
    let mut events = service.subscribe();
    let template = SessionConfig { session_id: "parent".into(), cwd: repo.path().into(), ..SessionConfig::default() };
    let adapter = Scripted {
        calls: &[
            ("sandbox_write", r#"{"path":"notes/hello.txt","content":"from the sandbox\n"}"#),
            ("sandbox_bash", r#"{"command":"printf 'changed\\n' > tracked.txt"}"#),
        ],
        ..Scripted::default()
    };
    let (spec, handler) = service.host_tool(Arc::new(adapter), &template).expect("the local provider is available");
    assert_eq!(spec.name, "sandbox_task");

    let answer = handler.call("parent", "sandbox_task", r#"{"task":"Write the notes","kind":"local","isolation":true}"#).await.unwrap();
    let result: SandboxTaskResult = serde_json::from_str(&answer).unwrap();
    let patch = result.patch.expect("isolated runs return a patch");
    assert!(patch.contains("+from the sandbox") && patch.contains("-original") && patch.contains("+changed"), "{patch}");
    let files: Vec<_> = patch_files(&patch).into_iter().map(|file| file.path).collect();
    assert_eq!(files, ["notes/hello.txt", "tracked.txt"]);

    // The workspace is untouched until the patch is applied.
    assert!(!repo.path().join("notes").exists());
    assert_eq!(std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(), "original\n");
    let worktrees = std::process::Command::new("git").args(["worktree", "list"]).current_dir(repo.path()).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&worktrees.stdout).lines().count(), 1, "the isolated worktree was removed");

    let SandboxEvent::Started(run) = events.recv().await.unwrap() else { panic!("a start first") };
    assert_eq!((run.parent.as_str(), run.kind, run.isolated, run.task.as_str()), ("parent", SandboxKind::Local, true, "Write the notes"));
    assert_eq!(run.worker, result.session_id);
    match events.recv().await.unwrap() {
        SandboxEvent::Finished { run: finished, patch: Some(sent), .. } => assert_eq!((finished, sent), (run.run, patch.clone())),
        other => panic!("expected the finish with its patch, got {other:?}"),
    }

    apply_patch(repo.path(), patch.as_bytes()).unwrap();
    assert_eq!(std::fs::read_to_string(repo.path().join("notes/hello.txt")).unwrap(), "from the sandbox\n");
    assert_eq!(std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(), "changed\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_run_is_reported() {
    let plain = tempfile::tempdir().unwrap();
    let service = service();
    let mut events = service.subscribe();
    let template = SessionConfig { session_id: "parent".into(), cwd: plain.path().into(), ..SessionConfig::default() };
    let (_, handler) = service.host_tool(Arc::new(Scripted::default()), &template).unwrap();
    let error = handler.call("parent", "sandbox_task", r#"{"task":"x","kind":"local","isolation":true}"#).await.unwrap_err();
    assert!(error.contains("isolation"), "{error}");
    assert!(matches!(events.recv().await.unwrap(), SandboxEvent::Started(_)));
    assert!(matches!(events.recv().await.unwrap(), SandboxEvent::Failed { error: reported, .. } if error.contains(&reported)));
    let error = handler.call("parent", "sandbox_task", r#"{"task":"x","kind":"firecracker"}"#).await.unwrap_err();
    assert!(error.contains("kind") || error.contains("firecracker"), "{error}");
}

/// omp in always-ask mode asks before its tool call and only goes on once
/// the decision is answered; the worker answers for itself.
#[tokio::test(flavor = "multi_thread")]
async fn the_worker_allows_its_own_approval_requests() {
    let repo = repository();
    let template = SessionConfig { session_id: "parent".into(), node: "host".into(), cwd: repo.path().into(), ..SessionConfig::default() };
    let adapter = Arc::new(Scripted { calls: &[("sandbox_write", r#"{"path":"approved.txt","content":"approved\n"}"#)], asks: true, ..Scripted::default() });
    let answers = Arc::clone(&adapter.answers);
    let (_, handler) = service().host_tool(adapter, &template).unwrap();
    let call = handler.call("parent", "sandbox_task", r#"{"task":"Write it","kind":"local","isolation":true}"#);
    let answer = tokio::time::timeout(std::time::Duration::from_secs(30), call).await.expect("the worker doesn't wait on nobody").unwrap();
    let result: SandboxTaskResult = serde_json::from_str(&answer).unwrap();
    assert!(result.patch.expect("a patch").contains("+approved"));
    let answers = answers.lock().unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!((answers[0].values.as_slice(), answers[0].by.as_str()), (["allow_once".to_owned()].as_slice(), "host"));
}
