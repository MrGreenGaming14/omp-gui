//! The `sandbox_task` runs the app lists, built from [`SandboxEvent`]s.

use crate::isolation::{PatchFile, patch_files};
use crate::tool::{SandboxEvent, SandboxRun};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    Running,
    Finished {
        final_message: Option<String>,
        /// The isolated run's patch; `None` for runs on the workspace itself.
        patch: Option<String>,
        /// The files the patch touches.
        files: Vec<PatchFile>,
        /// Set once the patch was applied to the workspace.
        applied: bool,
    },
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunEntry {
    pub run: SandboxRun,
    pub state: RunState,
}

impl RunEntry {
    /// A patch that can still be applied.
    pub fn pending_patch(&self) -> Option<&str> {
        match &self.state {
            RunState::Finished { patch: Some(patch), applied: false, .. } if !patch.is_empty() => Some(patch),
            _ => None,
        }
    }
}

/// Runs in the order they started.
#[derive(Clone, Debug, Default)]
pub struct RunLog {
    entries: Vec<RunEntry>,
}

impl RunLog {
    pub fn record(&mut self, event: SandboxEvent) {
        match event {
            SandboxEvent::Started(run) => self.entries.push(RunEntry { run, state: RunState::Running }),
            SandboxEvent::Finished { run, final_message, patch } => {
                if let Some(entry) = self.get_mut(run) {
                    let files = patch.as_deref().map(patch_files).unwrap_or_default();
                    entry.state = RunState::Finished { final_message, patch, files, applied: false };
                }
            }
            SandboxEvent::Failed { run, error } => {
                if let Some(entry) = self.get_mut(run) {
                    entry.state = RunState::Failed(error);
                }
            }
        }
    }

    pub fn entries(&self) -> &[RunEntry] { &self.entries }

    pub fn get(&self, run: u64) -> Option<&RunEntry> { self.entries.iter().find(|entry| entry.run.run == run) }

    fn get_mut(&mut self, run: u64) -> Option<&mut RunEntry> { self.entries.iter_mut().find(|entry| entry.run.run == run) }

    /// Runs of one session.
    pub fn of_session<'a>(&'a self, session: &'a str) -> impl Iterator<Item = &'a RunEntry> + 'a {
        self.entries.iter().filter(move |entry| entry.run.parent == session)
    }

    pub fn running(&self) -> usize { self.entries.iter().filter(|entry| entry.state == RunState::Running).count() }

    /// Records that the run's patch was applied (with
    /// [`crate::isolation::apply_patch`]) to its workspace.
    pub fn mark_applied(&mut self, run: u64) {
        if let Some(RunEntry { state: RunState::Finished { applied, .. }, .. }) = self.get_mut(run) {
            *applied = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::SandboxKind;

    fn started(run: u64, parent: &str) -> SandboxEvent {
        SandboxEvent::Started(SandboxRun {
            run,
            parent: parent.into(),
            worker: format!("{parent}-sandbox-{run}"),
            kind: SandboxKind::Local,
            task: "work".into(),
            isolated: true,
            workspace: "/repo".into(),
        })
    }

    #[test]
    fn follows_runs_from_start_to_end() {
        let mut log = RunLog::default();
        log.record(started(1, "a"));
        log.record(started(2, "b"));
        assert_eq!(log.running(), 2);
        let patch = "diff --git a/x b/x\nnew file mode 100644\n--- /dev/null\n+++ b/x\n@@ -0,0 +1 @@\n+hi\n";
        log.record(SandboxEvent::Finished { run: 1, final_message: Some("done".into()), patch: Some(patch.into()) });
        log.record(SandboxEvent::Failed { run: 2, error: "docker: gone".into() });
        assert_eq!(log.running(), 0);
        let first = log.get(1).unwrap();
        assert_eq!(first.pending_patch(), Some(patch));
        let RunState::Finished { files, .. } = &first.state else { panic!() };
        assert_eq!(files, &[PatchFile { path: "x".into(), added: 1, removed: 0, binary: false }]);
        assert_eq!(log.get(2).unwrap().state, RunState::Failed("docker: gone".into()));
        assert_eq!(log.of_session("b").count(), 1);
        log.mark_applied(1);
        assert_eq!(log.get(1).unwrap().pending_patch(), None);
    }
}
