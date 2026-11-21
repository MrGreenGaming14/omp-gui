//! Finds a product's executable and asks it for its version.

use semver::Version;
use std::path::Path;
use std::time::Duration;

/// `name` resolved against `PATH` when it is a bare name; paths are kept as
/// they are when they exist.
pub use loa_util::exec::which as find_executable;

/// Runs `<executable> --version` and reads the first semantic version in
/// its output: `omp/19.0.0`, `2.0.14 (Claude Code)`, `codex-cli 0.46.0`.
pub async fn executable_version(executable: &Path, timeout: Duration) -> Result<Version, String> {
    let text = loa_util::exec::probe_cli(executable, &["--version"], timeout).await.map_err(|e| e.to_string())?;
    parse_version(&text).ok_or_else(|| format!("no version in {:?}", text.trim()))
}

/// The first semantic version in `text`, after an optional leading `v`.
pub fn parse_version(text: &str) -> Option<Version> {
    Version::parse(&loa_util::exec::parse_version(text)?).ok()
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
