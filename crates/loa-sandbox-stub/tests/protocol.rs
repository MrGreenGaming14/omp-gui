use loa_sandbox::client::{ExecEvent, StubClient};
use loa_sandbox::proto::{Command, ExecRequest, FileKind};
use loa_sandbox::provider::local::LocalProvider;
use loa_sandbox::provider::{SandboxOptions, SandboxProvider, SandboxSpec};
use std::path::PathBuf;

#[cfg(unix)]
const BUSY_LOOP: &str = "while :; do :; done";
#[cfg(windows)]
const BUSY_LOOP: &str = "ping -n 60 127.0.0.1 > nul";

/// Prints `out` to stdout and `err` to stderr, without newlines, and exits 7.
#[cfg(unix)]
fn stdout_stderr_exit_7() -> Command {
    Command::Argv(vec!["/bin/sh".into(), "-c".into(), "printf out; printf err >&2; exit 7".into()])
}

/// cmd has no printf: `set /p` fed from nul prints its text without a newline.
#[cfg(windows)]
fn stdout_stderr_exit_7() -> Command {
    Command::Shell("<nul set /p =out& <nul 1>&2 set /p =err& exit 7".into())
}

async fn stub(root: PathBuf) -> (StubClient, Box<dyn loa_sandbox::provider::Sandbox>) {
    let provider = LocalProvider::new(env!("CARGO_BIN_EXE_loa-sandbox-stub"));
    let mut sandbox = provider.start(&SandboxSpec {
        name: "protocol-test".into(), workspace: root, env: Vec::new(), options: SandboxOptions::default(),
    }).await.unwrap();
    let client = StubClient::connect(sandbox.take_channel().unwrap()).await.unwrap();
    (client, sandbox)
}

#[tokio::test]
async fn streams_stdout_stderr_and_reports_exit_status() {
    let root = tempfile::tempdir().unwrap();
    let (client, sandbox) = stub(root.path().to_path_buf()).await;
    let mut command = client.spawn(ExecRequest {
        command: stdout_stderr_exit_7(),
        cwd: None, env: Vec::new(), stdin: None, timeout_ms: None,
    }).await.unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = loop {
        match command.next().await.unwrap().unwrap() {
            ExecEvent::Stdout(bytes) => stdout.extend(bytes),
            ExecEvent::Stderr(bytes) => stderr.extend(bytes),
            ExecEvent::Exit(status) => break status,
        }
    };
    assert_eq!(stdout, b"out");
    assert_eq!(stderr, b"err");
    assert_eq!(status.code, Some(7));
    client.shutdown().await.unwrap();
    sandbox.stop().await.unwrap();
}

#[tokio::test]
async fn writes_reads_lists_and_stats_files_in_workspace() {
    let root = tempfile::tempdir().unwrap();
    let (client, sandbox) = stub(root.path().to_path_buf()).await;
    client.write_file("nested/data.txt", b"abc".to_vec(), Some(0o600)).await.unwrap();
    assert_eq!(client.read_file("nested/data.txt", None).await.unwrap(), b"abc");
    let entries = client.list_dir("nested").await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "data.txt");
    assert_eq!(client.stat("nested/data.txt").await.unwrap().unwrap().kind, FileKind::File);
    assert!(client.stat("nested/missing").await.unwrap().is_none());
    client.shutdown().await.unwrap();
    sandbox.stop().await.unwrap();
}

#[tokio::test]
async fn command_timeout_kills_process_group() {
    let root = tempfile::tempdir().unwrap();
    let (client, sandbox) = stub(root.path().to_path_buf()).await;
    let output = client.exec(ExecRequest {
        command: Command::Shell(BUSY_LOOP.into()),
        cwd: None, env: Vec::new(), stdin: None, timeout_ms: Some(30),
    }).await.unwrap();
    assert!(output.status.timed_out);
    assert!(!output.status.success());
    client.shutdown().await.unwrap();
    sandbox.stop().await.unwrap();
}
