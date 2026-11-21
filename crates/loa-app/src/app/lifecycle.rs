//! The app side of the session lifecycle: hands lifecycle commands to the
//! runtime, runs its harness calls as tasks, adds every area's part to new
//! session configs and shows the active session in the chat.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use iced::{Task, Theme};
use loa_bus::SessionId;
use loa_contracts::harness::{HostToolRouter, Prompt, SessionConfig};
use loa_contracts::session::SessionSummary;
use loa_core::{Dom, Op, Patch, TurnStatus, attr};
use loa_runtime::lifecycle::{ACCESS, ACCESS_DEFAULT, Command, Completion, Effect, RuntimeEvent, Transition, session_cwd};
use loa_runtime::session::{Feed, SessionEvent, dom_turns};
use loa_ui_session::transcript as chat;
use loa_view::session::composer::AccessOption;

use super::Message as AppMessage;
use crate::area::{Ctx, Target};
use crate::areas::Registry;

#[derive(Clone)]
pub enum Message {
    /// A session's feed delivered something.
    Feed(SessionId, Feed),
    /// A harness call the runtime started returned.
    Completed(Completion),
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

/// Drives the runtime's session lifecycle and shows the active session in the chat.
pub(crate) struct SessionController {
    pub(crate) chat: chat::State,
    /// Transcripts of sessions shown in their own window.
    pub(crate) detached: HashMap<String, chat::State>,
    pub(super) after: Vec<After>,
}

/// Runs a harness call of the runtime; its completion goes back to the runtime.
fn perform(effect: Effect) -> Task<AppMessage> {
    Task::perform(effect, |completion| AppMessage::Session(Message::Completed(completion)))
}

impl SessionController {
    pub fn new(theme: &Theme) -> Self {
        let mut chat = chat::State::new(theme);
        chat.set_bottom_inset(loa_ui_kit::theme::width(loa_ui_kit::theme::LayoutRole::ComposerInset));
        Self { chat, detached: HashMap::new(), after: Vec::new() }
    }

    /// Hands `command` to the runtime, then does what its transition asks of the app.
    pub fn run(&mut self, ctx: &mut Ctx, areas: &mut Registry, command: Command) -> Task<AppMessage> {
        let transition = ctx.runtime.dispatch(command);
        self.apply(ctx, areas, transition)
    }

    /// Runs the harness calls of `transition` and reacts to its events, in order.
    pub fn apply(&mut self, ctx: &mut Ctx, areas: &mut Registry, transition: Transition) -> Task<AppMessage> {
        let Transition { effects, events } = transition;
        let mut tasks: Vec<_> = effects.into_iter().map(perform).collect();
        for event in events {
            tasks.push(self.react(ctx, areas, event));
        }
        Task::batch(tasks)
    }

    /// Sends `prompt` to the active session; every area may rewrite it first
    /// when a harness runs the session. Without one the runtime resumes it,
    /// and the prompt comes back as [`RuntimeEvent::SendRequested`].
    pub fn send(&mut self, ctx: &mut Ctx, areas: &mut Registry, mut prompt: Prompt) -> Task<AppMessage> {
        let running = ctx.runtime.sessions().active().filter(|session| session.harness().is_some()).map(|session| session.id().clone());
        if let Some(id) = running {
            areas.iter_mut().for_each(|area| area.before_send(&id, &mut prompt, ctx));
        }
        self.run(ctx, areas, Command::Send(prompt))
    }

    /// Handles a lifecycle message.
    pub fn update(&mut self, ctx: &mut Ctx, areas: &mut Registry, message: Message) -> Task<AppMessage> {
        match message {
            Message::Feed(id, feed) => {
                let mut batch = ctx.runtime.begin_feed(id, feed);
                let mut tasks = Vec::new();
                while let Some(transition) = ctx.runtime.advance_feed(&mut batch) {
                    tasks.push(self.apply(ctx, areas, transition));
                }
                Task::batch(tasks)
            }
            Message::Completed(completion) => self.run(ctx, areas, Command::Completed(completion)),
        }
    }

    /// The app's part of one runtime event.
    fn react(&mut self, ctx: &mut Ctx, areas: &mut Registry, event: RuntimeEvent) -> Task<AppMessage> {
        match event {
            RuntimeEvent::PrepareSession { operation, mut config, harness, resume } => {
                if !resume {
                    ctx.clear_notice();
                }
                prepare(ctx, areas, &mut config, &harness, resume);
                self.run(ctx, areas, Command::Prepared { operation, config })
            }
            RuntimeEvent::Session(event) => {
                if matches!(event, SessionEvent::Activated(Some(_))) {
                    ctx.close_screen();
                }
                self.dispatch(ctx, areas, event)
            }
            RuntimeEvent::Notice(notice) => {
                ctx.notify(notice);
                Task::none()
            }
            RuntimeEvent::StartFailed(_) => {
                ctx.show_harness();
                let _ = ctx.composer.turn_ended();
                Task::none()
            }
            RuntimeEvent::ResumeFailed(_) => {
                let _ = ctx.composer.turn_ended();
                Task::none()
            }
            RuntimeEvent::ContinueRequested { folder, prompt } => {
                self.after.push(After::Continue(folder, prompt));
                Task::none()
            }
            RuntimeEvent::SessionStarted(_) => {
                ctx.close_screen();
                self.after.extend([After::RefreshFiles, After::Diff]);
                Task::none()
            }
            RuntimeEvent::TurnStarted(_) => {
                ctx.composer.turn_started();
                Task::none()
            }
            RuntimeEvent::TurnEnded(_) => {
                self.after.extend([After::Diff, After::TurnEnded]);
                Task::none()
            }
            RuntimeEvent::SummaryChanged(id) => {
                if let Some(summary) = ctx.runtime.sessions().get(id.as_str()).and_then(|session| session.summary()) {
                    ctx.shell.sidebar_mut().upsert_session(summary.clone());
                }
                Task::none()
            }
            RuntimeEvent::StatusChanged(id) => {
                if let Some(session) = ctx.runtime.sessions().get(id.as_str()) {
                    ctx.shell.set_session_status(turn_status(session.dom()).map(str::to_owned));
                }
                Task::none()
            }
            RuntimeEvent::PromptReturned(prompt) => ctx.composer.give_back(&prompt.text).map(AppMessage::Composer),
            RuntimeEvent::SendRequested { prompt, .. } => self.send(ctx, areas, prompt),
        }
    }

    /// Loads the active session's DOM into the chat and the frame; the composer follows
    /// whether it runs a turn.
    fn present(&mut self, ctx: &mut Ctx) {
        let Some(session) = ctx.runtime.sessions().active() else {
            ctx.composer.follow_session(false);
            self.chat.load(&Dom::new());
            self.after.push(After::Present(ctx.runtime.cwd().to_path_buf()));
            return;
        };
        ctx.composer.follow_session(session.is_busy());
        self.chat.load(session.dom());
        let sidebar = ctx.shell.sidebar_mut();
        if let Some(summary) = session.summary() {
            sidebar.upsert_session(summary.clone());
        }
        sidebar.set_active(Some(session.id().to_string()));
        ctx.shell.set_session_status(turn_status(session.dom()).map(str::to_owned));
        self.after.push(After::Present(session_cwd(session.dom()).unwrap_or_else(|| ctx.runtime.cwd().to_path_buf())));
    }

    /// The app's own reaction to a session event, then every area's. A streamed append only
    /// reaches the transcripts and the sessions area's Agents tab.
    pub fn dispatch(&mut self, ctx: &mut Ctx, areas: &mut Registry, event: SessionEvent) -> Task<AppMessage> {
        match &event {
            SessionEvent::Activated(_) => self.present(ctx),
            SessionEvent::Loaded(id) if ctx.runtime.sessions().is_active(id.as_str()) => self.present(ctx),
            SessionEvent::Loaded(id) => {
                if let Some(summary) = ctx.runtime.sessions().get(id.as_str()).and_then(|session| session.summary()) {
                    ctx.shell.sidebar_mut().upsert_session(summary.clone());
                }
            }
            SessionEvent::Patch(id, patch) => {
                self.apply_to_chats(ctx, id, patch);
                if matches!(patch.op, Op::Append { .. }) {
                    areas.get_mut::<crate::areas::sessions::State>().on_append(id, patch, ctx);
                    return Task::none();
                }
            }
            _ => {}
        }
        let tasks: Vec<_> = areas.iter_mut().map(|area| area.on_session_event(&event, ctx)).collect();
        Task::batch(tasks)
    }

    /// Applies a patch to the session's transcript when it is active, and to its detached
    /// window's transcript when it has one.
    fn apply_to_chats(&mut self, ctx: &Ctx, id: &SessionId, patch: &Patch) {
        let Some(session) = ctx.runtime.sessions().get(id.as_str()) else { return };
        if ctx.runtime.sessions().is_active(id.as_str()) {
            self.chat.apply(patch, session.dom());
        }
        if let Some(chat) = self.detached.get_mut(id.as_str()) {
            chat.apply(patch, session.dom());
        }
    }

    /// The sessions read from disk at startup. Empty sessions that are not open are deleted;
    /// open sessions show their live summary. Rows other areas listed meanwhile (peers'
    /// sessions) stay.
    pub fn recents_loaded(&mut self, ctx: &mut Ctx, mut summaries: Vec<SessionSummary>, mut problems: Vec<String>, empty: Vec<String>) {
        problems.extend(ctx.runtime.delete_empty(empty));
        for summary in ctx.runtime.sessions().iter().filter_map(|session| session.summary()) {
            match summaries.iter_mut().find(|each| each.id == summary.id) {
                Some(each) => each.clone_from(summary),
                None => summaries.push(summary.clone()),
            }
        }
        let sidebar = ctx.shell.sidebar_mut();
        for summary in summaries {
            sidebar.upsert_session(summary);
        }
        if !problems.is_empty() {
            ctx.notify(problems.join("; "));
        }
    }

    /// Shows the permission picker for harnesses with approval modes, with the current level.
    pub fn sync_access(&self, ctx: &mut Ctx) {
        let harness = ctx.runtime.sessions().active().and_then(|session| session.harness_id()).or(ctx.runtime.sessions().harness.as_deref());
        let modes = harness.map_or(&[][..], |harness| ctx.runtime.harnesses().traits(harness).approval_modes);
        let options: Vec<_> = ACCESS
            .iter()
            .filter(|(id, _, _)| modes.contains(id))
            .map(|(id, label, detail)| AccessOption { id: (*id).to_owned(), label: (*label).to_owned(), detail: (*detail).to_owned() })
            .collect();
        let current = (!options.is_empty()).then(|| ctx.runtime.access().unwrap_or(ACCESS_DEFAULT).to_owned());
        ctx.composer.set_access(options, current.as_deref());
    }

    /// A permission level was picked: it is saved, new sessions start on it and the open one restarts on it.
    pub fn set_access(&mut self, ctx: &mut Ctx, areas: &mut Registry, id: String) -> Task<AppMessage> {
        if id == ctx.runtime.access().unwrap_or(ACCESS_DEFAULT) {
            return Task::none();
        }
        ctx.edit_config("settings", |config| config.harness.approval_mode = Some(id.clone()));
        self.run(ctx, areas, Command::SetAccess(id))
    }
}

/// Adds every area's contribution to `config`, the runtime's base config of a session on `harness`.
fn prepare(ctx: &mut Ctx, areas: &Registry, config: &mut SessionConfig, harness: &str, resume: bool) {
    let capabilities = ctx.runtime.harnesses().registry.get(harness).map(|adapter| adapter.capabilities()).unwrap_or_default();
    let target = Target { harness, capabilities: &capabilities, resume };
    areas.iter().for_each(|area| area.contribute(config, &target, ctx));
    let mut router = HostToolRouter::new();
    let mut duplicates = Vec::new();
    for set in areas.iter().filter_map(|area| area.host_tools(config, &target, ctx)) {
        for spec in set.specs {
            if let Err(duplicate) = router.add(spec, Arc::clone(&set.handler)) {
                duplicates.push(duplicate.0);
            }
        }
    }
    router.install(config);
    if !duplicates.is_empty() {
        ctx.notify(format!("Host tools offered twice, first one kept: {}", duplicates.join(", ")));
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
