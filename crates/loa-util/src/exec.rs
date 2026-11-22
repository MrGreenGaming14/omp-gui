//! Finding, writing and running executables. Children forked by other threads inherit
//! writable descriptors, so fresh files can hit `ETXTBSY`; these helpers avoid or retry it.

use std::collections::VecDeque;
use std::ffi::OsStr;
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

/// Writes a shell-script fixture and a Windows launcher next to it.
///
/// Callers spawn [`resolve`]`(path)`, which finds the `.cmd` launcher through
/// `PATHEXT`. The launcher uses Bash from Git for Windows; a checkout's CRLF
/// line endings must not reach Bash.
#[cfg(windows)]
pub fn write_executable(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let sidecar = |suffix: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    let contents = contents.as_ref();
    let mut script = Vec::with_capacity(contents.len());
    for (i, &byte) in contents.iter().enumerate() {
        if byte != b'\r' || contents.get(i + 1) != Some(&b'\n') {
            script.push(byte);
        }
    }
    std::fs::write(sidecar(".sh"), script)?;
    std::fs::write(sidecar(".cmd"), b"@bash.exe --noprofile --norc \"%~dpn0.sh\" %*\r\n")
}

/// Resolves `program` to an executable file, like a shell would.
///
/// A bare name is searched in every directory of `PATH`; a path with
/// separators is checked as given. On Windows the name is also tried with
/// each `PATHEXT` extension (default `.COM;.EXE;.BAT;.CMD`) appended, never
/// substituted, so `omp-1.2` stays intact. See [`which_in`].
pub fn which(program: impl AsRef<Path>) -> Option<PathBuf> {
    which_in(program.as_ref(), std::env::var_os("PATH").as_deref(), std::env::var_os("PATHEXT").as_deref())
}

/// The path to spawn for `program`: what [`which`] resolves, else `program`
/// unchanged so the spawn fails with its usual "not found" error. Spawning
/// the resolved path is what makes npm `.cmd` shims run on Windows.
pub fn resolve(program: &Path) -> PathBuf { which(program).unwrap_or_else(|| program.to_path_buf()) }

/// Testable core of [`which`] with the `PATH` and `PATHEXT` values passed in.
/// `pathext` is ignored outside Windows.
pub fn which_in(program: &Path, path_var: Option<&OsStr>, pathext: Option<&OsStr>) -> Option<PathBuf> {
    if program.as_os_str().is_empty() {
        return None;
    }
    if program.components().count() > 1 || program.has_root() {
        return first_executable(program, pathext, false);
    }
    std::env::split_paths(path_var?)
        .filter(|dir| !dir.as_os_str().is_empty())
        .find_map(|dir| first_executable(&dir.join(program), pathext, true))
}

/// Candidates for `base`: itself, then (Windows) with each extension appended.
/// `searched` (bare-name search) tries `base` only when it already has an
/// extension; an explicit path is always tried as given, and gets extensions
/// appended only when it has none.
#[cfg_attr(not(windows), allow(unused_variables))]
fn first_executable(base: &Path, pathext: Option<&OsStr>, searched: bool) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let has_ext = base.extension().is_some();
        if (has_ext || !searched) && is_executable(base) {
            return Some(base.to_path_buf());
        }
        if searched || !has_ext {
            let exts = pathext
                .map(|p| p.to_string_lossy().into_owned())
                .filter(|p| !p.trim().is_empty())
                .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_owned());
            for ext in exts.split(';').map(str::trim).filter(|e| !e.is_empty()) {
                let mut name = base.as_os_str().to_owned();
                if !ext.starts_with('.') {
                    name.push(".");
                }
                // PATHEXT is upper case; files are almost always lower case
                // and Windows matches names case-insensitively, so report
                // the path the way the file is usually spelled.
                name.push(ext.to_ascii_lowercase());
                let candidate = PathBuf::from(name);
                if is_executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        is_executable(base).then(|| base.to_path_buf())
    }
}

/// Whether `file` names an executable file. Unix: a regular file with any
/// execute bit set. Windows has no execute bit, so any existing regular file
/// counts; [`which`] decides by extension via `PATHEXT`.
pub fn is_executable(file: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    file.is_file()
}

/// Windows `CREATE_NO_WINDOW`: the child gets a console that is never shown.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Starts a command without a console window of its own. The app runs without a console on Windows, so every
/// console program it starts would otherwise open a new window; the child's own children share its hidden one.
/// Does nothing elsewhere. Wrapped commands (`process_wrap`'s `JobObject` replaces the creation flags) set
/// [`CREATE_NO_WINDOW`] through its `CreationFlags` instead.
pub trait NoWindow {
    fn no_window(&mut self) -> &mut Self;
}

impl NoWindow for Command {
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        self.creation_flags(CREATE_NO_WINDOW);
        self
    }
}

impl NoWindow for std::process::Command {
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(self, CREATE_NO_WINDOW);
        self
    }
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

/// Runs a command with stdin closed, capturing both output streams.
pub async fn output(cmd: &mut Command) -> io::Result<std::process::Output> {
    cmd.no_window().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    retry_busy(|| cmd.spawn())?.wait_with_output().await
}

fn first_stderr(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("no output").to_owned()
}

/// The last lines a child wrote to a stream, kept in a bounded ring by a reader task.
#[derive(Clone, Debug)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>, tokio::sync::watch::Receiver<bool>);

impl StderrTail {
    /// Drains `r` on a tokio task, keeping its last `lines` lines.
    pub fn spawn(r: impl AsyncRead + Unpin + Send + 'static, lines: usize) -> Self {
        let (done, closed) = tokio::sync::watch::channel(false);
        let tail = Self(Arc::default(), closed);
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
            done.send_replace(true);
        });
        tail
    }

    pub fn joined(&self) -> String { self.0.locked().iter().map(String::as_str).collect::<Vec<_>>().join("\n") }

    /// Resolves once the stream reached its end.
    pub async fn closed(&self) { let _ = self.1.clone().wait_for(|done| *done).await; }
}

/// `s` single-quoted for a POSIX shell.
pub fn shell_quote(s: &str) -> String { format!("'{}'", s.replace('\'', r"'\''")) }

/// Whether this process may read and write `/dev/kvm`.
pub fn kvm_available() -> bool { std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok() }

#[cfg(test)]
mod find_tests {
    use super::*;
    use std::ffi::OsString;

    fn make(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn path_of(dirs: &[&Path]) -> OsString { std::env::join_paths(dirs).unwrap() }

    /// `name` as a launchable file: unchanged on Unix, with `.exe` on Windows,
    /// where a file without a PATHEXT extension isn't an executable.
    fn exe(name: &str) -> String { if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() } }

    /// Paths compared the way the filesystem does: Windows matches names
    /// case-insensitively, so `tool.CMD` from PATHEXT is the file `tool.cmd`.
    fn norm(path: Option<PathBuf>) -> Option<String> {
        path.map(|p| {
            let text = p.to_string_lossy().into_owned();
            if cfg!(windows) { text.to_lowercase() } else { text }
        })
    }

    #[test]
    fn is_executable_rejects_missing_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_executable(&dir.path().join("nope")));
        assert!(!is_executable(dir.path()));
        assert!(is_executable(&make(dir.path(), "tool.exe")));
    }

    #[cfg(unix)]
    #[test]
    fn is_executable_needs_exec_bit_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = make(dir.path(), "tool");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable(&file));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o100)).unwrap();
        assert!(is_executable(&file));
    }

    #[test]
    fn bare_name_found_on_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let want = make(b.path(), &exe("omp-tool"));
        let var = path_of(&[a.path(), b.path()]);
        assert_eq!(norm(which_in(Path::new("omp-tool"), Some(&var), None)), norm(Some(want)));
        assert_eq!(which_in(Path::new("missing"), Some(&var), None), None);
        assert_eq!(which_in(Path::new("omp-tool"), None, None), None);
    }

    #[test]
    fn first_path_entry_wins() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let want = make(a.path(), &exe("dup"));
        make(b.path(), &exe("dup"));
        let var = path_of(&[a.path(), b.path()]);
        assert_eq!(norm(which_in(Path::new("dup"), Some(&var), None)), norm(Some(want)));
    }

    #[test]
    fn path_with_separator_is_not_searched() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let want = make(&sub, "tool");
        let var = path_of(&[dir.path()]);
        assert_eq!(which_in(&want, Some(&var), None), Some(want.clone()));
        // "sub/tool" relative to PATH dir must not be resolved via PATH.
        assert_eq!(which_in(Path::new("sub/tool"), Some(&var), None), None);
        assert_eq!(which_in(&sub.join("absent"), Some(&var), None), None);
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_file_skipped_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let plain = make(a.path(), "tool");
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        let want = make(b.path(), "tool");
        let var = path_of(&[a.path(), b.path()]);
        assert_eq!(which_in(Path::new("tool"), Some(&var), None), Some(want));
    }

    #[cfg(unix)]
    #[test]
    fn pathext_ignored_on_unix() {
        let dir = tempfile::tempdir().unwrap();
        make(dir.path(), "tool.exe");
        let var = path_of(&[dir.path()]);
        let ext = OsString::from(".EXE");
        assert_eq!(which_in(Path::new("tool"), Some(&var), Some(&ext)), None);
    }

    #[cfg(windows)]
    #[test]
    fn pathext_appended_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let want = make(dir.path(), "tool.cmd");
        let var = path_of(&[dir.path()]);
        let ext = OsString::from(".EXE;.CMD");
        assert_eq!(norm(which_in(Path::new("tool"), Some(&var), Some(&ext))), norm(Some(want)));
        // Default PATHEXT when unset.
        assert!(which_in(Path::new("tool"), Some(&var), None).is_some());
        // Extension not in PATHEXT is not found.
        let only_exe = OsString::from(".EXE");
        assert_eq!(which_in(Path::new("tool"), Some(&var), Some(&only_exe)), None);
    }

    #[cfg(windows)]
    #[test]
    fn dotted_names_keep_their_suffix_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let want = make(dir.path(), "omp-1.2.exe");
        make(dir.path(), "omp-1.exe");
        let var = path_of(&[dir.path()]);
        let ext = OsString::from(".EXE");
        assert_eq!(norm(which_in(Path::new("omp-1.2"), Some(&var), Some(&ext))), norm(Some(want)));
        // A name that exists as-is with an extension wins over appending.
        let exact = make(dir.path(), "run.bat");
        assert_eq!(norm(which_in(Path::new("run.bat"), Some(&var), Some(&ext))), norm(Some(exact)));
    }

    #[cfg(windows)]
    #[test]
    fn explicit_path_gets_pathext_when_extensionless_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let want = make(dir.path(), "tool.exe");
        let ext = OsString::from(".EXE");
        assert_eq!(norm(which_in(&dir.path().join("tool"), None, Some(&ext))), norm(Some(want)));
    }

    #[test]
    fn resolve_keeps_an_unfound_program_as_given() {
        assert_eq!(resolve(Path::new("loa-surely-missing-program")), PathBuf::from("loa-surely-missing-program"));
        let dir = tempfile::tempdir().unwrap();
        let tool = make(dir.path(), "tool.exe");
        assert_eq!(resolve(&tool), tool);
    }
}

#[cfg(all(test, windows))]
#[test]
fn windows_script_fixture_preserves_args_streams_and_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fake tool");
    write_executable(&script, "#!/usr/bin/env bash\r\nprintf '%s' \"$1\"\r\nprintf 'diagnostic' >&2\r\nexit 3\r\n").unwrap();
    let result = std::process::Command::new(resolve(&script)).arg("two words").output().unwrap();
    assert_eq!(result.stdout, b"two words");
    assert_eq!(result.stderr, b"diagnostic");
    assert_eq!(result.status.code(), Some(3));
}

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
        tokio::time::timeout(Duration::from_secs(5), tail.closed()).await.unwrap();
        assert_eq!(tail.joined(), "b\u{fffd}\nc");
    }

    #[test]
    fn retry_busy_passes_other_errors_through() {
        let dir = tempfile::tempdir().unwrap();
        let (mut attempts, missing) = (0, dir.path().join("missing"));
        let err = retry_busy(|| { attempts += 1; std::process::Command::new(&missing).output() }).unwrap_err();
        assert!(err.kind() == io::ErrorKind::NotFound && attempts == 1);
    }
}
