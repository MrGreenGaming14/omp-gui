//! Session feeds: patches from the bus applied to the UI copies and handed
//! to the chats and areas, and the Recents scan read from disk.

use super::*;

impl App {
    /// The app's own reaction to a session event, then every area's.
    pub(super) fn dispatch(&mut self, event: SessionEvent) -> Task<Message> {
        let mut presented = Task::none();
        match &event {
            SessionEvent::Activated(_) => presented = self.present(),
            SessionEvent::Loaded(id) => {
                if self.ctx.sessions.is_active(id.as_str()) {
                    presented = self.present();
                } else if let Some(summary) = self.ctx.sessions.get(id.as_str()).and_then(|session| session.summary()) {
                    self.ctx.shell.upsert_session(summary.clone());
                }
            }
            SessionEvent::Patch(id, patch) => self.apply_to_chats(id, patch),
            _ => {}
        }
        let tasks = with_areas!(each_area!(self.areas, mut |area, wrap| area.on_session_event(&event, &mut self.ctx).map(wrap)));
        Task::batch(tasks.into_iter().chain([presented, self.settle()]))
    }

    /// Applies a session feed. Every event of a batch is applied and reacted to
    /// in order; the turn reactions run once, after the batch.
    pub(super) fn feed(&mut self, id: SessionId, feed: Feed) -> Task<Message> {
        let active = self.ctx.sessions.is_active(id.as_str());
        let was_busy = active && self.ctx.sessions.active().is_some_and(|session| session.is_busy());
        let mut tasks = Vec::new();
        match feed {
            Feed::Synced(dom) => {
                let change = self.ctx.sessions.sync(id.as_str(), dom);
                tasks.push(self.on_change(id.clone(), change));
            }
            Feed::Events(events) => {
                for event in events {
                    let change = self.ctx.sessions.apply(id.as_str(), event);
                    tasks.push(self.on_change(id.clone(), change));
                }
            }
        }
        let busy = active && self.ctx.sessions.active().is_some_and(|session| session.is_busy());
        if busy && !was_busy {
            self.ctx.composer.turn_started();
        }
        if active && was_busy && !busy {
            if self.restart_for_access {
                tasks.push(self.restart_active_harness());
            }
            tasks.push(working_diff(self.ctx.services.cwd.clone()));
            let next = self.ctx.composer.turn_ended();
            tasks.push(self.composer_event(next));
        }
        Task::batch(tasks)
    }

    /// Reacts to one change of session `id`: its sidebar entry and status first,
    /// then the session's own event. Appends skip the areas.
    fn on_change(&mut self, id: SessionId, change: Change) -> Task<Message> {
        match change {
            Change::None => Task::none(),
            Change::Loaded => self.dispatch(SessionEvent::Loaded(id)),
            Change::Patch { patch, derived, summary_changed, turn_changed } => {
                let active = self.ctx.sessions.is_active(id.as_str());
                if let Some(session) = self.ctx.sessions.get(id.as_str()) {
                    if summary_changed {
                        if let Some(summary) = session.summary() {
                            self.ctx.shell.upsert_session(summary.clone());
                        }
                    }
                    if turn_changed && active {
                        self.ctx.shell.set_session_status(turn_status(session.dom()).map(str::to_owned));
                    }
                }
                let mut tasks = Vec::new();
                if matches!(patch.op, Op::Append { .. }) {
                    self.append(&id, &patch);
                } else {
                    tasks.push(self.dispatch(SessionEvent::Patch(id, patch)));
                }
                if let Some(event) = derived {
                    tasks.push(self.dispatch(event));
                }
                Task::batch(tasks)
            }
        }
    }

    /// A streamed chunk. Only the transcripts and the sessions area follow it,
    /// so the other areas and `settle` are skipped.
    fn append(&mut self, id: &SessionId, patch: &Patch) {
        self.apply_to_chats(id, patch);
        self.areas.sessions.on_append(id, patch, &self.ctx);
    }

    /// Applies a patch to the session's transcript when it is active, and to its
    /// detached window's transcript when it has one.
    fn apply_to_chats(&mut self, id: &SessionId, patch: &Patch) {
        let Some(session) = self.ctx.sessions.get(id.as_str()) else {
            return;
        };
        if self.ctx.sessions.is_active(id.as_str()) {
            self.chat.apply(patch, session.dom());
        }
        if let Some(chat) = self.detached_chats.get_mut(id.as_str()) {
            chat.apply(patch, session.dom());
        }
    }

    /// The sessions read from disk at startup. Empty sessions that are not open
    /// are deleted; open sessions show their live summary. Rows other areas
    /// listed meanwhile (peers' sessions) stay.
    pub(super) fn recents_loaded(&mut self, mut summaries: Vec<shell::SessionSummary>, mut problems: Vec<String>, empty: Vec<String>) -> Task<Message> {
        for id in empty {
            if self.ctx.sessions.get(&id).is_none() {
                if let Err(error) = self.ctx.services.library.delete(&id) {
                    problems.push(format!("Can't delete empty session {id}: {error}"));
                }
            }
        }
        for session in self.ctx.sessions.iter() {
            let Some(summary) = session.summary() else {
                continue;
            };
            match summaries.iter_mut().find(|each| each.id == summary.id) {
                Some(each) => *each = summary.clone(),
                None => summaries.push(summary.clone()),
            }
        }
        self.ctx.shell.upsert_sessions(summaries);
        if !problems.is_empty() {
            self.ctx.notify(problems.join("; "));
        }
        Task::none()
    }

    /// Runs what areas asked for and delivers the session events they
    /// caused, until none are left.
    pub(super) fn settle(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        loop {
            if !self.ctx.events.is_empty() {
                for event in std::mem::take(&mut self.ctx.events) {
                    tasks.push(self.dispatch(event));
                }
                continue;
            }
            if self.ctx.requests.is_empty() {
                break;
            }
            for request in std::mem::take(&mut self.ctx.requests) {
                let task = match request {
                    Request::NewSession => self.start(None),
                    Request::OpenSession(id) => self.open_session(&id),
                    Request::DetachSession(id) => self.detach_session(&id),
                    Request::Prompt(prompt) => self.send(prompt),
                    Request::OpenTab(id) => self.open_tab(&id),
                    Request::RevealTab(id) => self.reveal_tab(&id),
                    Request::Spawn(spawn) => self.spawn(spawn),
                    Request::ActivateProject { project, cwd } => self.activate_project(project, cwd),
                    Request::Forward(message) => self.update(*message),
                };
                tasks.push(task);
            }
        }
        Task::batch(tasks)
    }
}

/// The top bar status of the session: the state of its latest turn while it
/// runs, waits, or ended interrupted or failed.
pub(super) fn turn_status(dom: &Dom) -> Option<&'static str> {
    match dom_turns(dom).last()?.str_attr(attr::STATUS).and_then(TurnStatus::parse)? {
        TurnStatus::Running => Some("Running"),
        TurnStatus::Waiting => Some("Waiting"),
        TurnStatus::Cancelled => Some("Interrupted"),
        TurnStatus::Error => Some("Error"),
        TurnStatus::Done => None,
    }
}
