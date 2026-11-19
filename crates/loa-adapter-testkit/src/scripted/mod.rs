//! `ScriptedAdapter`: a fake harness that plays a script of events into the
//! sink. It tests the testkit itself and is the reference implementation of
//! "DOM conventions for adapters": every check in [`crate::check`] holds for
//! what it writes.
//!
//! Live sessions play one script per `send`. Its native session format is a
//! JSONL file of the same events with timestamps (see [`native`]), so it also
//! exercises import, export and the round trip.

pub mod native;
mod writer;

use async_trait::async_trait;
use loa_adapter::{
    Adapter, AdapterError, Answer, Capabilities, HarnessSession, Prompt, Result, SessionConfig, SessionState,
};
use loa_core::{
    DecisionKind, Dom, ElementKind, MessageStatus, NodeId, PatchSink, SCHEMA_VERSION, TurnStatus, attr, now_ms,
};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::{oneshot, watch};
use writer::{AskSpec, SessionHeader, Writer};

/// `<session harness>` of scripted sessions.
pub const HARNESS: &str = "scripted";

/// A decision the scripted harness raises.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub kind: DecisionKind,
    pub request_id: String,
    pub title: Option<String>,
    pub options: Vec<String>,
    /// Decision text: the command, question or plan.
    pub body: String,
}

/// One event the scripted harness reports during a turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Assistant text (or thinking) starts streaming.
    MessageStart { thinking: bool },
    /// A token of the streaming message.
    Token(String),
    MessageEnd,
    /// A tool call starts. `args` is JSON, possibly cut short and finished by
    /// [`Step::CallArgs`].
    CallStart { tool: String, call_id: String, title: Option<String>, args: String },
    /// More streamed arguments of the open call.
    CallArgs(String),
    CallEnd { output: String, is_error: bool },
    /// Raises a decision under the open call (or the turn) and blocks until
    /// it is answered or the turn is interrupted.
    Ask(Ask),
    Usage { input_tokens: i64, output_tokens: i64 },
    /// Harness data with no shared element.
    Ext { kind: String, raw: String },
    /// Blocks until the turn is interrupted.
    Hold,
    /// The harness fails the turn with this message.
    Fail(String),
}

impl Step {
    /// A complete assistant message streamed one word at a time.
    pub fn say(text: &str) -> Vec<Step> {
        Self::stream(false, text)
    }

    /// A complete thinking message streamed one word at a time.
    pub fn think(text: &str) -> Vec<Step> {
        Self::stream(true, text)
    }

    fn stream(thinking: bool, text: &str) -> Vec<Step> {
        let mut steps = vec![Step::MessageStart { thinking }];
        steps.extend(text.split_inclusive(' ').map(|t| Step::Token(t.to_owned())));
        steps.push(Step::MessageEnd);
        steps
    }
}

/// The script of one turn.
pub type Script = Vec<Step>;

/// A harness adapter whose sessions play scripts instead of running a program.
#[derive(Clone, Debug, Default)]
pub struct ScriptedAdapter {
    turns: Vec<Script>,
}

impl ScriptedAdapter {
    /// Every session started from this adapter plays `turns[0]` on the first
    /// `send`, `turns[1]` on the second, and so on.
    pub fn new(turns: Vec<Script>) -> Self {
        Self { turns }
    }

    fn session(&self, config: &SessionConfig, sink: Arc<dyn PatchSink>, session: NodeId) -> ScriptedSession {
        let (state, _) = watch::channel(SessionState::Idle);
        ScriptedSession {
            shared: Arc::new(Shared {
                sink,
                session,
                model: config.model.clone(),
                scripts: Mutex::new(self.turns.iter().cloned().collect()),
                state,
                control: Mutex::new(Control::default()),
                failure: Mutex::new(None),
            }),
        }
    }
}

#[async_trait]
impl Adapter for ScriptedAdapter {
    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: SCHEMA_VERSION,
            decisions: DecisionKind::ALL.to_vec(),
            import: true,
            export: true,
            ..Capabilities::default()
        }
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let header = SessionHeader {
            id: &config.session_id,
            harness: HARNESS,
            node: &config.node,
            created: now_ms(),
            cwd: config.cwd.to_str().filter(|c| !c.is_empty()),
            project: config.project.as_deref(),
            model: config.model.as_deref(),
            model_role: config.model_role.as_deref(),
            effort: config.effort.as_deref(),
            mode: config.mode,
        };
        let session = Writer::new(&*sink).session(&header)?;
        Ok(Box::new(self.session(&config, sink, session)))
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let root = dom.root().filter(|r| r.kind == ElementKind::Session && r.str_attr(attr::HARNESS) == Some(HARNESS));
        let Some(root) = root else {
            return Err(AdapterError::Format("not a scripted session".to_owned()));
        };
        Ok(Box::new(self.session(&config, sink, root.id)))
    }

    fn import(&self, native: &Path, sink: &dyn PatchSink) -> Result<()> {
        native::import(native, sink)
    }

    fn export(&self, dom: &Dom, out: &Path) -> Result<()> {
        native::export(dom, out)
    }
}

/// A decision the player is blocked on.
struct Pending {
    decision: NodeId,
    turn: NodeId,
    resume: oneshot::Sender<()>,
}

#[derive(Default)]
struct Control {
    pending: Option<Pending>,
    /// Interrupts the running turn.
    cancel: Option<watch::Sender<bool>>,
}

struct Shared {
    sink: Arc<dyn PatchSink>,
    session: NodeId,
    model: Option<String>,
    scripts: Mutex<VecDeque<Script>>,
    state: watch::Sender<SessionState>,
    control: Mutex<Control>,
    /// Why the last turn couldn't be written, reported by `wait_idle`.
    failure: Mutex<Option<String>>,
}

/// Recovers a poisoned lock: every critical section here leaves the guarded
/// value consistent before anything that can panic.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A live scripted session.
pub struct ScriptedSession {
    shared: Arc<Shared>,
}

#[async_trait]
impl HarnessSession for ScriptedSession {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        let shared = &self.shared;
        // Holding the control lock makes the idle check and the turn start one
        // step, so two concurrent sends can't both start a turn.
        let mut control = lock(&shared.control);
        let state = *shared.state.borrow();
        match state {
            SessionState::Idle => {}
            SessionState::Closed => return Err(AdapterError::Closed),
            SessionState::Running | SessionState::Waiting => return Err(AdapterError::Busy),
        }
        let Some(script) = lock(&shared.scripts).pop_front() else {
            return Err(AdapterError::Protocol("the script has no turn left".to_owned()));
        };
        let turn = Writer::new(&*shared.sink).turn(shared.session, None, &prompt.text, Some(&prompt.by), now_ms())?;
        let (cancel, cancelled) = watch::channel(false);
        control.cancel = Some(cancel);
        *lock(&shared.failure) = None;
        shared.state.send_replace(SessionState::Running);
        drop(control);
        tokio::spawn(play(Arc::clone(shared), turn, script, cancelled));
        Ok(())
    }

    async fn interrupt(&self) -> Result<()> {
        if let Some(cancel) = &lock(&self.shared.control).cancel {
            cancel.send_replace(true);
        }
        self.wait_idle().await
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        let shared = &self.shared;
        let mut control = lock(&shared.control);
        let Some(pending) = control.pending.take_if(|p| p.decision == decision) else {
            return Err(AdapterError::UnknownDecision(decision));
        };
        let written = Writer::new(&*shared.sink).answered(
            decision,
            pending.turn,
            &answer.values,
            Some(&answer.by),
            answer.reason.as_deref(),
            now_ms(),
        );
        shared.state.send_replace(SessionState::Running);
        // The player may have stopped waiting already; it then ends the turn itself.
        let _ = pending.resume.send(());
        drop(control);
        written.map_err(AdapterError::from)
    }

    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn state(&self) -> SessionState {
        *self.shared.state.borrow()
    }

    async fn wait_idle(&self) -> Result<()> {
        let mut state = self.shared.state.subscribe();
        // The sender lives in `shared`, which `self` keeps alive.
        let _ = state.wait_for(|s| matches!(s, SessionState::Idle | SessionState::Closed)).await;
        match lock(&self.shared.failure).take() {
            Some(why) => Err(AdapterError::Protocol(why)),
            None => Ok(()),
        }
    }

    async fn shutdown(&self) -> Result<()> {
        self.interrupt().await?;
        self.shared.state.send_replace(SessionState::Closed);
        Ok(())
    }
}

/// What the player has open in the running turn.
struct Open {
    turn: NodeId,
    message: Option<NodeId>,
    call: Option<(NodeId, NodeId)>,
}

/// How a script ended.
enum End {
    Done,
    Cancelled,
    Failed(String),
}

async fn play(shared: Arc<Shared>, turn: NodeId, script: Script, mut cancel: watch::Receiver<bool>) {
    let mut open = Open { turn, message: None, call: None };
    let end = run(&shared, &mut open, script, &mut cancel).await;
    let written = match end {
        Ok(End::Done) => Writer::new(&*shared.sink).end_turn(turn, TurnStatus::Done, now_ms()).map_err(|e| e.to_string()),
        Ok(End::Cancelled) => cancel_turn(&shared, &open).map_err(|e| e.to_string()),
        Ok(End::Failed(why)) | Err(why) => fail_turn(&shared, &open, &why).map_err(|e| e.to_string()),
    };
    if let Err(why) = written {
        *lock(&shared.failure) = Some(why);
    }
    lock(&shared.control).cancel = None;
    shared.state.send_if_modified(|s| {
        let running = matches!(s, SessionState::Running | SessionState::Waiting);
        if running {
            *s = SessionState::Idle;
        }
        running
    });
}

/// Plays `script`. `Err` is a script that breaks its own rules (a token with
/// no open message) or a commit that failed.
async fn run(
    shared: &Shared,
    open: &mut Open,
    script: Script,
    cancel: &mut watch::Receiver<bool>,
) -> std::result::Result<End, String> {
    let w = Writer::new(&*shared.sink);
    for step in script {
        // Lets an interrupt land between events, as it would between two
        // lines read from a real harness.
        tokio::task::yield_now().await;
        if *cancel.borrow() {
            return Ok(End::Cancelled);
        }
        let ts = now_ms();
        let e = |e: loa_core::CommitError| e.to_string();
        match step {
            Step::MessageStart { thinking } => {
                if open.message.is_some() {
                    return Err("a message starts while one streams".to_owned());
                }
                let model = shared.model.as_deref();
                open.message = Some(w.message(open.turn, thinking, model, MessageStatus::Streaming, "", ts).map_err(e)?);
            }
            Step::Token(text) => {
                let message = open.message.ok_or("a token with no open message")?;
                w.append(message, &text, ts).map_err(e)?;
            }
            Step::MessageEnd => {
                let message = open.message.take().ok_or("a message end with no open message")?;
                w.status(message, MessageStatus::Done, ts).map_err(e)?;
            }
            Step::CallStart { tool, call_id, title, args } => {
                if open.call.is_some() {
                    return Err("a call starts while one runs".to_owned());
                }
                open.call = Some(w.call(open.turn, &tool, &call_id, title.as_deref(), &args, ts).map_err(e)?);
            }
            Step::CallArgs(text) => {
                let (_, input) = open.call.ok_or("arguments with no open call")?;
                w.append(input, &text, ts).map_err(e)?;
            }
            Step::CallEnd { output, is_error } => {
                let (call, _) = open.call.take().ok_or("a call end with no open call")?;
                w.call_end(call, &output, is_error, ts).map_err(e)?;
            }
            Step::Ask(ask) => {
                let resumed = {
                    let mut control = lock(&shared.control);
                    let call = open.call.map(|(c, _)| c);
                    let spec = AskSpec {
                        kind: ask.kind,
                        request_id: &ask.request_id,
                        title: ask.title.as_deref(),
                        options: &ask.options,
                        body: &ask.body,
                    };
                    let decision = w.ask(call.unwrap_or(open.turn), open.turn, call, &spec, ts).map_err(e)?;
                    let (resume, resumed) = oneshot::channel();
                    control.pending = Some(Pending { decision, turn: open.turn, resume });
                    shared.state.send_replace(SessionState::Waiting);
                    resumed
                };
                tokio::select! {
                    answered = resumed => if answered.is_err() {
                        return Err("the pending decision was dropped".to_owned());
                    },
                    _ = cancel.wait_for(|c| *c) => return Ok(End::Cancelled),
                }
            }
            Step::Usage { input_tokens, output_tokens } => {
                w.usage(open.turn, input_tokens, output_tokens, ts).map_err(e)?;
            }
            Step::Ext { kind, raw } => {
                let parent = open.call.map_or(open.turn, |(c, _)| c);
                w.ext(parent, HARNESS, &kind, &raw, ts).map_err(e)?;
            }
            Step::Hold => {
                // Only an interrupt ends a hold; a dropped sender means the session is gone.
                let _ = cancel.wait_for(|c| *c).await;
                return Ok(End::Cancelled);
            }
            Step::Fail(why) => return Ok(End::Failed(why)),
        }
    }
    if open.message.is_some() || open.call.is_some() {
        return Err("the script ended with a message or call still open".to_owned());
    }
    Ok(End::Done)
}

/// Interrupt: the streaming message, the running call and the waiting
/// decision become cancelled, then the turn does.
fn cancel_turn(shared: &Shared, open: &Open) -> std::result::Result<(), loa_core::CommitError> {
    let w = Writer::new(&*shared.sink);
    let ts = now_ms();
    let mut control = lock(&shared.control);
    if let Some(message) = open.message {
        w.status(message, MessageStatus::Cancelled, ts)?;
    }
    if let Some((call, _)) = open.call {
        w.cancel_with_end(call, ts)?;
    }
    // Still pending means nobody answered before the interrupt.
    if let Some(pending) = control.pending.take() {
        w.cancel_with_end(pending.decision, ts)?;
    }
    w.end_turn(open.turn, TurnStatus::Cancelled, ts)
}

/// The harness failed the turn: open elements end in error, the reason is
/// kept in an `<ext type="error">`, then the turn ends in error.
fn fail_turn(shared: &Shared, open: &Open, why: &str) -> std::result::Result<(), loa_core::CommitError> {
    let w = Writer::new(&*shared.sink);
    let ts = now_ms();
    let mut control = lock(&shared.control);
    if let Some(message) = open.message {
        w.status(message, MessageStatus::Error, ts)?;
    }
    if let Some((call, _)) = open.call {
        w.call_end(call, why, true, ts)?;
    }
    if let Some(pending) = control.pending.take() {
        w.cancel_with_end(pending.decision, ts)?;
    }
    w.ext(open.turn, HARNESS, "error", why, ts)?;
    w.end_turn(open.turn, TurnStatus::Error, ts)
}
