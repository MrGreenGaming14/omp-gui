//! Opt-in test against the installed `omp`, with real model calls: an omp
//! session calls `sandbox_task` with the local provider in an isolated git
//! worktree; an omp worker does the work through the stub and the patch comes
//! back. Run with `cargo test -p loa-sandbox-stub --test live -- --ignored`.
//! `LOA_LIVE_MODEL` picks the model (default `anthropic/claude-haiku-5-5`).

use loa_adapter::{Adapter, Prompt, SessionConfig};
use loa_adapter_omp::{OmpAdapter, OmpAdapterConfig};
use loa_core::{ElementKind, MemorySink, attr};
use loa_sandbox::worker::MemoryJournal;
use loa_sandbox::{SandboxConfig, SandboxEvent, SandboxService};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

const TURN_TIMEOUT: Duration = Duration::from_secs(600);

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git").args(args).current_dir(dir).status().unwrap();
    assert!(status.success(), "git {args:?}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn an_omp_session_runs_sandbox_task_in_an_isolated_worktree_and_gets_the_patch() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("README.md"), "# Demo\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["-c", "user.name=Test", "-c", "user.email=t@example.org", "commit", "-qm", "baseline"]);

    let mut adapter_config = OmpAdapterConfig::new("live-test", dir.path().join("state"));
    adapter_config.approval_mode = Some("yolo".into());
    let adapter: Arc<dyn Adapter> = Arc::new(OmpAdapter::new(adapter_config));
    let journal = Arc::new(MemoryJournal::default());
    let service = SandboxService::new(SandboxConfig::default(), journal.clone())
        .with_stub(Some(PathBuf::from(env!("CARGO_BIN_EXE_loa-sandbox-stub"))));
    let mut events = service.subscribe();

    let mut config = SessionConfig {
        session_id: "live-sandbox".into(),
        node: "live-test".into(),
        cwd: repo.clone(),
        model: Some(std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".into())),
        ..Default::default()
    };
    let (spec, handler) = service.host_tool(Arc::clone(&adapter), &config).expect("the local provider is available");
    config.host_tools.push(spec);
    config.host_tool_handler = Some(handler);

    let sink = Arc::new(MemorySink::new());
    let session = adapter.start(config, sink.clone()).await.expect("omp starts in rpc mode");
    session
        .send(Prompt {
            text: "Call the sandbox_task tool exactly once with kind \"local\", isolation true, and this task: \
                   \"Create the file hello.txt in the workspace root containing exactly the line: hello from the sandbox\". \
                   Do not create the file yourself. When the tool returns, tell me which files its patch changes."
                .into(),
            by: "live-test".into(),
            ..Default::default()
        })
        .await
        .expect("prompt accepted");
    tokio::time::timeout(TURN_TIMEOUT, session.wait_idle()).await.expect("turn finished in time").expect("turn ended");
    session.shutdown().await.unwrap();

    let SandboxEvent::Started(run) = events.try_recv().expect("sandbox_task ran") else { panic!("a start first") };
    assert_eq!(run.kind, loa_sandbox::SandboxKind::Local);
    assert!(run.isolated);
    let patch = match events.try_recv().expect("the run ended") {
        SandboxEvent::Finished { patch, .. } => patch.expect("an isolated run returns a patch"),
        SandboxEvent::Failed { error, .. } => panic!("sandbox_task failed: {error}"),
        other => panic!("unexpected {other:?}"),
    };
    eprintln!("patch:\n{patch}");
    assert!(patch.contains("hello.txt") && patch.contains("+hello from the sandbox"), "{patch}");
    assert!(!repo.join("hello.txt").exists(), "the workspace stays untouched");

    // The worker session was journaled on the host and used the sandbox tools.
    let worker = journal.session(&run.worker).expect("the worker session was journaled").dom();
    let calls: Vec<_> = worker.iter().filter(|n| n.kind == ElementKind::Call).filter_map(|n| n.str_attr(attr::TOOL)).collect();
    eprintln!("worker calls: {calls:?}");
    assert!(calls.iter().any(|name| name.starts_with("sandbox_")), "{calls:?}");

    // The parent's DOM holds the sandbox_task call whose result carries the patch.
    let dom = sink.dom();
    let call = dom.iter().find(|n| n.kind == ElementKind::Call && n.str_attr(attr::TOOL) == Some("sandbox_task")).expect("the parent called sandbox_task");
    let result = dom.children_of_kind(call.id, ElementKind::Result).next().expect("the call has a result");
    assert!(result.text.contains("hello.txt"), "{}", result.text);
}
