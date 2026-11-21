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

    pub fn running(&self) -> usize { self.entries.iter().filter(|entry| entry.state == RunState::Running).count() }

    /// Records that the run's patch was applied (with
    /// [`crate::isolation::apply_patch`]) to its workspace.
    pub fn mark_applied(&mut self, run: u64) {
        if let Some(RunEntry { state: RunState::Finished { applied, .. }, .. }) = self.get_mut(run) {
            *applied = true;
        }
    }
}
