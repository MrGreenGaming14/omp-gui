//! The session lifecycle without a UI: starting, resuming, sending to,
//! interrupting, closing and feeding sessions.
//!
//! The app hands each [`Command`] to [`Runtime::dispatch`] and gets a
//! [`Transition`] back: futures to run, each resolving to a [`Completion`]
//! the app dispatches as [`Command::Completed`], and the events to react to,
//! in order. Feeds use [`Runtime::begin_feed`] and [`Runtime::advance_feed`]
//! so consumers react to each patch before the next one changes the DOM.
//!
//! Starting, resuming and spawning a session is a handshake: the runtime
//! builds the base [`SessionConfig`] and emits [`RuntimeEvent::PrepareSession`];
//! the app adds what its areas contribute and dispatches
//! [`Command::Prepared`], which launches the harness exactly once.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use loa_bus::SessionId;
use loa_contracts::harness::{AdapterError, HarnessSession, Prompt, SessionConfig};
use loa_core::{Dom, PatchSink, SessionMode, attr};

use crate::Runtime;
use crate::session::{Change, Feed, SessionEvent};

/// The permission picker's choices: omp's approval modes, as id, label and detail.
pub const ACCESS: [(&str, &str, &str); 4] = [
    ("always-ask", "Ask first", "Reads run; edits and commands ask"),
    ("always-ask-cached", "Ask and remember", "Asks before file reads and writes, remembers answers"),
    ("write", "Auto-edit", "Reads and edits run; commands ask"),
    ("yolo", "Full access", "Everything runs without asking"),
];
pub const ACCESS_DEFAULT: &str = "always-ask";

/// Label of approval mode `id`; empty for one [`ACCESS`] doesn't list.
pub fn access_label(id: &str) -> &'static str {
    ACCESS.iter().find(|(each, _, _)| *each == id).map_or("", |(_, label, _)| *label)
}

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

/// Starts a session whose journal the closure creates: it gets the session's
/// full config (every area's contribution, keyed by [`Spawn::id`]) and
/// returns the running harness.
pub type SpawnStart = Arc<
    dyn Fn(SessionConfig) -> Pin<Box<dyn Future<Output = Result<Arc<dyn HarnessSession>, String>> + Send>> + Send + Sync,
>;

/// A session an area creates itself (fork, rewind, compaction); it becomes
/// the active session.
#[derive(Clone)]
pub struct Spawn {
    /// Id of the new session; `start` must create it in the library.
    pub id: SessionId,
    /// Harness it runs on.
    pub harness: String,
    pub start: SpawnStart,
    /// Sent once it runs.
    pub prompt: Option<Prompt>,
}

/// One start, resume or spawn, from [`RuntimeEvent::PrepareSession`] to its [`Completion`].
pub type OperationId = u64;

/// Harness work to run off the caller; its result goes back as [`Command::Completed`].
pub type Effect = Pin<Box<dyn Future<Output = Completion> + Send + 'static>>;

/// What a [`Command`] did: work to run and events to react to, in order.
#[derive(Default)]
pub struct Transition {
    pub effects: Vec<Effect>,
    pub events: Vec<RuntimeEvent>,
}

/// What the runtime is asked to do.
#[derive(Clone)]
pub enum Command {
    /// Start a session on the selected harness; `prompt` is sent once it runs.
    Start { prompt: Option<Prompt> },
    /// Show session `id`, loading it from its journal when needed.
    Open(SessionId),
    /// Send to the active session, resuming it first when no harness runs it.
    Send(Prompt),
    /// Start a session an area creates itself.
    Spawn(Spawn),
    /// Stop the active session's running turn.
    Interrupt,
    /// Close a session, deleting its journal too when `delete` is set.
    Close { id: SessionId, delete: bool },
    /// Run new sessions, and the open one, on this approval mode.
    SetAccess(String),
    /// The full config of a prepared operation: launch it.
    Prepared { operation: OperationId, config: Box<SessionConfig> },
    /// An [`Effect`] finished.
    Completed(Completion),
}

/// What an [`Effect`] resolves to.
#[derive(Clone)]
pub enum Completion {
    /// A new session's harness started; `prompt` is sent once it runs.
    Started { operation: OperationId, id: SessionId, result: Result<Arc<dyn HarnessSession>, String>, prompt: Option<Prompt> },
    /// A session's harness resumed; `prompt` is sent once it runs.
    Resumed { operation: OperationId, id: SessionId, result: Result<Arc<dyn HarnessSession>, ResumeError>, prompt: Prompt },
    /// A session an area spawned started.
    Spawned { operation: OperationId, id: SessionId, result: Result<Arc<dyn HarnessSession>, String>, prompt: Option<Prompt> },
    /// A harness call on the active session returned: sending, interrupting,
    /// or stopping it to restart on a new approval mode.
    Done(Result<(), String>),
    /// The harness of a closed or discarded session stopped; no turn of the
    /// active session depends on it.
    ShutDown(Result<(), String>),
}

/// What the app reacts to.
#[derive(Clone)]
pub enum RuntimeEvent {
    /// Add every area's part to `config` and dispatch [`Command::Prepared`].
    /// `resume` when the session continues on a fresh harness.
    PrepareSession { operation: OperationId, config: Box<SessionConfig>, harness: String, resume: bool },
    Session(SessionEvent),
    Notice(String),
    /// A new session's harness failed to start; the session is gone.
    StartFailed(SessionId),
    /// A resume failed for good; the prompt is dropped.
    ResumeFailed(SessionId),
    /// Continue the active session in a new one, in `folder` when it is set.
    ContinueRequested { folder: Option<PathBuf>, prompt: Prompt },
    /// A started, spawned or resumed session runs: send `prompt` to it, as
    /// any other prompt, with [`Command::Send`].
    SendRequested { id: SessionId, prompt: Prompt },
    /// A started or spawned session runs and is the active one.
    SessionStarted(SessionId),
    /// The active session started a turn.
    TurnStarted(SessionId),
    /// The active session's turn ended.
    TurnEnded(SessionId),
    /// A session's sidebar entry changed ([`crate::session::Session::summary`]).
    SummaryChanged(SessionId),
    /// A turn of the active session changed status: the top bar follows.
    StatusChanged(SessionId),
    /// A prompt that couldn't be sent goes back to the composer, which stops waiting for its
    /// turn.
    PromptReturned(Prompt),
}

/// Launches a prepared operation with its full config.
type Launch = Box<dyn FnOnce(OperationId, SessionConfig) -> Effect + Send>;

/// An operation between [`RuntimeEvent::PrepareSession`] and its [`Completion`].
pub(crate) struct Pending {
    session: SessionId,
    /// `None` once launched.
    launch: Option<Launch>,
}

/// The operations in flight and the id of the next one.
#[derive(Default)]
pub(crate) struct Operations {
    pending: HashMap<OperationId, Pending>,
    next: OperationId,
}

/// A frame of feed events, applied one at a time so reactions see the post-patch DOM.
/// Drain with [`Runtime::advance_feed`] before processing another feed.
pub struct FeedBatch {
    id: SessionId,
    synced: Option<Arc<Dom>>,
    events: std::vec::IntoIter<loa_bus::Event>,
    active: bool,
    was_busy: bool,
    finished: bool,
}

impl Runtime {
    /// Applies `command` and returns the work and events it leads to.
    pub fn dispatch(&mut self, command: Command) -> Transition {
        let mut out = Transition::default();
        match command {
            Command::Start { prompt } => self.start(prompt, &mut out),
            Command::Open(id) => self.open(id, &mut out),
            Command::Send(prompt) => self.send(prompt, &mut out),
            Command::Spawn(spawn) => self.spawn(spawn, &mut out),
            Command::Interrupt => self.interrupt(&mut out),
            Command::Close { id, delete } => self.close(&id, delete, &mut out),
            Command::SetAccess(id) => self.change_access(id, &mut out),
            Command::Prepared { operation, config } => {
                if let Some(launch) = self.operations.pending.get_mut(&operation).and_then(|pending| pending.launch.take()) {
                    out.effects.push(launch(operation, *config));
                }
            }
            Command::Completed(completion) => self.complete(completion, &mut out),
        }
        out
    }

    /// The config of a session on `harness`, resumed from `dom` when given,
    /// before the app's areas add theirs.
    fn base_config(&self, session_id: &str, harness: &str, dom: Option<&Dom>) -> SessionConfig {
        let attr = |name| dom.and_then(Dom::root).and_then(|root| root.str_attr(name));
        SessionConfig {
            session_id: session_id.to_owned(),
            node: self.node.clone(),
            cwd: attr(attr::CWD).map_or_else(|| self.cwd.clone(), PathBuf::from),
            project: attr(attr::PROJECT).map(str::to_owned).or_else(|| self.project.clone()),
            mode: Some(attr(attr::MODE).and_then(SessionMode::parse).unwrap_or(SessionMode::Normal)),
            executable: self.harnesses.get(harness).and_then(|entry| entry.executable.clone()),
            approval_mode: self.access.clone(),
            ..SessionConfig::default()
        }
    }

    /// Records an operation for `session` and asks the app to complete its config.
    fn prepare(&mut self, session: SessionId, harness: String, resume: bool, config: SessionConfig, launch: Launch, out: &mut Transition) {
        let operation = self.operations.next;
        self.operations.next += 1;
        self.operations.pending.insert(operation, Pending { session, launch: Some(launch) });
        out.events.push(RuntimeEvent::PrepareSession { operation, config: Box::new(config), harness, resume });
    }

    fn start(&mut self, prompt: Option<Prompt>, out: &mut Transition) {
        if self.sessions.starting().is_some() {
            return give_back("A session is already starting".to_owned(), prompt, out);
        }
        let Some(harness) = self.sessions.harness.clone() else {
            return give_back("No harness is available on this machine".to_owned(), prompt, out);
        };
        let Some(adapter) = self.harnesses.runnable(&harness) else {
            return give_back(format!("{} is unavailable", self.harnesses.label(&harness)), prompt, out);
        };
        let (id, writer) = match self.create_session() {
            Ok(created) => created,
            Err(error) => return give_back(error, prompt, out),
        };
        self.sessions.set_starting(Some(id.clone()));
        let config = self.base_config(id.as_str(), &harness, None);
        let session = id.clone();
        let launch: Launch = Box::new(move |operation, config| {
            Box::pin(async move {
                let sink: Arc<dyn PatchSink> = writer;
                let result = adapter.start(config, sink).await.map(Arc::from).map_err(|error| error.to_string());
                Completion::Started { operation, id, result, prompt }
            })
        });
        self.prepare(session, harness, false, config, launch, out);
    }

    /// Resumes the active session on its harness, then sends `prompt`; else
    /// continues in a new session.
    fn resume(&mut self, prompt: Prompt, out: &mut Transition) {
        if self.sessions.starting().is_some() {
            return give_back("A session is already starting".to_owned(), Some(prompt), out);
        }
        let Some(session) = self.sessions.active() else { return };
        let id = session.id().clone();
        let harness = session.harness_id().map(str::to_owned);
        let adapter = harness.as_deref().and_then(|harness| self.harnesses.runnable(harness));
        let (Some(harness), Some(adapter)) = (harness, adapter) else {
            out.events.push(RuntimeEvent::Notice("This session's harness isn't available here; continuing in a new session".to_owned()));
            return self.start(Some(prompt), out);
        };
        let dom = session.dom().clone();
        let sink: Arc<dyn PatchSink> = Arc::clone(session.writer()) as _;
        let config = self.base_config(id.as_str(), &harness, Some(&dom));
        self.sessions.set_starting(Some(id.clone()));
        self.sessions.harness = Some(harness.clone());
        let session = id.clone();
        let launch: Launch = Box::new(move |operation, config| {
            Box::pin(async move {
                let result = adapter.resume(config, &dom, sink).await.map(Arc::from).map_err(ResumeError::from);
                Completion::Resumed { operation, id, result, prompt }
            })
        });
        self.prepare(session, harness, true, config, launch, out);
    }

    /// The active session's folder when it still exists and isn't the folder new sessions run in.
    fn continue_in_new_session(&self, prompt: Prompt, out: &mut Transition) {
        let folder = self.sessions.active().and_then(|session| session_cwd(session.dom())).filter(|folder| folder.is_dir() && *folder != self.cwd);
        out.events.push(RuntimeEvent::ContinueRequested { folder, prompt });
    }

    /// Sends `prompt` to the active session, resuming it first when no harness runs it.
    fn send(&mut self, prompt: Prompt, out: &mut Transition) {
        let Some(session) = self.sessions.active_mut() else { return };
        let Some(harness) = session.harness().cloned() else { return self.resume(prompt, out) };
        session.set_busy(true);
        out.events.push(RuntimeEvent::TurnStarted(session.id().clone()));
        out.effects.push(Box::pin(async move { Completion::Done(harness.send(prompt).await.map_err(|error| error.to_string())) }));
    }

    fn spawn(&mut self, spawn: Spawn, out: &mut Transition) {
        if self.sessions.starting().is_some() {
            out.events.push(RuntimeEvent::Notice("A session is already starting".to_owned()));
            return;
        }
        let Spawn { id, harness, start, prompt } = spawn;
        let config = self.base_config(id.as_str(), &harness, None);
        self.sessions.set_starting(Some(id.clone()));
        let session = id.clone();
        let launch: Launch = Box::new(move |operation, config| {
            Box::pin(async move {
                let result = start(config).await;
                Completion::Spawned { operation, id, result, prompt }
            })
        });
        self.prepare(session, harness, false, config, launch, out);
    }

    fn interrupt(&mut self, out: &mut Transition) {
        let Some(session) = self.sessions.active() else { return };
        let Some(harness) = session.harness().filter(|_| session.is_busy()).cloned() else { return };
        out.effects.push(Box::pin(async move { Completion::Done(harness.interrupt().await.map_err(|error| error.to_string())) }));
    }

    /// Shows session `id`, loading it from its journal when needed.
    fn open(&mut self, id: SessionId, out: &mut Transition) {
        if self.sessions.is_active(id.as_str()) {
            return;
        }
        if let Err(error) = self.load_session(id.as_str()) {
            out.events.push(RuntimeEvent::Notice(error));
            return;
        }
        self.sessions.set_active(Some(id.clone()));
        out.events.push(RuntimeEvent::Session(SessionEvent::Activated(Some(id))));
    }

    /// Closes session `id`; its operations in flight are dropped and its harness stops.
    fn close(&mut self, id: &SessionId, delete: bool, out: &mut Transition) {
        self.operations.pending.retain(|_, pending| pending.session != *id);
        let was_active = self.sessions.is_active(id.as_str());
        let (session, error) = self.close_session(id.as_str(), delete);
        if let Some(error) = error {
            out.events.push(RuntimeEvent::Notice(error));
        }
        let Some(session) = session else { return };
        if was_active {
            out.events.push(RuntimeEvent::Session(SessionEvent::Activated(None)));
        }
        out.events.push(RuntimeEvent::Session(SessionEvent::Closed(session.id().clone())));
        if let Some(harness) = session.harness().cloned() {
            out.effects.push(release(harness));
        }
    }

    /// Begins a feed frame without applying its patches yet.
    pub fn begin_feed(&self, id: SessionId, feed: Feed) -> FeedBatch {
        let active = self.sessions.is_active(id.as_str());
        let was_busy = active && self.sessions.active().is_some_and(|session| session.is_busy());
        let (synced, events, finished) = match feed {
            Feed::Synced(dom) => (Some(dom), Vec::new(), false),
            Feed::Events(events) => (None, events, false),
            Feed::Ended => (None, Vec::new(), true),
        };
        FeedBatch { id, synced, events: events.into_iter(), active, was_busy, finished }
    }

    /// Applies the next event, or finishes the frame's once-only turn reaction.
    /// React to the returned transition before calling again: its DOM is valid only until
    /// the next patch. `None` means the frame is fully drained.
    pub fn advance_feed(&mut self, batch: &mut FeedBatch) -> Option<Transition> {
        if batch.finished {
            return None;
        }
        let mut out = Transition::default();
        if let Some(dom) = batch.synced.take() {
            let change = self.sessions.sync(batch.id.as_str(), dom);
            self.changed(&batch.id, change, &mut out);
        } else if let Some(event) = batch.events.next() {
            let change = self.sessions.apply(batch.id.as_str(), event);
            self.changed(&batch.id, change, &mut out);
        } else {
            batch.finished = true;
            let now_busy = batch.active && self.sessions.active().is_some_and(|session| session.is_busy());
            if now_busy && !batch.was_busy {
                out.events.push(RuntimeEvent::TurnStarted(batch.id.clone()));
            }
            if batch.was_busy && !now_busy {
                if self.restart_for_access {
                    self.restart(&mut out);
                }
                out.events.push(RuntimeEvent::TurnEnded(batch.id.clone()));
            }
        }
        Some(out)
    }

    /// The events of one change of session `id`: its sidebar entry and status first, then the
    /// patch, then the event it stands for.
    fn changed(&self, id: &SessionId, change: Change, out: &mut Transition) {
        match change {
            Change::None => {}
            Change::Loaded => out.events.push(RuntimeEvent::Session(SessionEvent::Loaded(id.clone()))),
            Change::Patch { patch, derived, summary_changed, turn_changed } => {
                if summary_changed {
                    out.events.push(RuntimeEvent::SummaryChanged(id.clone()));
                }
                if turn_changed && self.sessions.is_active(id.as_str()) {
                    out.events.push(RuntimeEvent::StatusChanged(id.clone()));
                }
                out.events.push(RuntimeEvent::Session(SessionEvent::Patch(id.clone(), patch)));
                out.events.extend(derived.map(RuntimeEvent::Session));
            }
        }
    }

    fn complete(&mut self, completion: Completion, out: &mut Transition) {
        let operation = match &completion {
            Completion::Started { operation, .. } | Completion::Resumed { operation, .. } | Completion::Spawned { operation, .. } => *operation,
            Completion::Done(result) => {
                if let Err(error) = result {
                    if let Some(session) = self.sessions.active_mut() {
                        session.set_busy(false);
                    }
                    out.events.push(RuntimeEvent::Notice(error.clone()));
                }
                return;
            }
            Completion::ShutDown(result) => {
                if let Err(error) = result {
                    out.events.push(RuntimeEvent::Notice(error.clone()));
                }
                return;
            }
        };
        if self.operations.pending.remove(&operation).is_none() {
            // Its session closed meanwhile: stop what started for it.
            let harness = match completion {
                Completion::Started { result, .. } | Completion::Spawned { result, .. } => result.ok(),
                Completion::Resumed { result, .. } => result.ok(),
                Completion::Done(_) | Completion::ShutDown(_) => None,
            };
            out.effects.extend(harness.map(release));
            return;
        }
        match completion {
            Completion::Started { id, result: Ok(harness), prompt, .. } => self.started(id, harness, prompt, out),
            Completion::Started { id, result: Err(error), .. } => {
                self.sessions.set_starting(None);
                // Nothing ran: drop the journal it was given.
                let (_, closed) = self.close_session(id.as_str(), true);
                out.events.push(RuntimeEvent::Notice(closed.map_or_else(|| error.clone(), |closed| format!("{error}; {closed}"))));
                out.events.push(RuntimeEvent::StartFailed(id));
            }
            Completion::Spawned { id, result: Ok(harness), prompt, .. } => match self.load_session(id.as_str()) {
                Ok(()) => self.started(id, harness, prompt, out),
                Err(error) => {
                    self.sessions.set_starting(None);
                    out.events.push(RuntimeEvent::Notice(error));
                    out.effects.push(release(harness));
                }
            },
            Completion::Spawned { id, result: Err(error), .. } => {
                self.sessions.set_starting(None);
                // Leave no half created session behind; it may not exist.
                let _ = self.library.delete(id.as_str());
                out.events.push(RuntimeEvent::Notice(error));
            }
            Completion::Resumed { id, result, prompt, .. } => {
                self.sessions.set_starting(None);
                match result {
                    Ok(harness) => {
                        let Some(session) = self.sessions.get_mut(id.as_str()) else {
                            out.effects.push(release(harness));
                            return;
                        };
                        session.attach(harness);
                        self.open(id.clone(), out);
                        out.events.push(RuntimeEvent::SendRequested { id, prompt });
                    }
                    Err(ResumeError::Unsupported(reason)) => {
                        out.events.push(RuntimeEvent::Notice(format!("Can't resume this session ({reason}); continuing in a new session")));
                        self.continue_in_new_session(prompt, out);
                    }
                    // omp no longer has the session's own file: carry on in a new session.
                    Err(ResumeError::Failed(error)) if error.contains("not found") => {
                        out.events.push(RuntimeEvent::Notice("This session's harness history is gone; continuing in a new session".to_owned()));
                        self.continue_in_new_session(prompt, out);
                    }
                    Err(ResumeError::Failed(error)) => {
                        out.events.push(RuntimeEvent::Notice(error));
                        out.events.push(RuntimeEvent::ResumeFailed(id));
                    }
                }
            }
            Completion::Done(_) | Completion::ShutDown(_) => {}
        }
    }

    /// A started or spawned session runs: it becomes the active one.
    fn started(&mut self, id: SessionId, harness: Arc<dyn HarnessSession>, prompt: Option<Prompt>, out: &mut Transition) {
        self.sessions.set_starting(None);
        // Discarded while starting.
        let Some(session) = self.sessions.get_mut(id.as_str()) else {
            out.effects.push(release(harness));
            return;
        };
        session.attach(harness);
        self.sessions.set_active(Some(id.clone()));
        out.events.push(RuntimeEvent::Session(SessionEvent::Activated(Some(id.clone()))));
        out.events.push(RuntimeEvent::SessionStarted(id.clone()));
        if let Some(prompt) = prompt {
            out.events.push(RuntimeEvent::SendRequested { id, prompt });
        }
    }

    /// A permission level was picked: new sessions start on it, the open one restarts on it.
    fn change_access(&mut self, id: String, out: &mut Transition) {
        if id == self.access.as_deref().unwrap_or(ACCESS_DEFAULT) {
            return;
        }
        let label = access_label(&id);
        self.access = Some(id);
        let running = self.sessions.active().is_some_and(|session| session.harness().is_some());
        if running && self.sessions.active().is_some_and(|session| session.is_busy()) {
            self.restart_for_access = true;
            out.events.push(RuntimeEvent::Notice(format!("{label}: applies once this turn ends")));
            return;
        }
        out.events.push(RuntimeEvent::Notice(format!("{label}: applies from your next message")));
        if running {
            self.restart(out);
        }
    }

    /// Stops the active session's harness; the next send resumes it on a new one.
    fn restart(&mut self, out: &mut Transition) {
        self.restart_for_access = false;
        out.effects.extend(self.sessions.active_mut().and_then(|session| session.detach()).map(shutdown));
    }
}

/// Says why a start or resume can't happen and gives its prompt back to the composer. Without
/// a prompt (a new session) the composer is left as it is: a turn may run elsewhere.
fn give_back(notice: String, prompt: Option<Prompt>, out: &mut Transition) {
    out.events.push(RuntimeEvent::Notice(notice));
    out.events.extend(prompt.map(RuntimeEvent::PromptReturned));
}

/// Stops the active session's harness; a failure ends its turn.
fn shutdown(harness: Arc<dyn HarnessSession>) -> Effect {
    Box::pin(async move { Completion::Done(harness.shutdown().await.map_err(|error| error.to_string())) })
}

/// Stops the harness of a session that is closed or was never shown.
fn release(harness: Arc<dyn HarnessSession>) -> Effect {
    Box::pin(async move { Completion::ShutDown(harness.shutdown().await.map_err(|error| error.to_string())) })
}

/// The folder a session works in, from its DOM.
pub fn session_cwd(dom: &Dom) -> Option<PathBuf> {
    dom.root()?.str_attr(attr::CWD).map(PathBuf::from)
}
