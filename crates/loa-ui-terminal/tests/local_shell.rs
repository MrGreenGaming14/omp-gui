//! A real `/bin/sh` on a local PTY: cwd, input, resize, exit status, and the cwd check.

use bytes::Bytes;
use loa_ui_terminal::{ChannelRead, Error, GridSize, LocalPtyBackend, LocalShell, PtyBackend, Target};
use std::time::Duration;

#[tokio::test]
async fn sh_runs_in_its_cwd_resizes_and_reports_its_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let mut shell = LocalShell { program: Some("/bin/sh".into()), env: vec![("PS1".into(), "$ ".into())], ..LocalShell::login_shell(&cwd) };
    let mut ch = LocalPtyBackend.open(&Target::Local(shell.clone()), GridSize::new(80, 24)).await.unwrap();
    ch.writer.resize(GridSize::new(132, 43)).await.unwrap();
    ch.writer.write(Bytes::from_static(b"pwd; stty size; echo done-$((40+2)); exit 3\n")).await.unwrap();
    let mut out = String::new();
    let drain = async {
        loop {
            match ch.reader.read().await.expect("read") {
                ChannelRead::Data(b) => out.push_str(&String::from_utf8_lossy(&b)),
                ChannelRead::Closed { exit_code } => return exit_code,
            }
        }
    };
    assert_eq!(tokio::time::timeout(Duration::from_secs(10), drain).await.expect("exits in time"), Some(3));
    assert!(out.contains(cwd.to_str().unwrap()) && out.contains("43 132") && out.contains("done-42"), "{out}");
    shell.cwd = cwd.join("missing");
    assert!(matches!(LocalPtyBackend.open(&Target::Local(shell), GridSize::default()).await, Err(Error::NotADirectory(_))));
    assert!(matches!(LocalPtyBackend.open(&Target::Remote { host: "h".into() }, GridSize::default()).await, Err(Error::NoRemote)));
}
