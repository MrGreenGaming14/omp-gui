//! The stub protocol, the worker's host boundary and `sandbox_task` as the app
//! wires it, against the real stub binary and a scripted harness standing in
//! for the model.

use async_trait::async_trait;
use loa_contracts::harness::{Adapter, AdapterError, Answer, Capabilities, HarnessSession, HostTools, Prompt, Result, SessionConfig, SessionState};
use loa_core::{Dom, Draft, ElementKind, MemorySink, NodeId, PatchSink, SessionMode, Value, attr};
use loa_sandbox::client::StubClient;
use loa_sandbox::isolation::{apply_patch, patch_files};
use loa_sandbox::proto::ExecRequest;
use loa_sandbox::provider::local::LocalProvider;
use loa_sandbox::provider::{Sandbox, SandboxProvider, SandboxSpec};
use loa_sandbox::worker::WorkerJournal;
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

struct Journal;

impl WorkerJournal for Journal {
    fn open(&self, _: &str) -> loa_sandbox::Result<Arc<dyn PatchSink>> { Ok(Arc::new(MemorySink::new())) }
}

fn service() -> SandboxService {
    SandboxService::new(SandboxConfig::default(), Arc::new(Journal)).with_stub(Some(stub()))
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
    let output = client.exec(ExecRequest::shell("printf \"$1\"; printf err >&2; exit 7", &["out"])).await.unwrap();
    assert_eq!((output.stdout.as_slice(), output.stderr.as_slice(), output.status.code), (&b"out"[..], &b"err"[..], Some(7)));
    let error = client.run(ExecRequest { argv: vec!["missing-program".into()], ..ExecRequest::default() }).await.unwrap_err();
    assert!(error.to_string().contains("spawn"), "{error}");
    let output = client.exec(ExecRequest { timeout_ms: Some(30), ..ExecRequest::shell("while :; do :; done", &[]) }).await.unwrap();
    assert!(output.status.timed_out && !output.status.success());
    client.shutdown().await.unwrap();
    sandbox.stop().await.unwrap();
}

#[tokio::test]
async fn copied_workspaces_go_in_and_come_back_without_git_data() {
    let (workspace, root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::create_dir_all(workspace.path().join(".git")).unwrap();
    std::fs::write(workspace.path().join("kept.txt"), "kept").unwrap();
    std::fs::write(workspace.path().join("gone.txt"), "gone").unwrap();
    let (client, sandbox) = connect(root.path()).await;
    loa_sandbox::sync::copy_in(&client, workspace.path()).await.unwrap();
    assert!(!root.path().join(".git").exists() && root.path().join("kept.txt").exists());
    client.run(ExecRequest::shell("rm gone.txt && mkdir new && echo new > new/file", &[])).await.unwrap();
    loa_sandbox::sync::copy_out(&client, workspace.path()).await.unwrap();
    let mut names: Vec<_> = std::fs::read_dir(workspace.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    names.sort();
    assert_eq!(names, [".git", "kept.txt", "new"]);
    assert_eq!(std::fs::read_to_string(workspace.path().join("new/file")).unwrap(), "new\n");
    client.shutdown().await.unwrap();
    sandbox.stop().await.unwrap();
}

/// omp in always-ask mode asks before its tool calls and only goes on once
/// the decision is answered; the worker answers for itself. Host secrets
/// never reach the sandbox, and the workspace only changes through the patch.
#[tokio::test(flavor = "multi_thread")]
async fn an_isolated_run_keeps_secrets_on_the_host_and_returns_the_patch() {
    let repo = repository();
    let service = service();
    let mut events = service.subscribe();
    let mut template = SessionConfig { session_id: "parent".into(), node: "host".into(), cwd: repo.path().into(), ..SessionConfig::default() };
    template.env.insert("HOST_API_KEY".into(), "host-only-value".into());
    let adapter = Arc::new(Scripted {
        calls: &[
            ("sandbox_bash", r#"{"command":"printf '%s' \"${HOST_API_KEY-unset}\""}"#),
            ("sandbox_write", r#"{"path":"notes/hello.txt","content":"from the sandbox\n"}"#),
            ("sandbox_bash", r#"{"command":"printf 'changed\\n' > tracked.txt"}"#),
            ("sandbox_list", r#"{"path":"notes"}"#),
            ("sandbox_read", r#"{"path":"notes/hello.txt","limit":4}"#),
        ],
        asks: true,
        check: Some(|config| {
            assert_eq!(config.env.get("HOST_API_KEY").map(String::as_str), Some("host-only-value"));
            assert!(config.mcp_servers.is_empty() && config.disabled_tools.iter().any(|name| name == "bash"));
            assert!(!config.cwd.join("tracked.txt").exists(), "the harness cwd shows the workspace");
        }),
        ..Scripted::default()
    });
    let (observed, answers) = (Arc::clone(&adapter.observed), Arc::clone(&adapter.answers));
    let (spec, handler) = service.host_tool(adapter, &template).expect("the local provider is available");
    assert_eq!(spec.name, "sandbox_task");
    let schema: serde_json::Value = serde_json::from_str(&spec.input_schema).unwrap();
    let kinds = schema["properties"]["kind"]["enum"].as_array().unwrap();
    assert!(kinds.contains(&"local".into()) && !kinds.contains(&"firecracker".into()), "{kinds:?}");

    let call = handler.call("parent", "sandbox_task", r#"{"task":"Write the notes","kind":"local","isolation":true}"#);
    let answer = tokio::time::timeout(std::time::Duration::from_secs(30), call).await.expect("the worker doesn't wait on nobody").unwrap();
    let result: SandboxTaskResult = serde_json::from_str(&answer).unwrap();
    let observed = observed.lock().unwrap();
    assert!(observed[0].contains("unset"), "the sandbox inherited the host environment: {}", observed[0]);
    assert!(observed[3].contains("hello.txt") && observed[4] == "from", "{observed:?}");
    let answers = answers.lock().unwrap();
    assert_eq!((answers.len(), answers[0].values.as_slice(), answers[0].by.as_str()), (1, ["allow_once".to_owned()].as_slice(), "host"));

    let patch = result.patch.expect("isolated runs return a patch");
    assert!(patch.contains("+from the sandbox") && patch.contains("-original") && patch.contains("+changed"), "{patch}");
    let files: Vec<_> = patch_files(&patch).into_iter().map(|file| file.path).collect();
    assert_eq!(files, ["notes/hello.txt", "tracked.txt"]);
    assert!(!repo.path().join("notes").exists());
    assert_eq!(std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(), "original\n");
    let worktrees = std::process::Command::new("git").args(["worktree", "list"]).current_dir(repo.path()).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&worktrees.stdout).lines().count(), 1, "the isolated worktree was removed");

    let SandboxEvent::Started(run) = events.recv().await.unwrap() else { panic!("a start first") };
    assert_eq!((run.parent.as_str(), run.kind, run.isolated, run.worker.as_str()), ("parent", SandboxKind::Local, true, result.session_id.as_str()));
    let SandboxEvent::Finished { run: finished, patch: Some(sent), .. } = events.recv().await.unwrap() else { panic!("the finish with its patch") };
    assert_eq!((finished, &sent), (run.run, &patch));

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
    assert!(error.contains("firecracker"), "{error}");
}
