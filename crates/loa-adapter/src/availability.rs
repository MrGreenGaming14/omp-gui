//! Whether a harness can run on this machine.
//!
//! [`Adapter::availability`](crate::Adapter::availability) reports it for the
//! harness picker: unavailable harnesses are listed with the reason and can't
//! be picked. [`probe_cli`] covers harnesses run from an executable.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// How long [`probe_cli`] waits for `--version` to answer.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AvailabilityStatus {
    /// Sessions can start.
    Ready,
    /// The executable isn't installed.
    NotInstalled,
    /// The harness runs as a server and none answers.
    Unreachable,
    /// Installed, but the version or protocol can't be driven.
    Incompatible,
    /// Probing failed (the executable didn't run or answer).
    Failed,
    /// The adapter doesn't report availability.
    Unknown,
}

/// What [`Adapter::availability`](crate::Adapter::availability) found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Availability {
    pub status: AvailabilityStatus,
    /// Short note for the picker: the reason when not ready, else the version
    /// or the server address.
    pub detail: Option<String>,
    /// The executable found, for `SessionConfig::executable`.
    pub executable: Option<PathBuf>,
    /// Version the harness reported, e.g. `19.0.0`.
    pub version: Option<String>,
}

impl Availability {
    pub fn ready(executable: Option<PathBuf>, version: Option<String>) -> Self {
        let detail = version.clone();
        Self { status: AvailabilityStatus::Ready, detail, executable, version }
    }

    /// Not ready for `reason`.
    pub fn unavailable(status: AvailabilityStatus, reason: impl Into<String>) -> Self {
        Self { status, detail: Some(reason.into()), executable: None, version: None }
    }

    pub fn is_ready(&self) -> bool {
        self.status == AvailabilityStatus::Ready
    }
}

/// `program` as a path when it names one (contains a separator), else the
/// first executable with that name on `PATH`; on Windows also tried with each
/// `PATHEXT` extension. Thin wrapper over [`loa_exec::find_executable`].
pub fn find_executable(program: &Path) -> Option<PathBuf> {
    loa_exec::find_executable(program)
}

/// Finds `program` and runs it with `version_args`: ready with the reported
/// version when it exits successfully, `NotInstalled` when it isn't found,
/// `Failed` when it doesn't run, answer within [`PROBE_TIMEOUT`] or succeed.
pub async fn probe_cli(program: &Path, version_args: &[&str]) -> Availability {
    let Some(executable) = find_executable(program) else {
        let reason = format!("`{}` not found", program.display());
        return Availability::unavailable(AvailabilityStatus::NotInstalled, reason);
    };
    let mut command = tokio::process::Command::new(&executable);
    command.args(version_args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let failed = |reason: String| Availability { executable: Some(executable.clone()), ..Availability::unavailable(AvailabilityStatus::Failed, reason) };
    let output = match tokio::time::timeout(PROBE_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return failed(format!("`{}` didn't run: {error}", executable.display())),
        Err(_) => return failed(format!("`{}` didn't answer {}", executable.display(), version_args.join(" "))),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first = stderr.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("no output");
        return failed(format!("`{} {}` failed ({}): {first}", executable.display(), version_args.join(" "), output.status));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Availability::ready(Some(executable), parse_version(&stdout))
}

/// The first version number in `text`: digits and dots with at least one dot,
/// optionally after a `v`, plus a `-` pre-release or `+` build suffix
/// (`omp/19.0.0` gives `19.0.0`, `codex-cli 0.46.0` gives `0.46.0`).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions_from_cli_banners() {
        assert_eq!(parse_version("omp/19.0.0\n").as_deref(), Some("19.0.0"));
        assert_eq!(parse_version("codex-cli 0.46.0").as_deref(), Some("0.46.0"));
        assert_eq!(parse_version("2.0.14 (Claude Code)").as_deref(), Some("2.0.14"));
        assert_eq!(parse_version("opencode 1.2.3-beta.1+build").as_deref(), Some("1.2.3-beta.1+build"));
        assert_eq!(parse_version("v1.2.3, built today").as_deref(), Some("1.2.3"));
        assert_eq!(parse_version("gemini 0.9.").as_deref(), Some("0.9"));
        assert_eq!(parse_version("x86 build 7"), None);
    }

    #[tokio::test]
    async fn probe_reports_missing_and_installed_executables() {
        let missing = probe_cli(Path::new("loa-no-such-harness"), &["--version"]).await;
        assert_eq!(missing.status, AvailabilityStatus::NotInstalled);
        assert!(!missing.is_ready());

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-harness");
        loa_exec::write_executable(&script, "#!/bin/sh\necho \"fake-harness 4.5.6\"\n").unwrap();
        let ready = probe_cli(&script, &["--version"]).await;
        assert!(ready.is_ready(), "{ready:?}");
        assert_eq!(ready.version.as_deref(), Some("4.5.6"));
        assert_eq!(ready.executable.as_deref(), Some(loa_exec::resolve(&script).as_path()));

        let broken = dir.path().join("broken-harness");
        loa_exec::write_executable(&broken, "#!/bin/sh\necho 'bad flag' >&2\nexit 3\n").unwrap();
        let failed = probe_cli(&broken, &["--version"]).await;
        assert_eq!(failed.status, AvailabilityStatus::Failed);
        assert!(failed.detail.unwrap().contains("bad flag"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn probe_finds_and_runs_a_cmd_shim_on_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fake-npm-cli.cmd"), "@echo off\r\necho fake-npm-cli 7.8.9\r\n").unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        let found = loa_exec::find_executable_in(Path::new("fake-npm-cli"), Some(&path), None).unwrap();
        assert_eq!(found, dir.path().join("fake-npm-cli.cmd"));
        let ready = probe_cli(&found, &["--version"]).await;
        assert!(ready.is_ready(), "{ready:?}");
        assert_eq!(ready.version.as_deref(), Some("7.8.9"));
    }

    #[test]
    fn missing_program_stays_unresolved() {
        assert_eq!(find_executable(Path::new("loa-no-such-harness")), None);
    }
}
