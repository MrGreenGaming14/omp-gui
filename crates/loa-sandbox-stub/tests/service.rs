//! `sandbox_task` as the app wires it: the service's host tool, the local
//! provider running the real stub, an isolated git worktree and the patch
//! that comes back, with a scripted harness standing in for the model.

use async_trait::async_trait;
use loa_adapter::{Adapter, AdapterError, Answer, Capabilities, HarnessSession, HostTools, Prompt, SessionConfig, SessionState};
use loa_core::{Dom, Draft, ElementKind, NodeId, PatchSink, SessionMode, Value, attr};
use loa_sandbox::isolation::{apply_patch, patch_files};
use loa_sandbox::worker::MemoryJournal;
use loa_sandbox::{SandboxConfig, SandboxEvent, SandboxKind, SandboxService, SandboxTaskResult};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

/// The `sandbox_bash` call that overwrites `tracked.txt` with `changed`.
#[cfg(unix)]
const OVERWRITE_TRACKED: &str = r#"{"command":"printf 'changed\\n' > tracked.txt"}"#;
#[cfg(windows)]
const OVERWRITE_TRACKED: &str = r#"{"command":"echo changed> tracked.txt"}"#;

/// A harness whose "model" writes one file through `sandbox_write`.
struct ScriptedAdapter;

struct ScriptedSession {
    tools: Arc<dyn HostTools>,
    id: String,
}

#[async_trait]
impl Adapter for ScriptedAdapter {
    fn harness(&self) -> &'static str { "scripted" }
    fn capabilities(&self) -> Capabilities { Capabilities::default() }
    async fn start(&self, config: SessionConfig, _sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        assert!(config.host_tools.iter().all(|tool| tool.name.starts_with("sandbox_") && tool.name != "sandbox_task"));
        let tools = config.host_tool_handler.ok_or_else(|| AdapterError::Format("worker has no sandbox tools".into()))?;
        Ok(Box::new(ScriptedSession { tools, id: config.session_id }))
    }
    async fn resume(&self, _: SessionConfig, _: &Dom, _: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        Err(AdapterError::Format("unexpected resume".into()))
    }
}

#[async_trait]
impl HarnessSession for ScriptedSession {
    async fn send(&self, _: Prompt) -> loa_adapter::Result<()> {
        self.tools.call(&self.id, "sandbox_write", r#"{"path":"notes/hello.txt","content":"from the sandbox\n"}"#)
            .await.map_err(AdapterError::Protocol)?;
        self.tools.call(&self.id, "sandbox_bash", OVERWRITE_TRACKED)
            .await.map_err(AdapterError::Protocol)?;
        Ok(())
    }
    async fn interrupt(&self) -> loa_adapter::Result<()> { Ok(()) }
    async fn answer(&self, _: NodeId, _: Answer) -> loa_adapter::Result<()> { Ok(()) }
    async fn set_mode(&self, _: SessionMode) -> loa_adapter::Result<()> { Ok(()) }
    fn harness(&self) -> &'static str { "scripted" }
    fn state(&self) -> SessionState { SessionState::Idle }
    async fn wait_idle(&self) -> loa_adapter::Result<()> { Ok(()) }
    async fn shutdown(&self) -> loa_adapter::Result<()> { Ok(()) }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git").args(args).current_dir(dir).status().unwrap();
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

#[tokio::test(flavor = "multi_thread")]
async fn the_service_tool_runs_isolated_and_returns_the_patch() {
    let repo = repository();
    let service = SandboxService::new(SandboxConfig::default(), Arc::new(MemoryJournal::default()))
        .with_stub(Some(PathBuf::from(env!("CARGO_BIN_EXE_loa-sandbox-stub"))));
    let mut events = service.subscribe();
    let template = SessionConfig { session_id: "parent".into(), cwd: repo.path().into(), ..SessionConfig::default() };
    let (spec, handler) = service.host_tool(Arc::new(ScriptedAdapter), &template).expect("the local provider is available");
    assert_eq!(spec.name, "sandbox_task");

    let answer = handler
        .call("parent", "sandbox_task", r#"{"task":"Write the notes","kind":"local","isolation":true}"#)
        .await
        .unwrap();
    let result: SandboxTaskResult = serde_json::from_str(&answer).unwrap();
    let patch = result.patch.expect("isolated runs return a patch");
    assert!(patch.contains("+from the sandbox") && patch.contains("-original") && patch.contains("+changed"), "{patch}");
    let files: Vec<_> = patch_files(&patch).into_iter().map(|file| file.path).collect();
    assert_eq!(files, ["notes/hello.txt", "tracked.txt"]);

    // The workspace is untouched until the patch is applied.
    assert!(!repo.path().join("notes").exists());
    assert_eq!(std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(), "original\n");
    let worktrees = Command::new("git").args(["worktree", "list"]).current_dir(repo.path()).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&worktrees.stdout).lines().count(), 1, "the isolated worktree was removed");

    let SandboxEvent::Started(run) = events.recv().await.unwrap() else { panic!("a start first") };
    assert_eq!((run.parent.as_str(), run.kind, run.isolated, run.task.as_str()), ("parent", SandboxKind::Local, true, "Write the notes"));
    assert_eq!(run.worker, result.session_id);
    match events.recv().await.unwrap() {
        SandboxEvent::Finished { run: finished, patch: Some(sent), .. } => {
            assert_eq!(finished, run.run);
            assert_eq!(sent, patch);
        }
        other => panic!("expected the finish with its patch, got {other:?}"),
    }

    apply_patch(repo.path(), patch.as_bytes()).unwrap();
    assert_eq!(std::fs::read_to_string(repo.path().join("notes/hello.txt")).unwrap().replace("\r\n", "\n"), "from the sandbox\n");
    // cmd's `echo` ends lines with CRLF.
    assert_eq!(std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap().replace("\r\n", "\n"), "changed\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_run_is_reported() {
    let plain = tempfile::tempdir().unwrap();
    let service = SandboxService::new(SandboxConfig::default(), Arc::new(MemoryJournal::default()))
        .with_stub(Some(PathBuf::from(env!("CARGO_BIN_EXE_loa-sandbox-stub"))));
    let mut events = service.subscribe();
    let template = SessionConfig { session_id: "parent".into(), cwd: plain.path().into(), ..SessionConfig::default() };
    let (_, handler) = service.host_tool(Arc::new(ScriptedAdapter), &template).unwrap();
    let error = handler.call("parent", "sandbox_task", r#"{"task":"x","kind":"local","isolation":true}"#).await.unwrap_err();
    assert!(error.starts_with("isolation:"), "{error}");
    assert!(matches!(events.recv().await.unwrap(), SandboxEvent::Started(_)));
    assert!(matches!(events.recv().await.unwrap(), SandboxEvent::Failed { error: reported, .. } if reported == error));
    let error = handler.call("parent", "sandbox_task", r#"{"task":"x","kind":"firecracker"}"#).await.unwrap_err();
    assert!(error.contains("`kind`") || error.contains("firecracker"), "{error}");
}

/// A harness that asks for approval before its tool call, as omp does in
/// always-ask mode, and only goes on once the decision is answered.
struct AskingAdapter {
    answers: Arc<std::sync::Mutex<Vec<(NodeId, Answer)>>>,
}

struct AskingSession {
    tools: Arc<dyn HostTools>,
    id: String,
    sink: Arc<dyn PatchSink>,
    answered: tokio::sync::Notify,
    answers: Arc<std::sync::Mutex<Vec<(NodeId, Answer)>>>,
}

#[async_trait]
impl Adapter for AskingAdapter {
    fn harness(&self) -> &'static str { "asking" }
    fn capabilities(&self) -> Capabilities { Capabilities::default() }
    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        let tools = config.host_tool_handler.ok_or_else(|| AdapterError::Format("worker has no sandbox tools".into()))?;
        let answers = Arc::clone(&self.answers);
        Ok(Box::new(AskingSession { tools, id: config.session_id, sink, answered: tokio::sync::Notify::new(), answers }))
    }
    async fn resume(&self, _: SessionConfig, _: &Dom, _: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        Err(AdapterError::Format("unexpected resume".into()))
    }
}

#[async_trait]
impl HarnessSession for AskingSession {
    async fn send(&self, _: Prompt) -> loa_adapter::Result<()> {
        let session_attrs = [
            (attr::ID, Value::Str(self.id.clone())),
            (attr::HARNESS, Value::Str("asking".into())),
            (attr::NODE, Value::Str("host".into())),
            (attr::CREATED, Value::Int(0)),
        ];
        let session = self.sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, session_attrs, "")).unwrap();
        let attrs = [(attr::KIND, Value::Str("approval".into())), (attr::STATUS, Value::Str("waiting".into()))];
        self.sink.commit(Draft::insert(session.node, ElementKind::Decision, attrs, "Allow tool: sandbox_write")).unwrap();
        Ok(())
    }
    async fn interrupt(&self) -> loa_adapter::Result<()> { Ok(()) }
    async fn answer(&self, decision: NodeId, answer: Answer) -> loa_adapter::Result<()> {
        self.answers.lock().unwrap().push((decision, answer));
        self.answered.notify_one();
        Ok(())
    }
    async fn set_mode(&self, _: SessionMode) -> loa_adapter::Result<()> { Ok(()) }
    fn harness(&self) -> &'static str { "asking" }
    fn state(&self) -> SessionState { SessionState::Idle }
    async fn wait_idle(&self) -> loa_adapter::Result<()> {
        self.answered.notified().await;
        self.tools.call(&self.id, "sandbox_write", r#"{"path":"approved.txt","content":"approved\n"}"#)
            .await.map_err(AdapterError::Protocol)?;
        Ok(())
    }
    async fn shutdown(&self) -> loa_adapter::Result<()> { Ok(()) }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_worker_allows_its_own_approval_requests() {
    let repo = repository();
    let service = SandboxService::new(SandboxConfig::default(), Arc::new(MemoryJournal::default()))
        .with_stub(Some(PathBuf::from(env!("CARGO_BIN_EXE_loa-sandbox-stub"))));
    let template = SessionConfig { session_id: "parent".into(), node: "host".into(), cwd: repo.path().into(), ..SessionConfig::default() };
    let answers = Arc::default();
    let (_, handler) = service.host_tool(Arc::new(AskingAdapter { answers: Arc::clone(&answers) }), &template).unwrap();
    let call = handler.call("parent", "sandbox_task", r#"{"task":"Write it","kind":"local","isolation":true}"#);
    let answer = tokio::time::timeout(std::time::Duration::from_secs(30), call).await.expect("the worker doesn't wait on nobody").unwrap();
    let result: SandboxTaskResult = serde_json::from_str(&answer).unwrap();
    assert!(result.patch.expect("a patch").contains("+approved"));
    let answers = answers.lock().unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!((answers[0].1.values.as_slice(), answers[0].1.by.as_str()), (["allow_once".to_owned()].as_slice(), "host"));
}
