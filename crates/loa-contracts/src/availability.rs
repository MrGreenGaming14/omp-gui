//! Whether a harness can run on this machine, for the harness picker.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AvailabilityStatus {
    Ready,
    NotInstalled,
    /// A server harness that doesn't answer.
    Unreachable,
    Incompatible,
    /// The executable didn't run or answer.
    Failed,
    Unknown,
}

/// What `Adapter::availability` found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Availability {
    pub status: AvailabilityStatus,
    /// Short picker note: the reason when not ready, else the version or address.
    pub detail: Option<String>,
    /// The executable found, for `SessionConfig::executable`.
    pub executable: Option<PathBuf>,
    pub version: Option<String>,
}

impl Availability {
    /// How long [`Availability::probe`] waits for `--version`.
    pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

    pub fn ready(executable: Option<PathBuf>, version: Option<String>) -> Self {
        Self { status: AvailabilityStatus::Ready, detail: version.clone(), executable, version }
    }

    pub fn unavailable(status: AvailabilityStatus, reason: impl Into<String>) -> Self {
        Self { status, detail: Some(reason.into()), executable: None, version: None }
    }

    pub fn is_ready(&self) -> bool { self.status == AvailabilityStatus::Ready }

    /// Classifies a `loa_util::exec` probe of `program version_args`.
    pub async fn probe(program: &Path, version_args: &[&str]) -> Self {
        use loa_util::exec;
        let Some(exe) = exec::which(program) else {
            return Self::unavailable(AvailabilityStatus::NotInstalled, format!("`{}` not found", program.display()));
        };
        match exec::probe_cli(&exe, version_args, Self::PROBE_TIMEOUT).await {
            Ok(stdout) => Self::ready(Some(exe), exec::parse_version(&stdout)),
            Err(err) => Self { executable: Some(exe), ..Self::unavailable(AvailabilityStatus::Failed, err.to_string()) },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_reports_missing_ready_and_failing_executables() {
        let dir = tempfile::tempdir().unwrap();
        let (ok, bad) = (dir.path().join("ok"), dir.path().join("bad"));
        loa_util::exec::write_executable(&ok, "#!/bin/sh\necho \"fake 4.5.6\"\n").unwrap();
        loa_util::exec::write_executable(&bad, "#!/bin/sh\necho 'bad flag' >&2\nexit 3\n").unwrap();
        assert_eq!(Availability::probe(Path::new("loa-no-such-harness"), &[]).await.status, AvailabilityStatus::NotInstalled);
        let ready = Availability::probe(&ok, &["--version"]).await;
        assert_eq!((ready.version.as_deref(), ready.executable.as_deref()), (Some("4.5.6"), Some(ok.as_path())));
        let failed = Availability::probe(&bad, &["--version"]).await;
        assert_eq!(failed.status, AvailabilityStatus::Failed);
        assert!(failed.detail.unwrap().contains("bad flag"));
    }
}
