//! Finding, writing and running executables. Children forked by other threads inherit
//! writable descriptors, so fresh files can hit `ETXTBSY`; these helpers avoid or retry it.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;

use crate::sync::LockExt;

/// Calls `spawn` again after a short doubling backoff (about 0.4 s in total) while it fails with `ETXTBSY`.
pub fn retry_busy<T>(mut spawn: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut delay = Duration::from_millis(2);
    for _ in 1..12 {
        match spawn() {
            Err(err) if err.kind() == io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(50));
            }
            other => return other,
        }
    }
    spawn()
}

/// Writes `contents` to `path` (created or truncated) with mode 0755, never holding it open for writing.
#[cfg(unix)]
pub fn write_executable(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    use std::io::Write;
    let mut writer = std::process::Command::new("/bin/sh")
        .args(["-c", r#"cat > "$1" && chmod 755 "$1""#, "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()?;
    let mut stdin = writer.stdin.take().expect("stdin is piped");
    let written = stdin.write_all(contents.as_ref());
    drop(stdin);
    let status = writer.wait()?;
    written?;
    if status.success() { Ok(()) } else { Err(io::Error::other(format!("writing {} failed: {status}", path.display()))) }
}

/// Writes `contents` to `path`; without fork there is no race to avoid.
#[cfg(not(unix))]
pub fn write_executable(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    std::fs::write(path, contents)
}

/// `program` itself when it contains a separator, else the first executable of that name on `PATH`.
pub fn which(program: impl AsRef<Path>) -> Option<PathBuf> {
    let program = program.as_ref();
    if program.components().count() > 1 {
        return is_executable(program).then(|| program.to_owned());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| {
            let file = dir.join(program);
            if cfg!(windows) { ["exe", "cmd", "bat"].map(|ext| file.with_extension(ext)).to_vec() } else { vec![file] }
        })
        .find(|file| is_executable(file))
}

/// A regular file this user may execute.
pub fn is_executable(file: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    file.is_file()
}

/// The first dotted version in `text`, optionally after `v`, with any `-pre`/`+build` suffix.
pub fn parse_version(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let word_start = |i: usize| i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
    (0..bytes.len())
        .filter(|&i| bytes[i].is_ascii_digit() && (word_start(i) || (bytes[i - 1] == b'v' && word_start(i - 1))))
        .find_map(|start| {
            let rest = &text[start..];
            let core = rest.bytes().take_while(|b| b.is_ascii_digit() || *b == b'.').count();
            let numbers = rest[..core].trim_end_matches('.');
            if !numbers.contains('.') {
                return None;
            }
            let tail = &rest[core..];
            if numbers.len() == core && tail.starts_with(['-', '+']) {
                let suffix = tail.bytes().take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'+' | b'.')).count();
                return Some(rest[..core + suffix].trim_end_matches('.').to_owned());
            }
            Some(numbers.to_owned())
        })
}

/// Runs `exe args` and returns its stdout; errors name the command and its first stderr line.
pub async fn probe_cli(exe: &Path, args: &[&str], timeout: Duration) -> io::Result<String> {
    let shown = exe.display();
    match tokio::time::timeout(timeout, output(Command::new(exe).args(args))).await {
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, format!("`{shown}` didn't answer {}", args.join(" ")))),
        Ok(Err(err)) => Err(io::Error::new(err.kind(), format!("`{shown}` didn't run: {err}"))),
        Ok(Ok(out)) if !out.status.success() => {
            Err(io::Error::other(format!("`{shown} {}` failed ({}): {}", args.join(" "), out.status, first_stderr(&out.stderr))))
        }
        Ok(Ok(out)) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
    }
}

/// Runs `cmd` to completion: stdout on success, else the exit status and first stderr line.
pub async fn run_checked(mut cmd: Command) -> Result<String, String> {
    let out = output(&mut cmd).await.map_err(|err| err.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    out.status.success().then_some(stdout).ok_or_else(|| format!("{}: {}", out.status, first_stderr(&out.stderr)))
}

async fn output(cmd: &mut Command) -> io::Result<std::process::Output> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    retry_busy(|| cmd.spawn())?.wait_with_output().await
}

fn first_stderr(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("no output").to_owned()
}

/// The last lines a child wrote to a stream, kept in a bounded ring by a reader task.
#[derive(Clone, Default, Debug)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    /// Drains `r` on a tokio task, keeping its last `lines` lines.
    pub fn spawn(r: impl AsyncRead + Unpin + Send + 'static, lines: usize) -> Self {
        let tail = Self::default();
        let ring = tail.0.clone();
        tokio::spawn(async move {
            let (mut reader, mut line) = (BufReader::new(r), Vec::new());
            while reader.read_until(b'\n', &mut line).await.is_ok_and(|n| n > 0) {
                let mut ring = ring.locked();
                if ring.len() >= lines.max(1) {
                    ring.pop_front();
                }
                ring.push_back(String::from_utf8_lossy(&line).trim_end().to_owned());
                line.clear();
            }
        });
        tail
    }

    pub fn joined(&self) -> String { self.0.locked().iter().map(String::as_str).collect::<Vec<_>>().join("\n") }
}

/// `s` single-quoted for a POSIX shell.
pub fn shell_quote(s: &str) -> String { format!("'{}'", s.replace('\'', r"'\''")) }

/// Whether this process may read and write `/dev/kvm`.
pub fn kvm_available() -> bool { std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok() }

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn versions_quoting_and_running_programs() {
        let versions = [("omp/19.0.0\n", Some("19.0.0")), ("codex-cli 0.46.0", Some("0.46.0")), ("2.0.14 (Claude Code)", Some("2.0.14")),
            ("opencode 1.2.3-beta.1+build", Some("1.2.3-beta.1+build")), ("v1.2.3, built", Some("1.2.3")), ("gemini 0.9.", Some("0.9")), ("x86 build 7", None)];
        versions.iter().for_each(|&(text, want)| assert_eq!(parse_version(text).as_deref(), want, "{text}"));
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        let dir = tempfile::tempdir().unwrap();
        let (ok, bad) = (dir.path().join("ok tool"), dir.path().join("bad"));
        write_executable(&ok, "#!/bin/sh\necho \"v$1\"\n").unwrap();
        write_executable(&bad, "#!/bin/sh\necho 'bad flag' >&2\nexit 3\n").unwrap();
        assert_eq!(which(&ok).as_deref(), Some(ok.as_path()));
        assert!(which("loa-no-such-program").is_none() && which("sh").is_some());
        assert_eq!(probe_cli(&ok, &["1.2"], Duration::from_secs(5)).await.unwrap(), "v1.2\n");
        assert!(probe_cli(&bad, &[], Duration::from_secs(5)).await.unwrap_err().to_string().contains("bad flag"));
        assert!(run_checked(Command::new(&bad)).await.unwrap_err().contains("bad flag"));
        let tail = StderrTail::spawn(&b"a\nb\xff\nc"[..], 2);
        let filled = async { while !tail.joined().ends_with('c') { tokio::task::yield_now().await } };
        tokio::time::timeout(Duration::from_secs(5), filled).await.unwrap();
        assert_eq!(tail.joined(), "b\u{fffd}\nc");
    }
}
