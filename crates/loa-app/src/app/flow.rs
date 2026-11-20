//! Starting, resuming, spawning and opening sessions, and sending prompts.

use super::*;

impl App {
    /// The config of a session on harness `harness`: this node and
    /// workspace, the session's own `cwd`, `project` and `mode` when it is
    /// resumed from `dom`, then every area's contribution and host tools.
    fn session_config(&mut self, session_id: &str, harness: &str, dom: Option<&Dom>) -> SessionConfig {
        let services = &self.ctx.services;
        let root = dom.and_then(Dom::root);
        let attr = |name| root.and_then(|root| root.str_attr(name));
        let mut config = SessionConfig {
            session_id: session_id.to_owned(),
            node: services.node.key.clone(),
            cwd: attr(attr::CWD).map_or_else(|| services.cwd.clone(), PathBuf::from),
            project: attr(attr::PROJECT).map(str::to_owned).or_else(|| services.project.clone()),
            mode: Some(attr(attr::MODE).and_then(SessionMode::parse).unwrap_or(SessionMode::Normal)),
            executable: services.harnesses.get(harness).and_then(|entry| entry.executable.clone()),
            ..SessionConfig::default()
        };
        config.approval_mode = self.access.clone();
        let capabilities = self.ctx.services.harnesses.registry.get(harness).map(|adapter| adapter.capabilities()).unwrap_or_default();
        let target = Target { harness, capabilities: &capabilities, resume: dom.is_some() };
        let ctx = &self.ctx;
        with_areas!(each_area!(self.areas, ref |area, wrap| area.contribute(&mut config, &target, ctx)));
        let mut router = HostToolRouter::new();
        let mut duplicates = Vec::new();
        for set in with_areas!(each_area!(self.areas, ref |area, wrap| area.host_tools(&config, &target, ctx))).into_iter().flatten() {
            for spec in set.specs {
                if let Err(duplicate) = router.add(spec, Arc::clone(&set.handler)) {
                    duplicates.push(duplicate.0);
                }
            }
        }
        router.install(&mut config);
        if !duplicates.is_empty() {
            self.ctx.notify(format!("Host tools offered twice, first one kept: {}", duplicates.join(", ")));
        }
        config
    }

    /// Starts a session on the selected harness; `prompt` is sent once it
    /// runs.
    pub(super) fn start(&mut self, prompt: Option<Prompt>) -> Task<Message> {
        if self.ctx.sessions.starting().is_some() {
            self.ctx.notify("A session is already starting");
            return self.give_back(prompt);
        }
        let Some(harness) = self.ctx.sessions.harness.clone() else {
            self.ctx.notify("No harness is available on this machine");
            return self.give_back(prompt);
        };
        let Some(adapter) = self.ctx.services.harnesses.runnable(&harness) else {
            let label = self.ctx.services.harnesses.label(&harness).to_owned();
            self.ctx.notify(format!("{label} is unavailable"));
            return self.give_back(prompt);
        };
        let (id, writer) = match self.ctx.sessions.create(&self.ctx.services) {
            Ok(created) => created,
            Err(error) => {
                self.ctx.notify(error);
                return self.give_back(prompt);
            }
        };
        self.ctx.sessions.set_starting(Some(id.clone()));
        self.ctx.clear_notice();
        let config = self.session_config(id.as_str(), &harness, None);
        Task::perform(
            async move {
                let sink: Arc<dyn loa_core::PatchSink> = writer;
                adapter.start(config, sink).await.map(Arc::from).map_err(|error| error.to_string())
            },
            move |result| Message::Started { id, result, prompt },
        )
    }

    /// Resumes the active session on its harness, then sends `prompt`. A
    /// harness that can't resume it, or isn't here, gets a new session.
    pub(super) fn resume(&mut self, prompt: Prompt) -> Task<Message> {
        if self.ctx.sessions.starting().is_some() {
            self.ctx.notify("A session is already starting");
            return self.give_back(Some(prompt));
        }
        let Some(session) = self.ctx.sessions.active() else { return Task::none() };
        let id = session.id().clone();
        let harness = session.harness_id().map(str::to_owned);
        let adapter = harness.as_deref().and_then(|harness| self.ctx.services.harnesses.runnable(harness));
        let (Some(harness), Some(adapter)) = (harness, adapter) else {
            self.ctx.notify("This session's harness isn't available here; continuing in a new session");
            return self.start(Some(prompt));
        };
        let dom = session.dom().clone();
        let sink: Arc<dyn loa_core::PatchSink> = Arc::clone(session.writer()) as _;
        let config = self.session_config(id.as_str(), &harness, Some(&dom));
        self.ctx.sessions.set_starting(Some(id.clone()));
        self.ctx.sessions.harness = Some(harness);
        Task::perform(
            async move { adapter.resume(config, &dom, sink).await.map(Arc::from).map_err(ResumeError::from) },
            move |result| Message::Resumed { id, result, prompt },
        )
    }

    /// A prompt that couldn't be sent goes back into the composer, which
    /// stops waiting for its turn. Without a prompt (New session) the
    /// composer is left as it is: a turn may be running elsewhere.
    fn give_back(&mut self, prompt: Option<Prompt>) -> Task<Message> {
        match prompt {
            Some(prompt) => self.ctx.composer.give_back(&prompt.text).map(Message::Composer),
            None => Task::none(),
        }
    }

    /// Continues the active session's conversation in a new session in the
    /// same folder, for a session its harness can no longer resume.
    fn continue_in_new_session(&mut self, prompt: Prompt) -> Task<Message> {
        let folder = self
            .ctx
            .sessions
            .active()
            .and_then(|session| session.dom().root()?.str_attr(attr::CWD).map(PathBuf::from))
            .filter(|folder| folder.is_dir() && *folder != self.ctx.services.cwd);
        let moved = folder.map_or_else(Task::none, |folder| self.set_folder(folder));
        Task::batch([moved, self.start(Some(prompt))])
    }

    /// Sends `prompt` to the active session, resuming it first when no
    /// harness runs it.
    pub(super) fn send(&mut self, mut prompt: Prompt) -> Task<Message> {
        let Some(session) = self.ctx.sessions.active() else { return Task::none() };
        let Some(harness) = session.harness().cloned() else { return self.resume(prompt) };
        let id = session.id().clone();
        with_areas!(each_area!(self.areas, mut |area, wrap| area.before_send(&id, &mut prompt, &mut self.ctx)));
        if let Some(session) = self.ctx.sessions.get_mut(id.as_str()) {
            session.set_busy(true);
        }
        self.ctx.composer.turn_started();
        let send = Task::perform(async move { harness.send(prompt).await.map_err(|error| error.to_string()) }, Message::Operation);
        Task::batch([send, self.settle()])
    }

    /// Starts a session an area creates itself: its config gets every
    /// area's contribution, then `spawn.start` creates and runs it.
    pub(super) fn spawn(&mut self, spawn: Spawn) -> Task<Message> {
        if self.ctx.sessions.starting().is_some() {
            self.ctx.notify("A session is already starting");
            return Task::none();
        }
        let Spawn { id, harness, start, prompt } = spawn;
        let config = self.session_config(id.as_str(), &harness, None);
        self.ctx.sessions.set_starting(Some(id.clone()));
        self.ctx.clear_notice();
        Task::perform(start(config), move |result| Message::Spawned { id, result, prompt })
    }

    pub(super) fn interrupt(&mut self) -> Task<Message> {
        let Some(session) = self.ctx.sessions.active() else { return Task::none() };
        let Some(harness) = session.harness().filter(|_| session.is_busy()).cloned() else { return Task::none() };
        Task::perform(async move { harness.interrupt().await.map_err(|error| error.to_string()) }, Message::Operation)
    }

    /// Shows session `id`, loading it from its journal when needed.
    pub(super) fn open_session(&mut self, id: &str) -> Task<Message> {
        if self.ctx.sessions.is_active(id) {
            return Task::none();
        }
        if let Err(error) = self.ctx.sessions.load(&self.ctx.services, id) {
            self.ctx.notify(error);
            return Task::none();
        }
        let id = SessionId::from(id);
        self.ctx.sessions.set_active(Some(id.clone()));
        self.ctx.close_screen();
        self.dispatch(SessionEvent::Activated(Some(id)))
    }

    /// A spawned session's harness came up: open its journal, then carry on
    /// as for a started one.
    pub(super) fn on_spawned(&mut self, id: SessionId, result: Started, prompt: Option<Prompt>) -> Task<Message> {
        match result {
            Ok(harness) => match self.ctx.sessions.load(&self.ctx.services, id.as_str()) {
                Ok(()) => self.update(Message::Started { id, result: Ok(harness), prompt }),
                Err(error) => {
                    self.ctx.sessions.set_starting(None);
                    self.ctx.notify(error);
                    Task::perform(async move { harness.shutdown().await.map_err(|error| error.to_string()) }, Message::Operation)
                }
            },
            Err(error) => {
                self.ctx.sessions.set_starting(None);
                // Leave no half created session behind; it may not exist.
                let _ = self.ctx.services.library.delete(id.as_str());
                self.ctx.notify(error);
                Task::none()
            }
        }
    }

    /// A new session's harness started (or failed to): attach it, show the
    /// session and send the prompt that started it.
    pub(super) fn on_started(&mut self, id: SessionId, result: Started, prompt: Option<Prompt>) -> Task<Message> {
        self.ctx.sessions.set_starting(None);
        let harness = match result {
            Ok(harness) => harness,
            Err(error) => {
                // Nothing ran: drop the journal it was given.
                let (_, closed) = self.ctx.sessions.close(&self.ctx.services, id.as_str(), true);
                self.ctx.notify(match closed {
                    Some(closed) => format!("{error}; {closed}"),
                    None => error,
                });
                self.ctx.show_harness();
                let _ = self.ctx.composer.turn_ended();
                return Task::none();
            }
        };
        let Some(session) = self.ctx.sessions.get_mut(id.as_str()) else {
            // Discarded while starting.
            return Task::perform(async move { harness.shutdown().await.map_err(|error| error.to_string()) }, Message::Operation);
        };
        session.attach(harness);
        self.ctx.sessions.set_active(Some(id.clone()));
        self.ctx.close_screen();
        self.files.refresh();
        let activated = self.dispatch(SessionEvent::Activated(Some(id)));
        let send = prompt.map_or_else(Task::none, |prompt| self.send(prompt));
        Task::batch([activated, working_diff(self.ctx.services.cwd.clone()), send])
    }

    /// A session's harness resumed (or couldn't): send `prompt` to it, or
    /// carry the conversation on in a new session.
    pub(super) fn on_resumed(&mut self, id: SessionId, result: Result<Arc<dyn HarnessSession>, ResumeError>, prompt: Prompt) -> Task<Message> {
        self.ctx.sessions.set_starting(None);
        match result {
            Ok(harness) => {
                let Some(session) = self.ctx.sessions.get_mut(id.as_str()) else {
                    return Task::perform(async move { harness.shutdown().await.map_err(|error| error.to_string()) }, Message::Operation);
                };
                session.attach(harness);
                let open = self.open_session(id.as_str());
                Task::batch([open, self.send(prompt)])
            }
            Err(ResumeError::Unsupported(reason)) => {
                self.ctx.notify(format!("Can't resume this session ({reason}); continuing in a new session"));
                self.continue_in_new_session(prompt)
            }
            // omp no longer has the session's own file: carry on
            // in a new session rather than failing every send.
            Err(ResumeError::Failed(error)) if error.contains("not found") => {
                self.ctx.notify("This session's harness history is gone; continuing in a new session");
                self.continue_in_new_session(prompt)
            }
            Err(ResumeError::Failed(error)) => {
                self.ctx.notify(error);
                // Nothing runs: give the composer back.
                let _ = self.ctx.composer.turn_ended();
                Task::none()
            }
        }
    }
}
