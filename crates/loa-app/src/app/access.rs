//! The permission picker: omp's approval modes for the active session.

use super::*;

/// The permission picker's choices: omp's approval modes. Sessions start on
/// [`ACCESS_DEFAULT`] until another is picked (the adapter's own default).
const ACCESS: [(&str, &str, &str); 4] = [
    ("always-ask", "Ask first", "Reads run; edits and commands ask"),
    ("always-ask-cached", "Ask and remember", "Asks before file reads and writes, remembers answers"),
    ("write", "Auto-edit", "Reads and edits run; commands ask"),
    ("yolo", "Full access", "Everything runs without asking"),
];
const ACCESS_DEFAULT: &str = "always-ask";

impl App {
    /// Shows the permission picker for omp sessions, with the current level.
    pub(super) fn sync_access(&mut self) {
        let harness = self.ctx.sessions.active().and_then(|session| session.harness_id()).or(self.ctx.sessions.harness.as_deref());
        let omp = harness == Some("omp");
        let level = self.access.as_deref();
        if self.access_synced.as_ref().is_some_and(|(synced_omp, synced_level)| *synced_omp == omp && synced_level.as_deref() == level) {
            return;
        }
        self.access_synced = Some((omp, self.access.clone()));
        if !omp {
            self.ctx.composer.set_access(Vec::new(), None);
            return;
        }
        let options = ACCESS
            .iter()
            .map(|(id, label, detail)| composer::AccessOption { id: (*id).to_owned(), label: (*label).to_owned(), detail: (*detail).to_owned() })
            .collect();
        self.ctx.composer.set_access(options, Some(level.unwrap_or(ACCESS_DEFAULT)));
    }

    /// A permission level was picked: new sessions start on it, and the
    /// open session restarts on it (resuming its conversation), now when
    /// it is idle, else once its turn ends.
    pub(super) fn set_access(&mut self, id: String) -> Task<Message> {
        let access = Some(id.clone());
        if access.as_deref().unwrap_or(ACCESS_DEFAULT) == self.access.as_deref().unwrap_or(ACCESS_DEFAULT) {
            return Task::none();
        }
        self.access = access.clone();
        self.save_ui(|config| config.harness.approval_mode = access);
        let label = ACCESS.iter().find(|(each, _, _)| *each == id).map_or("", |(_, label, _)| *label);
        let running = self.ctx.sessions.active().is_some_and(|session| session.harness().is_some());
        if running && self.ctx.sessions.active().is_some_and(|session| session.is_busy()) {
            self.restart_for_access = true;
            self.ctx.notify(format!("{label}: applies once this turn ends"));
            return Task::none();
        }
        self.ctx.notify(format!("{label}: applies from your next message"));
        if running { self.restart_active_harness() } else { Task::none() }
    }

    /// Stops the active session's harness; the next send resumes the
    /// conversation on a new one, started with the current settings.
    pub(super) fn restart_active_harness(&mut self) -> Task<Message> {
        self.restart_for_access = false;
        match self.ctx.sessions.active_mut().and_then(|session| session.detach()) {
            Some(harness) => Task::perform(async move { harness.shutdown().await.map_err(|error| error.to_string()) }, Message::Operation),
            None => Task::none(),
        }
    }
}
