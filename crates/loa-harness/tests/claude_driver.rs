//! The process driver against a fake `claude` that speaks the stream JSON
//! and control protocol the real CLI speaks.

use async_trait::async_trait;
use loa_contracts::harness::{
    Adapter, AdapterError, Answer, HostToolSpec, HostTools, McpServerSpec, McpTransport, ModelSelection, Prompt,
    SessionConfig, SessionState,
};
use loa_harness::claude::{ClaudeAdapter, HOST_SERVER};
use loa_core::{CommitError, Dom, Draft, ElementKind, MemorySink, Node, Patch, PatchSink, SessionMode, answer, attr};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::sync::watch;

const FAKE: &str = include_str!("../fixtures/claude/fake-cli.sh");

/// The fake, written once to a temp dir by a separate process so no fork of
/// this one holds it open for writing when it runs ("text file busy").
static FAKE_CLAUDE: LazyLock<(tempfile::TempDir, PathBuf)> = LazyLock::new(|| {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claude");
    loa_util::exec::write_executable(&path, FAKE).unwrap();
    (dir, path)
});

fn fake_claude() -> &'static Path {
    &FAKE_CLAUDE.1
}

/// A sink that wakes waiters on every commit.
struct WatchSink {
    inner: MemorySink,
    seq: watch::Sender<u64>,
}

impl WatchSink {
    fn new() -> Arc<Self> {
        Arc::new(Self { inner: MemorySink::new(), seq: watch::channel(0).0 })
    }

    fn from_dom(dom: Dom) -> Arc<Self> {
        Arc::new(Self { inner: MemorySink::from_dom(dom), seq: watch::channel(0).0 })
    }

    /// Waits until `check` holds on the DOM.
    async fn until(&self, what: &str, check: impl Fn(&Dom) -> bool) {
        let mut rx = self.seq.subscribe();
        let wait = rx.wait_for(|_| self.inner.with_dom(&check));
        if tokio::time::timeout(Duration::from_secs(20), wait).await.is_err() {
            panic!("timed out waiting for {what}; dom: {:#?}", self.inner.dom().iter().collect::<Vec<_>>());
        }
    }
}

impl PatchSink for WatchSink {
    fn commit_at(&self, draft: Draft, ts: i64) -> Result<Patch, CommitError> {
        let patch = self.inner.commit_at(draft, ts)?;
        self.seq.send_replace(patch.seq);
        Ok(patch)
    }
}

struct Recorder(Mutex<Vec<(String, String, String)>>);

#[async_trait]
impl HostTools for Recorder {
    async fn call(&self, session_id: &str, tool: &str, args: &str) -> Result<String, String> {
        self.0.lock().unwrap().push((session_id.to_owned(), tool.to_owned(), args.to_owned()));
        Ok("3 hits in auth.rs".to_owned())
    }
}

struct Files {
    dir: tempfile::TempDir,
}

impl Files {
    fn new() -> Self {
        Self { dir: tempfile::tempdir().unwrap() }
    }

    fn args(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("args")).unwrap().lines().map(str::to_owned).collect()
    }

    fn stdin(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.dir.path().join("stdin")).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn config(&self, recorder: Option<Arc<Recorder>>) -> SessionConfig {
        let mut config = SessionConfig {
            session_id: "app-1".into(),
            node: "node-1".into(),
            cwd: self.dir.path().to_owned(),
            model: Some("fake-model".into()),
            context: Some("Project digest: uses SQLite.".into()),
            disabled_tools: vec!["WebFetch".into()],
            mcp_servers: vec![McpServerSpec {
                name: "browser".into(),
                transport: McpTransport::Stdio { command: "browser-mcp".into(), args: vec!["--headless".into()], env: Default::default() },
            }],
            executable: Some(fake_claude().to_owned()),
            ..SessionConfig::default()
        };
        if let Some(recorder) = recorder {
            config.host_tools = vec![HostToolSpec {
                name: "project_search".into(),
                description: "Search project sessions".into(),
                input_schema: r#"{"type":"object","properties":{"query":{"type":"string"}}}"#.into(),
            }];
            config.host_tool_handler = Some(recorder);
        }
        config.env.insert("ARGS_FILE".into(), self.dir.path().join("args").display().to_string());
        config.env.insert("LOG_FILE".into(), self.dir.path().join("stdin").display().to_string());
        config
    }
}

fn prompt(text: &str) -> Prompt {
    Prompt { text: text.into(), attachments: Vec::new(), by: "alice".into() }
}

fn find(dom: &Dom, kind: ElementKind) -> Vec<&Node> {
    dom.iter().filter(|n| n.kind == kind).collect()
}

fn status_of(dom: &Dom, kind: ElementKind) -> Option<String> {
    find(dom, kind).last().and_then(|n| n.str_attr(attr::STATUS)).map(str::to_owned)
}

#[tokio::test]
async fn a_turn_with_approval_and_a_host_tool_runs_end_to_end() {
    let files = Files::new();
    let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
    let sink = WatchSink::new();
    let session = ClaudeAdapter::new().start(files.config(Some(recorder.clone())), sink.clone()).await.unwrap();

    let args = files.args();
    let pair = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].clone());
    assert_eq!(pair("--input-format").as_deref(), Some("stream-json"));
    assert_eq!(pair("--output-format").as_deref(), Some("stream-json"));
    assert_eq!(pair("--permission-prompt-tool").as_deref(), Some("stdio"));
    assert_eq!(pair("--model").as_deref(), Some("fake-model"));
    assert_eq!(pair("--append-system-prompt").as_deref(), Some("Project digest: uses SQLite."));
    assert_eq!(pair("--disallowedTools").as_deref(), Some("WebFetch"));
    let mcp: Value = serde_json::from_str(&pair("--mcp-config").unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"][HOST_SERVER]["type"], "sdk");
    assert_eq!(mcp["mcpServers"]["browser"]["command"], "browser-mcp");
    assert_eq!(mcp["mcpServers"]["browser"]["args"][0], "--headless");

    session.send(prompt("please use the tool")).await.unwrap();
    sink.until("the approval", |dom| status_of(dom, ElementKind::Decision).as_deref() == Some("waiting")).await;
    assert_eq!(session.state(), SessionState::Waiting);
    let decision = sink.inner.with_dom(|dom| {
        assert_eq!(dom.root().unwrap().str_attr(attr::NATIVE_ID), Some("cc-fake"));
        find(dom, ElementKind::Decision)[0].id
    });

    session.answer(decision, Answer { values: vec![answer::ALLOW_ONCE.into()], reason: None, by: "alice".into() }).await.unwrap();
    session.wait_idle().await.unwrap();

    let dom = sink.inner.dom();
    assert_eq!(status_of(&dom, ElementKind::Turn).as_deref(), Some("done"));
    let call = find(&dom, ElementKind::Call)[0];
    assert_eq!(call.str_attr(attr::STATUS), Some("done"));
    assert_eq!(find(&dom, ElementKind::Result)[0].text, "file-a\nfile-b");
    let texts: Vec<&str> = find(&dom, ElementKind::Message).iter().map(|m| m.text.as_str()).collect();
    assert_eq!(texts, ["please use the tool", "Listing.", "Done."]);
    assert_eq!(find(&dom, ElementKind::Usage)[0].int_attr(attr::OUTPUT_TOKENS), Some(7));

    let calls = recorder.0.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!((calls[0].0.as_str(), calls[0].1.as_str()), ("app-1", "project_search"));
    assert_eq!(serde_json::from_str::<Value>(&calls[0].2).unwrap(), serde_json::json!({"query": "auth"}));

    session.shutdown().await.unwrap();
    assert_eq!(session.state(), SessionState::Closed);

    let stdin = files.stdin();
    let user = stdin.iter().find(|l| l["type"] == "user").unwrap();
    assert_eq!(user["message"]["content"], "please use the tool");
    let prompt_id = user["uuid"].as_str().unwrap();
    let turn_id = find(&dom, ElementKind::Turn)[0].str_attr(attr::ID).unwrap().to_owned();
    assert_eq!(prompt_id, turn_id);
    let permission = stdin.iter().find(|l| l["response"]["request_id"] == "perm-1").unwrap();
    assert_eq!(permission["response"]["response"], serde_json::json!({"behavior": "allow", "updatedInput": {"command": "ls"}}));
    let mcp = stdin.iter().find(|l| l["response"]["request_id"] == "mcp-1").unwrap();
    let reply = &mcp["response"]["response"]["mcp_response"];
    assert_eq!(reply["id"], 3);
    assert_eq!(reply["result"]["content"][0]["text"], "3 hits in auth.rs");
    assert_eq!(reply["result"]["isError"], false);
}

#[tokio::test]
async fn interrupt_model_and_mode_go_through_control_requests() {
    let files = Files::new();
    let sink = WatchSink::new();
    let session = ClaudeAdapter::new().start(files.config(None), sink.clone()).await.unwrap();
    let models = session.models().await.unwrap();
    let listed: Vec<(&str, &str, Vec<&str>)> =
        models.iter().map(|m| (m.id.as_str(), m.name.as_str(), m.efforts.iter().map(String::as_str).collect())).collect();
    assert_eq!(listed, [("default", "Default (recommended)", vec![]), ("haiku", "Haiku", vec!["low", "medium", "high"])]);
    assert!(models.iter().all(|m| m.provider == "anthropic"));

    session.send(prompt("hang please")).await.unwrap();
    assert!(matches!(session.send(prompt("another")).await, Err(AdapterError::Busy)));
    sink.until("init", |dom| dom.root().unwrap().str_attr(attr::NATIVE_ID).is_some()).await;
    session.interrupt().await.unwrap();
    assert_eq!(session.state(), SessionState::Idle);
    assert_eq!(sink.inner.with_dom(|dom| status_of(dom, ElementKind::Turn)).as_deref(), Some("cancelled"));

    session.set_model(ModelSelection { model: "other-model".into(), role: None }).await.unwrap();
    assert_eq!(sink.inner.with_dom(|dom| dom.root().unwrap().str_attr(attr::MODEL).map(str::to_owned)).as_deref(), Some("other-model"));
    let refused = session.set_model(ModelSelection { model: "bad-model".into(), role: None }).await;
    assert!(matches!(&refused, Err(AdapterError::Protocol(e)) if e == "unknown model"), "{refused:?}");
    assert_eq!(sink.inner.with_dom(|dom| dom.root().unwrap().str_attr(attr::MODEL).map(str::to_owned)).as_deref(), Some("other-model"));

    session.set_mode(SessionMode::Plan).await.unwrap();
    assert_eq!(sink.inner.with_dom(|dom| dom.root().unwrap().str_attr(attr::MODE).map(str::to_owned)).as_deref(), Some("plan"));
    session.shutdown().await.unwrap();

    let stdin = files.stdin();
    let subtypes: Vec<&str> = stdin.iter().filter_map(|l| l["request"]["subtype"].as_str()).collect();
    assert_eq!(subtypes, ["initialize", "interrupt", "set_model", "set_model", "set_permission_mode"]);
    let mode = stdin.iter().find(|l| l["request"]["subtype"] == "set_permission_mode").unwrap();
    assert_eq!(mode["request"]["mode"], "plan");
}

#[tokio::test]
async fn a_crashing_cli_fails_the_turn_and_closes_the_session() {
    let files = Files::new();
    let sink = WatchSink::new();
    let session = ClaudeAdapter::new().start(files.config(None), sink.clone()).await.unwrap();
    session.send(prompt("die now")).await.unwrap();
    session.wait_idle().await.unwrap();
    assert_eq!(session.state(), SessionState::Closed);
    assert_eq!(sink.inner.with_dom(|dom| status_of(dom, ElementKind::Turn)).as_deref(), Some("error"));
    assert!(matches!(session.send(prompt("again")).await, Err(AdapterError::Closed)));
}

#[tokio::test]
async fn resume_and_fork_pass_the_native_session_to_the_cli() {
    let files = Files::new();
    let sink = WatchSink::new();
    let session = ClaudeAdapter::new().start(files.config(Some(Arc::new(Recorder(Mutex::new(Vec::new()))))), sink.clone()).await.unwrap();
    session.send(prompt("use the tool")).await.unwrap();
    sink.until("the approval", |dom| status_of(dom, ElementKind::Decision).as_deref() == Some("waiting")).await;
    // The process goes away while the decision waits.
    session.shutdown().await.unwrap();
    let dom = sink.inner.dom();

    let resumed_sink = WatchSink::from_dom(dom.clone());
    let resumed = ClaudeAdapter::new().resume(files.config(None), &dom, resumed_sink.clone()).await.unwrap();
    assert!(files.args().contains(&"--resume=cc-fake".to_owned()));
    assert!(!files.args().contains(&"--fork-session".to_owned()));
    // The turn the old process left open can't finish any more.
    assert_eq!(resumed_sink.inner.with_dom(|dom| status_of(dom, ElementKind::Turn)).as_deref(), Some("cancelled"));
    assert_eq!(resumed_sink.inner.with_dom(|dom| status_of(dom, ElementKind::Decision)).as_deref(), Some("cancelled"));
    assert_eq!(resumed.state(), SessionState::Idle);
    resumed.shutdown().await.unwrap();

    let forked_sink = WatchSink::from_dom(dom.clone());
    let forked = ClaudeAdapter::new().fork(files.config(None), &dom, forked_sink).await.unwrap();
    let args = files.args();
    assert!(args.contains(&"--resume=cc-fake".to_owned()));
    assert!(args.contains(&"--fork-session".to_owned()));
    assert!(args.contains(&"--resume-session-at=a-1b".to_owned()), "{args:?}");
    forked.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_missing_executable_is_reported_as_not_installed() {
    let files = Files::new();
    let mut config = files.config(None);
    config.executable = Some(files.dir.path().join("no-such-claude"));
    let started = ClaudeAdapter::new().start(config, WatchSink::new()).await;
    assert!(matches!(started, Err(AdapterError::NotInstalled(_))));
}
