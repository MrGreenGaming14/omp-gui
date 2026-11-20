//! Whether a harness can run on this machine.
//!
//! [`Adapter::availability`](crate::Adapter::availability) reports it for the
//! harness picker: unavailable harnesses are listed with the reason and can't
//! be picked. [`probe_cli`] covers harnesses run from an executable.

use std::path::{Path, PathBuf};
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

pub use loa_util::exec::{parse_version, which as find_executable};

/// Finds `program` and runs it with `version_args`: ready with the reported
/// version when it exits successfully, `NotInstalled` when it isn't found,
/// `Failed` when it doesn't run, answer within [`PROBE_TIMEOUT`] or succeed.
pub async fn probe_cli(program: &Path, version_args: &[&str]) -> Availability {
    let Some(executable) = find_executable(program) else {
        let reason = format!("`{}` not found", program.display());
        return Availability::unavailable(AvailabilityStatus::NotInstalled, reason);
    };
    match loa_util::exec::probe_cli(&executable, version_args, PROBE_TIMEOUT).await {
        Ok(stdout) => Availability::ready(Some(executable), parse_version(&stdout)),
        Err(err) => Availability { executable: Some(executable), ..Availability::unavailable(AvailabilityStatus::Failed, err.to_string()) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_reports_missing_and_installed_executables() {
        let missing = probe_cli(Path::new("loa-no-such-harness"), &["--version"]).await;
        assert_eq!(missing.status, AvailabilityStatus::NotInstalled);
        assert!(!missing.is_ready());

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-harness");
        loa_util::exec::write_executable(&script, "#!/bin/sh\necho \"fake-harness 4.5.6\"\n").unwrap();
        let ready = probe_cli(&script, &["--version"]).await;
        assert!(ready.is_ready(), "{ready:?}");
        assert_eq!(ready.version.as_deref(), Some("4.5.6"));
        assert_eq!(ready.executable.as_deref(), Some(script.as_path()));

        let broken = dir.path().join("broken-harness");
        loa_util::exec::write_executable(&broken, "#!/bin/sh\necho 'bad flag' >&2\nexit 3\n").unwrap();
        let failed = probe_cli(&broken, &["--version"]).await;
        assert_eq!(failed.status, AvailabilityStatus::Failed);
        assert!(failed.detail.unwrap().contains("bad flag"));
    }
}
