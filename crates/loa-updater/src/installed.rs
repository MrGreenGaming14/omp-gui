//! Finds a product's executable and asks it for its version.

use semver::Version;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// `name` resolved against `PATH` when it is a bare name; paths are kept as
/// they are when they exist.
pub fn find_executable(name: &Path) -> Option<PathBuf> {
    if name.components().count() > 1 {
        return is_executable(name).then(|| name.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Runs `<executable> --version` and reads the first semantic version in
/// its output: `omp/19.0.0`, `2.0.14 (Claude Code)`, `codex-cli 0.46.0`.
pub async fn executable_version(executable: &Path, timeout: Duration) -> Result<Version, String> {
    let child = loa_util::exec::retry_busy(|| {
        Command::new(executable)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
    })
    .map_err(|e| format!("cannot start {}: {e}", executable.display()))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("{} --version did not finish within {timeout:?}", executable.display()))?
        .map_err(|e| format!("running {} --version: {e}", executable.display()))?;
    if !output.status.success() {
        return Err(format!("{} --version exited with {}", executable.display(), output.status));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_version(&text).ok_or_else(|| format!("no version in {:?}", text.trim()))
}

/// The first token of `text` that parses as a semantic version, after
/// splitting on whitespace and `/` and dropping a leading `v`.
pub fn parse_version(text: &str) -> Option<Version> {
    text.split(|c: char| c.is_whitespace() || c == '/' || c == '(' || c == ')' || c == ',')
        .map(|token| token.strip_prefix('v').unwrap_or(token))
        .find_map(|token| Version::parse(token).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_from_harness_banners() {
        let v = |s: &str| parse_version(s).map(|v| v.to_string());
        assert_eq!(v("omp/19.0.0\n").as_deref(), Some("19.0.0"));
        assert_eq!(v("2.0.14 (Claude Code)").as_deref(), Some("2.0.14"));
        assert_eq!(v("codex-cli 0.46.0").as_deref(), Some("0.46.0"));
        assert_eq!(v("opencode v1.2.3-beta.1").as_deref(), Some("1.2.3-beta.1"));
        assert_eq!(v("no version here"), None);
    }

    #[test]
    fn bare_names_resolve_on_path_and_paths_must_exist() {
        let sh = find_executable(Path::new("sh")).expect("sh on PATH");
        assert!(sh.is_absolute());
        assert_eq!(find_executable(&sh), Some(sh.clone()));
        assert_eq!(find_executable(Path::new("/nonexistent/loa-updater-test")), None);
        assert_eq!(find_executable(Path::new("loa-updater-surely-missing")), None);
    }

    /// A staged binary still open for writing (as in a child forked while it
    /// was being written) is busy; asking its version waits until it closes.
    #[cfg(unix)]
    #[tokio::test]
    async fn version_of_a_busy_staged_binary_is_read_once_the_writer_closes() {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omp");
        let mut writer = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o755).open(&path).unwrap();
        writer.write_all(b"#!/bin/sh\necho omp/19.2.0\n").unwrap();
        let busy = std::process::Command::new(&path).output().unwrap_err();
        assert_eq!(busy.kind(), std::io::ErrorKind::ExecutableFileBusy, "{busy}");

        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            drop(writer);
        });
        let version = executable_version(&path, Duration::from_secs(10)).await.unwrap();
        closer.join().unwrap();
        assert_eq!(version.to_string(), "19.2.0");
    }
}
