//! The omp2 process driver: one `omp --mode rpc` child per session, spoken
//! to with one JSON object per line.

use loa_util::LockExt;
use crate::translate::{TranslateError, Translator};
use crate::wire::{self, DomEvent, Frame, Line};
use crate::HARNESS;
use async_trait::async_trait;
use base64::Engine as _;
use loa_contracts::harness::{AdapterError, Answer, HarnessModel, HarnessSession, ModelSelection, Prompt, Result, SessionState};
use loa_core::{Attrs, DecisionKind, DecisionStatus, Draft, ElementKind, NodeId, PatchSink, TurnStatus, Value, answer, attr, now_ms};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

/// A reply to one request line.
#[derive(Debug)]
struct Reply {
    success: bool,
    data: serde_json::Value,
    error: Option<String>,
    code: Option<String>,
}

/// A `<decision>` waiting on omp2.
enum Pending {
    Approval { prompt_id: String },
    Select { ui_id: String, options: Vec<String> },
}

struct Shared {
    translator: Translator,
    decisions: HashMap<NodeId, Pending>,
    replies: HashMap<String, oneshot::Sender<Reply>>,
}

struct Inner {
    sink: Arc<dyn PatchSink>,
    shared: Mutex<Shared>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    state: watch::Sender<SessionState>,
    next_id: AtomicU64,
}

impl Inner {
    fn set_state(&self, state: SessionState) {
        self.state.send_if_modified(|current| {
            let changed = *current != state && *current != SessionState::Closed;
            if changed {
                *current = state;
            }
            changed
        });
    }

    async fn write(&self, line: &str) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        let pipe = stdin.as_mut().ok_or(AdapterError::Closed)?;
        pipe.write_all(line.as_bytes()).await?;
        pipe.flush().await?;
        Ok(())
    }

    /// Writes the line `make` builds for a fresh request id and waits for
    /// omp2's response to it.
    async fn request(&self, make: impl FnOnce(&str) -> String) -> Result<Reply> {
        let id = format!("loa-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.shared.locked().replies.insert(id.clone(), tx);
        if let Err(error) = self.write(&make(&id)).await {
            self.shared.locked().replies.remove(&id);
            return Err(error);
        }
        rx.await.map_err(|_| AdapterError::Closed)
    }

    /// Like [`Inner::request`], turning a failure response into an error.
    async fn call(&self, make: impl FnOnce(&str) -> String) -> Result<serde_json::Value> {
        let reply = self.request(make).await?;
        if reply.success {
            return Ok(reply.data);
        }
        if reply.code.as_deref() == Some(wire::SESSION_BUSY) {
            return Err(AdapterError::Busy);
        }
        Err(AdapterError::Protocol(reply.error.unwrap_or_else(|| "omp2 refused the request".to_owned())))
    }

    fn commit(&self, draft: Draft) -> Result<NodeId> {
        Ok(self.sink.commit(draft)?.node)
    }

    /// Handles one stdout line.
    fn line(&self, line: &str) -> Result<(), TranslateError> {
        let ts = now_ms();
        match wire::parse_line(line) {
            Ok(Line::Dom(event)) => self.dom(event, line, ts),
            Ok(Line::Frame(frame)) => self.frame(frame, ts),
            Err(error) => self.keep(line, "unparsed", &error.to_string(), ts),
        }
    }

    fn dom(&self, event: DomEvent, line: &str, ts: i64) -> Result<(), TranslateError> {
        let result = self.shared.locked().translator.apply(self.sink.as_ref(), event, ts);
        match result {
            Err(TranslateError::Commit(error)) => Err(TranslateError::Commit(error)),
            Err(error) => self.keep(line, "untranslated", &error.to_string(), ts),
            Ok(()) => Ok(()),
        }
    }

    /// Keeps a line the adapter couldn't use as `<ext>` under the session.
    fn keep(&self, line: &str, kind: &str, error: &str, ts: i64) -> Result<(), TranslateError> {
        let root = self.shared.locked().translator.root();
        let attrs = [(attr::HARNESS, Value::from(HARNESS)), (attr::TYPE, Value::from(kind)), ("x-error", Value::from(error))];
        self.sink.commit_at(Draft::insert(root, ElementKind::Ext, attrs, line), ts)?;
        Ok(())
    }

    fn frame(&self, frame: Frame, ts: i64) -> Result<(), TranslateError> {
        let sink = self.sink.as_ref();
        match frame {
            Frame::Response { id, success, data, error, code } => {
                if let Some(tx) = self.shared.locked().replies.remove(&id) {
                    // The requester may have given up; nothing else wants it.
                    let _ = tx.send(Reply { success, data, error, code });
                }
            }
            Frame::AgentEnd { cancelled, error } => {
                let status = match (error.is_some(), cancelled) {
                    (true, _) => TurnStatus::Error,
                    (false, true) => TurnStatus::Cancelled,
                    (false, false) => TurnStatus::Done,
                };
                let mut shared = self.shared.locked();
                for (decision, _) in shared.decisions.drain() {
                    sink.commit_at(Draft::set(decision, attr::STATUS, DecisionStatus::Cancelled), ts)?;
                    sink.commit_at(Draft::set(decision, attr::ENDED, ts), ts)?;
                }
                if let (Some(error), Some(turn)) = (&error, shared.translator.current_turn()) {
                    let attrs = [(attr::HARNESS, Value::from(HARNESS)), (attr::TYPE, Value::from("error"))];
                    sink.commit_at(Draft::insert(turn, ElementKind::Ext, attrs, error.as_str()), ts)?;
                }
                shared.translator.finish_turn(sink, status, ts)?;
                drop(shared);
                self.set_state(SessionState::Idle);
            }
            Frame::Approval { prompt_id, call_id, title, body, timeout_ms } => {
                let mut shared = self.shared.locked();
                let call = call_id.as_deref().and_then(|c| shared.translator.call(c));
                let turn = shared.translator.current_turn();
                let parent = call.or(turn).unwrap_or(shared.translator.root());
                let mut attrs = Attrs::from([
                    (attr::KIND.to_owned(), Value::from(DecisionKind::Approval)),
                    (attr::STATUS.to_owned(), Value::from(DecisionStatus::Waiting)),
                    (attr::REQUEST_ID.to_owned(), Value::Str(prompt_id.clone())),
                    (attr::CREATED.to_owned(), Value::Int(ts)),
                ]);
                if let Some(call) = call {
                    attrs.insert(attr::CALL.to_owned(), Value::Int(i64::try_from(call.0).unwrap_or(i64::MAX)));
                }
                if let Some(title) = title {
                    attrs.insert(attr::TITLE.to_owned(), Value::Str(title));
                }
                if let Some(timeout) = timeout_ms {
                    attrs.insert(attr::TIMEOUT_MS.to_owned(), Value::Int(timeout));
                }
                let text = body.unwrap_or_default();
                let draft = Draft::Insert { parent, index: None, kind: ElementKind::Decision, attrs, text };
                let decision = sink.commit_at(draft, ts)?.node;
                shared.decisions.insert(decision, Pending::Approval { prompt_id });
                if let Some(turn) = turn {
                    sink.commit_at(Draft::set(turn, attr::STATUS, TurnStatus::Waiting), ts)?;
                }
                drop(shared);
                self.set_state(SessionState::Waiting);
            }
            Frame::Select { id, title, options, multi } => {
                let mut shared = self.shared.locked();
                let turn = shared.translator.current_turn();
                let parent = turn.unwrap_or(shared.translator.root());
                let attrs = Attrs::from([
                    (attr::KIND.to_owned(), Value::from(DecisionKind::Question)),
                    (attr::STATUS.to_owned(), Value::from(DecisionStatus::Waiting)),
                    (attr::REQUEST_ID.to_owned(), Value::Str(id.clone())),
                    (attr::OPTIONS.to_owned(), Value::from(options.clone())),
                    (attr::MULTI.to_owned(), Value::Bool(multi)),
                    (attr::CREATED.to_owned(), Value::Int(ts)),
                ]);
                let text = title.unwrap_or_default();
                let draft = Draft::Insert { parent, index: None, kind: ElementKind::Decision, attrs, text };
                let decision = sink.commit_at(draft, ts)?.node;
                shared.decisions.insert(decision, Pending::Select { ui_id: id, options });
                if let Some(turn) = turn {
                    sink.commit_at(Draft::set(turn, attr::STATUS, TurnStatus::Waiting), ts)?;
                }
                drop(shared);
                self.set_state(SessionState::Waiting);
            }
            Frame::Ready | Frame::Shutdown | Frame::Other => {}
        }
        Ok(())
    }
}

/// One running omp2 session.
pub struct Omp2Session {
    inner: Arc<Inner>,
    child: tokio::sync::Mutex<Child>,
    reader: Mutex<Option<JoinHandle<()>>>,
}

impl Omp2Session {
    /// Takes over a spawned `omp --mode rpc` child with piped stdin and
    /// stdout, subscribes to its DOM and returns once omp2 accepted that.
    /// Returns the session and omp2's own session id.
    pub(crate) async fn launch(
        mut child: Child,
        sink: Arc<dyn PatchSink>,
        translator: Translator,
    ) -> Result<(Self, Option<String>)> {
        let stdin = child.stdin.take().ok_or_else(|| AdapterError::Protocol("omp2 stdin isn't piped".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| AdapterError::Protocol("omp2 stdout isn't piped".into()))?;
        let (state, _) = watch::channel(SessionState::Idle);
        let inner = Arc::new(Inner {
            sink,
            shared: Mutex::new(Shared { translator, decisions: HashMap::new(), replies: HashMap::new() }),
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            state,
            next_id: AtomicU64::new(1),
        });
        let reader = tokio::spawn(read(inner.clone(), stdout));
        let session = Self { inner, child: tokio::sync::Mutex::new(child), reader: Mutex::new(Some(reader)) };
        let subscribed = session.inner.request(wire::subscribe_dom).await;
        match subscribed {
            Ok(reply) if reply.success => {}
            Ok(reply) => {
                // Shutting down is best effort; the version error is what matters.
                let _ = session.shutdown().await;
                let detail = reply.error.unwrap_or_else(|| format!("`{}` was refused", wire::DOM_SUBSCRIPTION));
                return Err(AdapterError::Version { harness: HARNESS, detail });
            }
            Err(error) => {
                let _ = session.shutdown().await;
                return Err(error);
            }
        }
        let state = session.inner.call(|id| wire::bare(id, "get_state")).await?;
        let native = wire::session_id(&state).map(str::to_owned);
        Ok((session, native))
    }

    fn state_now(&self) -> SessionState {
        *self.inner.state.borrow()
    }
}

/// Reads stdout until omp2 exits, then closes the session.
async fn read(inner: Arc<Inner>, stdout: ChildStdout) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        if inner.line(&line).is_err() {
            // Only a refused commit stops the reader: the session can no
            // longer be written.
            break;
        }
    }
    inner.state.send_replace(SessionState::Closed);
    let mut shared = inner.shared.locked();
    shared.replies.clear();
}

fn mime_of(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

#[async_trait]
impl HarnessSession for Omp2Session {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        match self.state_now() {
            SessionState::Running | SessionState::Waiting => return Err(AdapterError::Busy),
            SessionState::Closed => return Err(AdapterError::Closed),
            SessionState::Idle => {}
        }
        // omp2's RPC prompt carries images only.
        let mut images = Vec::with_capacity(prompt.attachments.len());
        for attachment in &prompt.attachments {
            let mime = attachment.mime.as_deref().or_else(|| mime_of(&attachment.path));
            let Some(mime) = mime.filter(|m| m.starts_with("image/")) else {
                return Err(AdapterError::Unsupported { harness: HARNESS, what: "attachments other than images" });
            };
            let bytes = tokio::fs::read(&attachment.path).await?;
            images.push((base64::engine::general_purpose::STANDARD.encode(bytes), mime.to_owned()));
        }
        let ts = now_ms();
        self.inner.shared.locked().translator.begin_turn(self.inner.sink.as_ref(), &prompt.text, &prompt.by, ts).map_err(translate_error)?;
        self.inner.set_state(SessionState::Running);
        let sent = self.inner.call(|id| wire::prompt(id, &prompt.text, images)).await;
        if let Err(error) = sent {
            let ts = now_ms();
            let sink = self.inner.sink.clone();
            self.inner.shared.locked().translator.finish_turn(sink.as_ref(), TurnStatus::Error, ts).map_err(translate_error)?;
            self.inner.set_state(SessionState::Idle);
            return Err(error);
        }
        Ok(())
    }

    async fn interrupt(&self) -> Result<()> {
        if !matches!(self.state_now(), SessionState::Running | SessionState::Waiting) {
            return Ok(());
        }
        self.inner.call(|id| wire::bare(id, "abort")).await?;
        Ok(())
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        let pending = self.inner.shared.locked().decisions.remove(&decision).ok_or(AdapterError::UnknownDecision(decision))?;
        let sent = match &pending {
            Pending::Approval { prompt_id } => {
                let (approved, scope) = match answer.values.first().map(String::as_str) {
                    Some(answer::ALLOW_ONCE) => (true, "once"),
                    Some(answer::ALLOW_SESSION) => (true, "session"),
                    _ => (false, "once"),
                };
                self.inner
                    .call(|id| wire::approve(id, prompt_id, approved, scope, answer.reason.as_deref()))
                    .await
                    .map(drop)
            }
            Pending::Select { ui_id, options } => {
                // Picked labels go in `values`; anything else is typed text.
                let (picked, typed): (Vec<String>, Vec<String>) =
                    answer.values.iter().cloned().partition(|v| options.contains(v));
                let custom = (!typed.is_empty()).then(|| typed.join("\n"));
                self.inner.write(&wire::select_response(ui_id, &picked, custom.as_deref())).await
            }
        };
        if let Err(error) = sent {
            self.inner.shared.locked().decisions.insert(decision, pending);
            return Err(error);
        }
        let ts = now_ms();
        let values: Vec<Value> = answer.values.into_iter().map(Value::Str).collect();
        let shared = self.inner.shared.locked();
        let sink = self.inner.sink.as_ref();
        sink.commit_at(Draft::set(decision, attr::ANSWER, values), ts)?;
        sink.commit_at(Draft::set(decision, attr::BY, answer.by), ts)?;
        if let Some(reason) = answer.reason {
            sink.commit_at(Draft::set(decision, attr::REASON, reason), ts)?;
        }
        sink.commit_at(Draft::set(decision, attr::STATUS, DecisionStatus::Answered), ts)?;
        sink.commit_at(Draft::set(decision, attr::ENDED, ts), ts)?;
        let waiting = !shared.decisions.is_empty();
        if let (false, Some(turn)) = (waiting, shared.translator.current_turn()) {
            sink.commit_at(Draft::set(turn, attr::STATUS, TurnStatus::Running), ts)?;
        }
        drop(shared);
        if !waiting {
            self.inner.set_state(SessionState::Running);
        }
        Ok(())
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        if selection.role.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "setting a model role over RPC" });
        }
        let Some((provider, model)) = selection.model.split_once('/') else {
            return Err(AdapterError::Protocol(format!("omp2 models are `provider/model`, got `{}`", selection.model)));
        };
        self.inner.call(|id| wire::set_model(id, provider, model)).await?;
        let root = self.inner.shared.locked().translator.root();
        self.inner.commit(Draft::set(root, attr::MODEL, selection.model))?;
        Ok(())
    }

    /// `get_available_models`; reasoning models take every omp2 thinking level.
    async fn models(&self) -> Result<Vec<HarnessModel>> {
        let data = self.inner.call(|id| wire::bare(id, "get_available_models")).await?;
        let models = wire::available_models(&data).map_err(|error| AdapterError::Protocol(format!("get_available_models: {error}")))?;
        Ok(models
            .into_iter()
            .map(|model| HarnessModel {
                id: format!("{}/{}", model.provider, model.id),
                name: if model.name.is_empty() { model.id.clone() } else { model.name },
                efforts: if model.reasoning { wire::THINKING_LEVELS.iter().map(|level| (*level).to_owned()).collect() } else { Vec::new() },
                provider: model.provider,
            })
            .collect())
    }

    /// `set_thinking_level`, recorded as the session's effort.
    async fn set_effort(&self, effort: String) -> Result<()> {
        self.inner.call(|id| wire::set_thinking(id, &effort)).await?;
        let root = self.inner.shared.locked().translator.root();
        self.inner.commit(Draft::set(root, attr::EFFORT, effort))?;
        Ok(())
    }

    async fn compact(&self, instructions: Option<String>) -> Result<()> {
        self.inner.call(|id| wire::compact(id, instructions.as_deref())).await?;
        Ok(())
    }

    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn state(&self) -> SessionState {
        self.state_now()
    }

    async fn wait_idle(&self) -> Result<()> {
        let mut rx = self.inner.state.subscribe();
        rx.wait_for(|s| matches!(s, SessionState::Idle | SessionState::Closed)).await.map_err(|_| AdapterError::Closed)?;
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        // `quit` lets omp2 flush its journal; closing stdin ends it either way.
        let _ = self.inner.write(&wire::bare("loa-quit", "quit")).await;
        self.inner.stdin.lock().await.take();
        self.child.lock().await.wait().await?;
        let reader = self.reader.locked().take();
        if let Some(reader) = reader {
            // The reader ends at EOF, which the exit above guarantees.
            let _ = reader.await;
        }
        self.inner.state.send_replace(SessionState::Closed);
        Ok(())
    }
}

fn translate_error(error: TranslateError) -> AdapterError {
    match error {
        TranslateError::Commit(error) => AdapterError::Commit(error),
        other => AdapterError::Protocol(other.to_string()),
    }
}
