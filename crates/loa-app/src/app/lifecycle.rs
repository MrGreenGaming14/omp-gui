//! The session lifecycle: starting, resuming, sending to, interrupting and feeding sessions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use iced::{Task, Theme};
use loa_bus::SessionId;
use loa_contracts::harness::{AdapterError, HarnessSession, HostToolRouter, Prompt, SessionConfig};
use loa_core::{Dom, ElementKind, Op, SessionMode, TurnStatus, attr};
use loa_ui_session::composer;
use loa_ui_session::transcript as chat;

use super::Message as AppMessage;
use crate::area::{Ctx, Spawn, Target, perform};
use crate::areas::Registry;
use loa_runtime::session::{Change, Feed, SessionEvent, dom_turns};

/// The permission picker's choices: omp's approval modes, as id, label and detail.
const ACCESS: [(&str, &str, &str); 4] = [
    ("always-ask", "Ask first", "Reads run; edits and commands ask"),
    ("always-ask-cached", "Ask and remember", "Asks before file reads and writes, remembers answers"),
    ("write", "Auto-edit", "Reads and edits run; commands ask"),
    ("yolo", "Full access", "Everything runs without asking"),
];
const ACCESS_DEFAULT: &str = "always-ask";

/// Why a session couldn't be resumed.
#[derive(Clone, Debug)]
pub enum ResumeError {
    /// The harness can't resume it; a new session takes the prompt.
    Unsupported(String),
    Failed(String),
}

impl From<AdapterError> for ResumeError {
    fn from(error: AdapterError) -> Self {
        match error {
            AdapterError::Unsupported { .. } | AdapterError::Format(_) => ResumeError::Unsupported(error.to_string()),
            error => ResumeError::Failed(error.to_string()),
        }
    }
}

type Started = Result<Arc<dyn HarnessSession>, String>;

#[derive(Clone)]
pub enum Message {
    /// A session's feed delivered something.
    Feed(SessionId, Feed),
    /// A new session's harness started; `prompt` is sent once it runs.
    Started { id: SessionId, result: Started, prompt: Option<Prompt> },
    /// A session's harness resumed; `prompt` is sent once it runs.
    Resumed { id: SessionId, result: Result<Arc<dyn HarnessSession>, ResumeError>, prompt: Prompt },
    /// A session an area spawned (fork, rewind, compaction) started.
    Spawned { id: SessionId, result: Started, prompt: Option<Prompt> },
    /// A harness call returned.
    Done(Result<(), String>),
}

/// What the app does after a lifecycle call, with the parts it owns.
pub(super) enum After {
    /// The center shows a session in `cwd` (or none): the composer chip and terminal follow.
    Present(PathBuf),
    /// Reread the working tree diff.
    Diff,
    RefreshFiles,
    /// The active session's turn ended: a queued composer message goes out.
    TurnEnded,
    /// Continue a session in a new one, in `folder` when it still exists.
    Continue(Option<PathBuf>, Prompt),
}

/// Runs sessions on their harnesses and shows the active one in the chat.
pub(crate) struct SessionController {
    pub(crate) chat: chat::State,
    /// Transcripts of sessions shown in their own window.
    pub(crate) detached: HashMap<String, chat::State>,
    pub(super) after: Vec<After>,
}

fn wrap(message: Message) -> AppMessage {
    AppMessage::Session(message)
}

impl SessionController {
    pub fn new(theme: &Theme) -> Self {
        let mut chat = chat::State::new(theme);
        chat.set_bottom_inset(loa_ui_kit::theme::width(loa_ui_kit::theme::LayoutRole::ComposerInset));
        Self { chat, detached: HashMap::new(), after: Vec::new() }
    }

    /// The config of a session on `harness`, resumed from `dom` when given, with every area's contribution.
    fn config(&self, ctx: &mut Ctx, areas: &Registry, session_id: &str, harness: &str, dom: Option<&Dom>) -> SessionConfig {
        let runtime = &ctx.runtime;
        let attr = |name| dom.and_then(Dom::root).and_then(|root| root.str_attr(name));
        let mut config = SessionConfig {
            session_id: session_id.to_owned(),
            node: runtime.node().to_owned(),
            cwd: attr(attr::CWD).map_or_else(|| runtime.cwd().to_path_buf(), PathBuf::from),
            project: Some(attr(attr::PROJECT).unwrap_or(runtime.project()).to_owned()),
            mode: Some(attr(attr::MODE).and_then(SessionMode::parse).unwrap_or(SessionMode::Normal)),
            executable: runtime.harnesses().get(harness).and_then(|entry| entry.executable.clone()),
            approval_mode: runtime.access().map(str::to_owned),
            ..SessionConfig::default()
        };
        let capabilities = runtime.harnesses().registry.get(harness).map(|adapter| adapter.capabilities()).unwrap_or_default();
        let target = Target { harness, capabilities: &capabilities, resume: dom.is_some() };
        areas.iter().for_each(|area| area.contribute(&mut config, &target, ctx));
        let mut router = HostToolRouter::new();
        let mut duplicates = Vec::new();
        for set in areas.iter().filter_map(|area| area.host_tools(&config, &target, ctx)) {
            for spec in set.specs {
                if let Err(duplicate) = router.add(spec, Arc::clone(&set.handler)) {
                    duplicates.push(duplicate.0);
                }
            }
        }
        router.install(&mut config);
        if !duplicates.is_empty() {
            ctx.notify(format!("Host tools offered twice, first one kept: {}", duplicates.join(", ")));
        }
        config
    }

    /// Starts a session on the selected harness; `prompt` is sent once it runs.
    pub fn start(&mut self, ctx: &mut Ctx, areas: &Registry, prompt: Option<Prompt>) -> Task<AppMessage> {
        if ctx.runtime.sessions().starting().is_some() {
            return Task::none();
        }
        let Some(harness) = ctx.runtime.sessions().harness.clone() else {
            ctx.notify("No harness is available on this machine");
            return Task::none();
        };
        let Some(adapter) = ctx.runtime.harnesses().runnable(&harness) else {
            ctx.notify(format!("{} is unavailable", ctx.runtime.harnesses().label(&harness)));
            return Task::none();
        };
        let (id, writer) = match ctx.runtime.create_session() {
            Ok(created) => created,
            Err(error) => {
                ctx.notify(error);
                return Task::none();
            }
        };
        ctx.runtime.sessions_mut().set_starting(Some(id.clone()));
        ctx.clear_notice();
        let config = self.config(ctx, areas, id.as_str(), &harness, None);
        perform(
            async move {
                let sink: Arc<dyn loa_core::PatchSink> = writer;
                adapter.start(config, sink).await.map(Arc::from)
            },
            move |result| wrap(Message::Started { id, result, prompt }),
        )
    }

    /// Resumes the active session on its harness, then sends `prompt`; else continues in a new session.
    fn resume(&mut self, ctx: &mut Ctx, areas: &Registry, prompt: Prompt) -> Task<AppMessage> {
        if ctx.runtime.sessions().starting().is_some() {
            ctx.notify("A session is already starting");
            return Task::none();
        }
        let Some(session) = ctx.runtime.sessions().active() else { return Task::none() };
        let id = session.id().clone();
        let harness = session.harness_id().map(str::to_owned);
        let adapter = harness.as_deref().and_then(|harness| ctx.runtime.harnesses().runnable(harness));
        let (Some(harness), Some(adapter)) = (harness, adapter) else {
            ctx.notify("This session's harness isn't available here; continuing in a new session");
            return self.start(ctx, areas, Some(prompt));
        };
        let dom = session.dom().clone();
        let sink: Arc<dyn loa_core::PatchSink> = Arc::clone(session.writer()) as _;
        let config = self.config(ctx, areas, id.as_str(), &harness, Some(&dom));
        ctx.runtime.sessions_mut().set_starting(Some(id.clone()));
        ctx.runtime.sessions_mut().harness = Some(harness);
        Task::perform(async move { adapter.resume(config, &dom, sink).await.map(Arc::from).map_err(ResumeError::from) }, move |result| {
            wrap(Message::Resumed { id, result, prompt })
        })
    }

    /// Continues the active session's conversation in a new session in its folder.
    fn continue_in_new_session(&mut self, ctx: &Ctx, prompt: Prompt) {
        let folder = ctx.runtime.sessions().active().and_then(|session| session_cwd(session.dom())).filter(|folder| folder.is_dir() && *folder != ctx.runtime.cwd());
        self.after.push(After::Continue(folder, prompt));
    }

    /// Sends `prompt` to the active session, resuming it first when no harness runs it.
    pub fn send(&mut self, ctx: &mut Ctx, areas: &mut Registry, mut prompt: Prompt) -> Task<AppMessage> {
        let Some(session) = ctx.runtime.sessions().active() else { return Task::none() };
        let Some(harness) = session.harness().cloned() else { return self.resume(ctx, areas, prompt) };
        let id = session.id().clone();
        areas.iter_mut().for_each(|area| area.before_send(&id, &mut prompt, ctx));
        if let Some(session) = ctx.runtime.sessions_mut().get_mut(id.as_str()) {
            session.set_busy(true);
        }
        ctx.composer.turn_started();
        perform(async move { harness.send(prompt).await }, |result| wrap(Message::Done(result)))
    }

    /// Starts a session an area creates itself, with every area's contribution.
    pub fn spawn(&mut self, ctx: &mut Ctx, areas: &Registry, spawn: Spawn) -> Task<AppMessage> {
        if ctx.runtime.sessions().starting().is_some() {
            ctx.notify("A session is already starting");
            return Task::none();
        }
        let Spawn { id, harness, start, prompt } = spawn;
        let config = self.config(ctx, areas, id.as_str(), &harness, None);
        ctx.runtime.sessions_mut().set_starting(Some(id.clone()));
        ctx.clear_notice();
        Task::perform(start(config), move |result| wrap(Message::Spawned { id, result, prompt }))
    }

    pub fn interrupt(&mut self, ctx: &Ctx) -> Task<AppMessage> {
        let Some(session) = ctx.runtime.sessions().active() else { return Task::none() };
        let Some(harness) = session.harness().filter(|_| session.is_busy()).cloned() else { return Task::none() };
        perform(async move { harness.interrupt().await }, |result| wrap(Message::Done(result)))
    }

    /// Shows session `id`, loading it from its journal when needed.
    pub fn open(&mut self, ctx: &mut Ctx, areas: &mut Registry, id: &str) -> Task<AppMessage> {
        if ctx.runtime.sessions().is_active(id) {
            return Task::none();
        }
        if let Err(error) = ctx.runtime.load_session(id) {
            ctx.notify(error);
            return Task::none();
        }
        let id = SessionId::from(id);
        ctx.runtime.sessions_mut().set_active(Some(id.clone()));
        ctx.close_screen();
        self.dispatch(ctx, areas, SessionEvent::Activated(Some(id)))
    }

    /// Loads the active session's DOM into the chat and the frame.
    fn present(&mut self, ctx: &mut Ctx) {
        let Some(session) = ctx.runtime.sessions().active() else {
            self.chat.load(&Dom::new());
            self.after.push(After::Present(ctx.runtime.cwd().to_path_buf()));
            return;
        };
        self.chat.load(session.dom());
        let sidebar = ctx.shell.sidebar_mut();
        if let Some(summary) = session.summary() {
            sidebar.upsert_session(summary);
        }
        sidebar.set_active(Some(session.id().to_string()));
        ctx.shell.set_session_status(turn_status(session.dom()).map(str::to_owned));
        self.after.push(After::Present(session_cwd(session.dom()).unwrap_or_else(|| ctx.runtime.cwd().to_path_buf())));
    }

    /// The app's own reaction to a session event, then every area's.
    pub fn dispatch(&mut self, ctx: &mut Ctx, areas: &mut Registry, event: SessionEvent) -> Task<AppMessage> {
        match &event {
            SessionEvent::Activated(_) => self.present(ctx),
            SessionEvent::Loaded(id) if ctx.runtime.sessions().is_active(id.as_str()) => self.present(ctx),
            SessionEvent::Loaded(id) => {
                if let Some(summary) = ctx.runtime.sessions().get(id.as_str()).and_then(|session| session.summary()) {
                    ctx.shell.sidebar_mut().upsert_session(summary);
                }
            }
            SessionEvent::Patch(id, patch) => {
                let active = ctx.runtime.sessions().is_active(id.as_str());
                if let Some(session) = ctx.runtime.sessions().get(id.as_str()) {
                    if active {
                        self.chat.apply(patch, session.dom());
                    }
                    if let Some(chat) = self.detached.get_mut(id.as_str()) {
                        chat.apply(patch, session.dom());
                    }
                    if changes_summary(session.dom(), patch) {
                        if let Some(summary) = session.summary() {
                            ctx.shell.sidebar_mut().upsert_session(summary);
                        }
                        if active {
                            ctx.shell.set_session_status(turn_status(session.dom()).map(str::to_owned));
                        }
                    }
                }
            }
            _ => {}
        }
        let tasks: Vec<_> = areas.iter_mut().map(|area| area.on_session_event(&event, ctx)).collect();
        Task::batch(tasks)
    }

    /// Applies a session feed and reacts to the active session starting or finishing a turn.
    fn feed(&mut self, ctx: &mut Ctx, areas: &mut Registry, id: SessionId, feed: Feed) -> Task<AppMessage> {
        let active = ctx.runtime.sessions().is_active(id.as_str());
        let busy = |ctx: &Ctx| active && ctx.runtime.sessions().active().is_some_and(|session| session.is_busy());
        let was_busy = busy(ctx);
        let task = match ctx.runtime.sessions_mut().feed(id.as_str(), feed) {
            Change::None => return Task::none(),
            Change::Loaded => self.dispatch(ctx, areas, SessionEvent::Loaded(id)),
            Change::Patch(patch, derived) => {
                let patched = self.dispatch(ctx, areas, SessionEvent::Patch(id, patch));
                let derived = derived.map(|event| self.dispatch(ctx, areas, event));
                Task::batch([patched, derived.unwrap_or_else(Task::none)])
            }
        };
        let now_busy = busy(ctx);
        if now_busy && !was_busy {
            ctx.composer.turn_started();
        }
        if was_busy && !now_busy {
            let restart = if ctx.runtime.restart_for_access() { self.restart(ctx) } else { Task::none() };
            self.after.extend([After::Diff, After::TurnEnded]);
            return Task::batch([task, restart]);
        }
        task
    }

    /// Handles a lifecycle message.
    pub fn update(&mut self, ctx: &mut Ctx, areas: &mut Registry, message: Message) -> Task<AppMessage> {
        match message {
            Message::Feed(id, feed) => self.feed(ctx, areas, id, feed),
            Message::Spawned { id, result: Ok(harness), prompt } => match ctx.runtime.load_session(id.as_str()) {
                Ok(()) => self.update(ctx, areas, Message::Started { id, result: Ok(harness), prompt }),
                Err(error) => {
                    ctx.runtime.sessions_mut().set_starting(None);
                    ctx.notify(error);
                    shutdown(harness)
                }
            },
            Message::Spawned { id, result: Err(error), .. } => {
                ctx.runtime.sessions_mut().set_starting(None);
                // Leave no half created session behind; it may not exist.
                let _ = ctx.runtime.library().delete(id.as_str());
                ctx.notify(error);
                Task::none()
            }
            Message::Started { id, result: Err(error), .. } => {
                ctx.runtime.sessions_mut().set_starting(None);
                // Nothing ran: drop the journal it was given.
                let (_, closed) = ctx.runtime.close_session(id.as_str(), true);
                ctx.notify(closed.map_or_else(|| error.clone(), |closed| format!("{error}; {closed}")));
                ctx.show_harness();
                let _ = ctx.composer.turn_ended();
                Task::none()
            }
            Message::Started { id, result: Ok(harness), prompt } => {
                ctx.runtime.sessions_mut().set_starting(None);
                // Discarded while starting.
                let Some(session) = ctx.runtime.sessions_mut().get_mut(id.as_str()) else { return shutdown(harness) };
                session.attach(harness);
                ctx.runtime.sessions_mut().set_active(Some(id.clone()));
                ctx.close_screen();
                self.after.extend([After::RefreshFiles, After::Diff]);
                let activated = self.dispatch(ctx, areas, SessionEvent::Activated(Some(id)));
                let send = prompt.map_or_else(Task::none, |prompt| self.send(ctx, areas, prompt));
                Task::batch([activated, send])
            }
            Message::Resumed { id, result, prompt } => {
                ctx.runtime.sessions_mut().set_starting(None);
                match result {
                    Ok(harness) => {
                        let Some(session) = ctx.runtime.sessions_mut().get_mut(id.as_str()) else { return shutdown(harness) };
                        session.attach(harness);
                        let open = self.open(ctx, areas, id.as_str());
                        Task::batch([open, self.send(ctx, areas, prompt)])
                    }
                    Err(ResumeError::Unsupported(reason)) => {
                        ctx.notify(format!("Can't resume this session ({reason}); continuing in a new session"));
                        self.continue_in_new_session(ctx, prompt);
                        Task::none()
                    }
                    // omp no longer has the session's own file: carry on in a new session.
                    Err(ResumeError::Failed(error)) if error.contains("not found") => {
                        ctx.notify("This session's harness history is gone; continuing in a new session");
                        self.continue_in_new_session(ctx, prompt);
                        Task::none()
                    }
                    Err(ResumeError::Failed(error)) => {
                        ctx.notify(error);
                        let _ = ctx.composer.turn_ended();
                        Task::none()
                    }
                }
            }
            Message::Done(result) => {
                if let Err(error) = result {
                    if let Some(session) = ctx.runtime.sessions_mut().active_mut() {
                        session.set_busy(false);
                    }
                    ctx.notify(error);
                }
                Task::none()
            }
        }
    }

    /// Shows the permission picker for harnesses with approval modes, with the current level.
    pub fn sync_access(&self, ctx: &mut Ctx) {
        let harness = ctx.runtime.sessions().active().and_then(|session| session.harness_id()).or(ctx.runtime.sessions().harness.as_deref());
        let modes = harness.map_or(&[][..], |harness| ctx.runtime.harnesses().traits(harness).approval_modes);
        let options: Vec<_> = ACCESS
            .iter()
            .filter(|(id, _, _)| modes.contains(id))
            .map(|(id, label, detail)| composer::AccessOption { id: (*id).to_owned(), label: (*label).to_owned(), detail: (*detail).to_owned() })
            .collect();
        let current = (!options.is_empty()).then(|| ctx.runtime.access().unwrap_or(ACCESS_DEFAULT).to_owned());
        ctx.composer.set_access(options, current.as_deref());
    }

    /// A permission level was picked: new sessions start on it, the open one restarts on it.
    pub fn set_access(&mut self, ctx: &mut Ctx, id: String) -> Task<AppMessage> {
        if id == ctx.runtime.access().unwrap_or(ACCESS_DEFAULT) {
            return Task::none();
        }
        ctx.runtime.set_access(Some(id.clone()));
        ctx.edit_config("settings", |config| config.harness.approval_mode = Some(id.clone()));
        let label = ACCESS.iter().find(|(each, _, _)| *each == id).map_or("", |(_, label, _)| *label);
        let running = ctx.runtime.sessions().active().is_some_and(|session| session.harness().is_some());
        if running && ctx.runtime.sessions().active().is_some_and(|session| session.is_busy()) {
            ctx.runtime.set_restart_for_access(true);
            ctx.notify(format!("{label}: applies once this turn ends"));
            return Task::none();
        }
        ctx.notify(format!("{label}: applies from your next message"));
        if running { self.restart(ctx) } else { Task::none() }
    }

    /// Stops the active session's harness; the next send resumes it on a new one.
    fn restart(&mut self, ctx: &mut Ctx) -> Task<AppMessage> {
        ctx.runtime.set_restart_for_access(false);
        ctx.runtime.sessions_mut().active_mut().and_then(|session| session.detach()).map_or_else(Task::none, shutdown)
    }
}

fn shutdown(harness: Arc<dyn HarnessSession>) -> Task<AppMessage> {
    perform(async move { harness.shutdown().await }, |result| wrap(Message::Done(result)))
}

/// The folder a session works in, from its DOM.
fn session_cwd(dom: &Dom) -> Option<PathBuf> {
    dom.root()?.str_attr(attr::CWD).map(PathBuf::from)
}

/// Whether `patch` may change the session's sidebar entry or status.
fn changes_summary(dom: &Dom, patch: &loa_core::Patch) -> bool {
    match &patch.op {
        Op::Insert { kind, .. } => matches!(kind, ElementKind::Session | ElementKind::Turn | ElementKind::Message | ElementKind::Decision),
        Op::Set { name, .. } => {
            matches!(name.as_str(), attr::TITLE | attr::PROJECT | attr::STATUS | attr::STARTED | attr::ENDED)
                && dom.get(patch.node).is_some_and(|node| node.kind != ElementKind::Call)
        }
        Op::Remove => true,
        Op::Append { .. } => false,
    }
}

/// The top bar status of the session: its latest turn's state unless it finished normally.
pub(crate) fn turn_status(dom: &Dom) -> Option<&'static str> {
    match dom_turns(dom).last()?.str_attr(attr::STATUS).and_then(TurnStatus::parse)? {
        TurnStatus::Running => Some("Running"),
        TurnStatus::Waiting => Some("Waiting"),
        TurnStatus::Cancelled => Some("Interrupted"),
        TurnStatus::Error => Some("Error"),
        TurnStatus::Done => None,
    }
}
