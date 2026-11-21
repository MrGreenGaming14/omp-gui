//! A real shell on a local PTY (`/bin/sh`, or `cmd.exe` on ConPTY): cwd, input, resize, exit status, and the cwd check.

use bytes::Bytes;
use loa_ui_terminal::{LocalPtyBackend, LocalShell, PtyBackend, Target};
use loa_view::terminal::port::{ChannelRead, Error, GridSize};
use std::path::Path;
use std::time::Duration;

/// The platform shell in `cwd`.
fn shell(cwd: &Path) -> LocalShell {
    let (program, args): (&str, &[&str]) = if cfg!(windows) { ("cmd.exe", &["/D", "/Q"]) } else { ("/bin/sh", &[]) };
    LocalShell { program: Some(program.into()), args: args.iter().map(Into::into).collect(), ..LocalShell::login_shell(printed(cwd)) }
}

/// `path` as the shell prints its working directory: no `\\?\` prefix on Windows.
fn printed(path: &Path) -> String {
    let text = path.to_str().unwrap();
    if cfg!(windows) { text.trim_start_matches(r"\\?\").to_owned() } else { text.to_owned() }
}

/// Prints the working directory, the window size and `$LOA_MARK`, then exits with 3. The mark is expanded only
/// in the output, so the echoed command line can't match however ConPTY redraws the screen.
const SCRIPT: &str = if cfg!(windows) {
    "cd&powershell.exe -NoProfile -Command \"'{0} {1}' -f [Console]::WindowHeight,[Console]::WindowWidth\"&echo %LOA_MARK%&exit 3\r"
} else {
    "pwd; stty size; echo $LOA_MARK; exit 3\n"
};

#[tokio::test]
async fn sh_runs_in_its_cwd_resizes_and_reports_its_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let mut shell = shell(&cwd);
    shell.env = vec![("PS1".into(), "$ ".into()), ("LOA_MARK".into(), "done-42".into())];
    let mut ch = LocalPtyBackend.open(&Target::Local(shell.clone()), GridSize::new(80, 24)).await.unwrap();
    ch.writer.resize(GridSize::new(132, 43)).await.unwrap();
    ch.writer.write(Bytes::from_static(SCRIPT.as_bytes())).await.unwrap();
    let mut out = String::new();
    let drain = async {
        loop {
            match ch.reader.read().await.expect("read") {
                ChannelRead::Data(b) => {
                    // ConPTY asks for the cursor position at startup and waits for the terminal to answer.
                    if b.windows(4).any(|window| window == b"\x1b[6n") {
                        ch.writer.write(Bytes::from_static(b"\x1b[1;1R")).await.unwrap();
                    }
                    out.push_str(&String::from_utf8_lossy(&b));
                }
                ChannelRead::Closed { exit_code } => return exit_code,
            }
        }
    };
    let exit = tokio::time::timeout(Duration::from_secs(10), drain).await;
    assert_eq!(exit.unwrap_or_else(|_| panic!("exits in time; output: {out:?}")), Some(3));
    assert!(out.contains(&printed(&cwd)) && out.contains("43 132") && out.contains("done-42"), "{out}");
    shell.cwd = cwd.join("missing");
    assert!(matches!(LocalPtyBackend.open(&Target::Local(shell), GridSize::default()).await, Err(Error::NotADirectory(_))));
    assert!(matches!(LocalPtyBackend.open(&Target::Remote { host: "h".into() }, GridSize::default()).await, Err(Error::NoRemote)));
}
