//! Line-delimited JSON RPC over a child's stdio. A [`Framing`] strategy maps
//! the harness's envelope (omp frames, JSON-RPC 2.0, claude control) onto
//! [`Incoming`]; [`StdioRpc`] correlates responses by id, writes through one
//! writer task, serves inbound requests through the [`Inbound`] port and
//! reports the process exit.

use super::proc;
use async_trait::async_trait;
use loa_util::LockExt;
use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use std::collections::HashMap;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The executable couldn't be started because it doesn't exist.
    #[error("the executable `{0}` wasn't found")]
    NotFound(String),
    #[error("harness i/o failed: {0}")]
    Io(#[from] std::io::Error),
    /// The process exited, or closed its stdout, while the client still
    /// needed it. `stderr` holds the tail of what it wrote to stderr.
    #[error("the harness exited ({}){}", status.map_or_else(|| "unknown status".to_owned(), |s| s.to_string()), if stderr.is_empty() { String::new() } else { format!(": {stderr}") })]
    Exited { status: Option<ExitStatus>, stderr: String },
    /// The harness speaks no protocol version this client supports.
    #[error("the harness offers RPC protocol versions {offered:?}, this client supports {min}..={max}")]
    Version { offered: Vec<u32>, min: u32, max: u32 },
    #[error("RPC protocol error: {0}")]
    Protocol(String),
    /// A request failed on the harness side.
    #[error("the harness rejected `{command}`: {message}")]
    Command { command: String, message: String, code: Option<String> },
    #[error("the harness didn't become ready within {0:?}")]
    StartTimeout(Duration),
    #[error("the RPC client is closed")]
    Closed,
    #[error("malformed JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl RpcError {
    /// The machine readable code of a failed request (`session_busy`, …).
    pub fn code(&self) -> Option<&str> {
        match self {
            RpcError::Command { code, .. } => code.as_deref(),
            _ => None,
        }
    }
}

pub type Result<T, E = RpcError> = std::result::Result<T, E>;

/// One decoded line, as the framing classifies it.
#[derive(Debug)]
pub enum Incoming {
    /// The answer to a request this side sent; `id` as [`id_key`] renders it.
    Response { id: String, result: Result<Json> },
    /// A request the harness sends; answered with [`Framing::response`].
    Request { id: Json, method: String, params: Json },
    Notification { method: String, params: Json },
    /// The harness withdrew its request `id`.
    Cancel { id: Json },
}

/// The envelope of one harness protocol.
pub trait Framing: Send + Sync + 'static {
    fn request(&self, id: &Json, method: &str, params: Json) -> Json;
    fn notification(&self, method: &str, params: Json) -> Json;
    /// The reply to an inbound [`Incoming::Request`] for `method`.
    fn response(&self, id: Json, method: &str, result: Result<Json>) -> Json;
    /// `None` when the line was consumed (a partial chunk) or is not a frame.
    fn classify(&self, msg: Json) -> Option<Incoming>;
}

/// What the harness sends that isn't a response.
#[async_trait]
pub trait Inbound: Send + Sync + 'static {
    async fn on_request(&self, method: &str, params: Json) -> Result<Json>;
    /// Called in order; the read loop waits, so a slow consumer backpressures the harness.
    async fn on_notification(&self, method: &str, params: Json);
    async fn on_closed(&self, status: Option<ExitStatus>, stderr: &str);
    /// True when requests go to [`Inbound::on_deferred`] in read order, to be
    /// answered later with [`StdioRpc::respond`], instead of being served on
    /// their own task.
    fn defers_requests(&self) -> bool {
        false
    }
    async fn on_deferred(&self, _id: Json, _method: &str, _params: Json) {}
    /// Sees every response, in read order, before the request waiting for it does.
    async fn on_response(&self, _id: &str, _result: &Result<Json>) {}
}

/// The id as a pending-map key: a string id as is, anything else as JSON text.
pub fn id_key(id: &Json) -> String {
    id.as_str().map_or_else(|| id.to_string(), str::to_owned)
}

/// Decodes a result as `T`; a missing or `null` result decodes like `{}`.
pub fn decode<T: DeserializeOwned>(data: Json) -> Result<T> {
    let data = if data.is_null() { Json::Object(serde_json::Map::new()) } else { data };
    Ok(serde_json::from_value(data)?)
}

#[derive(Clone, Debug)]
struct Exit {
    status: Option<ExitStatus>,
    stderr: String,
}

impl Exit {
    fn error(&self) -> RpcError {
        RpcError::Exited { status: self.status, stderr: self.stderr.clone() }
    }
}

#[derive(Default)]
struct State {
    pending: HashMap<String, oneshot::Sender<Result<Json>>>,
    tasks: HashMap<String, AbortHandle>,
    exited: Option<Exit>,
}

/// What the handle, the read loop and inbound tasks share. It holds no
/// sender clone, so dropping the handle closes the child's stdin.
struct Core<F> {
    framing: F,
    out: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    state: Mutex<State>,
}

/// Per-frame traces: `RUST_LOG=loa_harness::frames=trace`.
const FRAMES: &str = "loa_harness::frames";

impl<F: Framing> Core<F> {
    fn send(&self, frame: &Json) -> Result<()> {
        tracing::trace!(target: FRAMES, %frame, "to harness");
        let mut line = serde_json::to_vec(frame)?;
        line.push(b'\n');
        match self.out.locked().as_ref() {
            Some(out) if out.send(line).is_ok() => Ok(()),
            _ => Err(self.gone()),
        }
    }

    fn gone(&self) -> RpcError {
        self.state.locked().exited.as_ref().map_or(RpcError::Closed, Exit::error)
    }

    async fn dispatch(self: &Arc<Self>, line: &[u8], inbound: &Arc<dyn Inbound>) {
        let line = line.trim_ascii();
        if line.is_empty() {
            return;
        }
        tracing::trace!(target: FRAMES, frame = %String::from_utf8_lossy(line), "from harness");
        let msg = match serde_json::from_slice(line) {
            Ok(msg) => msg,
            Err(err) => return tracing::warn!(%err, "the harness wrote a line that isn't JSON"),
        };
        match self.framing.classify(msg) {
            None => {}
            Some(Incoming::Response { id, result }) => {
                inbound.on_response(&id, &result).await;
                match self.state.locked().pending.remove(&id) {
                    Some(tx) => drop(tx.send(result)),
                    None => tracing::debug!(id, "a response nobody waits for"),
                }
            }
            Some(Incoming::Notification { method, params }) => inbound.on_notification(&method, params).await,
            Some(Incoming::Request { id, method, params }) if inbound.defers_requests() => {
                inbound.on_deferred(id, &method, params).await
            }
            Some(Incoming::Request { id, method, params }) => self.serve(id, method, params, inbound.clone()),
            Some(Incoming::Cancel { id }) => {
                if let Some(task) = self.state.locked().tasks.remove(&id_key(&id)) {
                    task.abort();
                }
            }
        }
    }

    /// Answers one inbound request on its own task, abortable by [`Incoming::Cancel`].
    fn serve(self: &Arc<Self>, id: Json, method: String, params: Json, inbound: Arc<dyn Inbound>) {
        let core = self.clone();
        let key = id_key(&id);
        // Held across the spawn so the task's own removal can't run before the insert.
        let mut state = self.state.locked();
        let task = tokio::spawn({
            let key = key.clone();
            async move {
                let result = inbound.on_request(&method, params).await;
                core.state.locked().tasks.remove(&key);
                if let Err(err) = core.send(&core.framing.response(id, &method, result)) {
                    tracing::warn!(%err, method, "couldn't answer the harness");
                }
            }
        });
        state.tasks.insert(key, task.abort_handle());
    }

    /// Records the exit and fails everything still waiting.
    fn exited(&self, exit: &Exit) {
        let mut state = self.state.locked();
        state.exited = Some(exit.clone());
        for (_, tx) in state.pending.drain() {
            let _ = tx.send(Err(exit.error()));
        }
        state.tasks.drain().for_each(|(_, task)| task.abort());
        self.out.locked().take();
    }
}

/// A running harness process spoken to through framing `F`. Dropping it
/// closes stdin and kills the process.
pub struct StdioRpc<F: Framing> {
    core: Arc<Core<F>>,
    next: AtomicU64,
    exit: watch::Receiver<Option<Exit>>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
}

impl<F: Framing> StdioRpc<F> {
    /// Starts `cmd` with piped stdio. Everything the harness sends that isn't
    /// a response goes to `inbound`.
    pub fn spawn(mut cmd: Command, framing: F, inbound: Arc<dyn Inbound>) -> Result<Self> {
        let proc::Piped { child, mut stdin, stdout, stderr } = proc::spawn_piped(&mut cmd)?;
        let (out, mut lines) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(line) = lines.recv().await {
                if stdin.write_all(&line).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
        let core = Arc::new(Core { framing, out: Mutex::new(Some(out)), state: Mutex::default() });
        let (exit_tx, exit) = watch::channel(None);
        let (kill, kill_rx) = oneshot::channel();
        let reader = core.clone();
        tokio::spawn(async move {
            let mut stdout = BufReader::new(stdout);
            let mut line = Vec::with_capacity(4096);
            let mut kill_rx = kill_rx;
            let mut killed = false;
            loop {
                line.clear();
                tokio::select! {
                    read = stdout.read_until(b'\n', &mut line) => match read {
                        Ok(0) | Err(_) => break,
                        Ok(_) => reader.dispatch(&line, &inbound).await,
                    },
                    _ = &mut kill_rx => { killed = true; break }
                }
            }
            let status = proc::reap(child, (!killed).then_some(kill_rx)).await;
            // stderr closes with the process; a grandchild holding it open
            // must not keep the exit report back forever.
            let _ = tokio::time::timeout(Duration::from_secs(2), stderr.closed()).await;
            let exit = Exit { status, stderr: stderr.joined().trim().to_owned() };
            reader.exited(&exit);
            exit_tx.send_replace(Some(exit.clone()));
            inbound.on_closed(exit.status, &exit.stderr).await;
        });
        Ok(Self { core, next: AtomicU64::new(1), exit, kill: Mutex::new(Some(kill)) })
    }

    /// A fresh request id number; ids are never reused.
    pub fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Sends a request under `id` and waits for its result.
    pub async fn request_as(&self, id: Json, method: &str, params: Json) -> Result<Json> {
        let key = id_key(&id);
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.core.state.locked();
            if let Some(exit) = &state.exited {
                return Err(exit.error());
            }
            state.pending.insert(key.clone(), tx);
        }
        if let Err(err) = self.core.send(&self.core.framing.request(&id, method, params)) {
            self.core.state.locked().pending.remove(&key);
            return Err(err);
        }
        rx.await.map_err(|_| self.core.gone())?
    }

    /// Sends a request under a fresh numeric id and decodes its result.
    pub async fn request<T: DeserializeOwned>(&self, method: &str, params: Json) -> Result<T> {
        decode(self.request_as(Json::from(self.next_id()), method, params).await?)
    }

    pub fn notify(&self, method: &str, params: Json) -> Result<()> {
        self.core.send(&self.core.framing.notification(method, params))
    }

    /// Answers an inbound request the [`Inbound`] port deferred.
    pub fn respond(&self, id: Json, method: &str, result: Result<Json>) -> Result<()> {
        self.core.send(&self.core.framing.response(id, method, result))
    }

    /// Writes a frame as is.
    pub fn send(&self, frame: &Json) -> Result<()> {
        self.core.send(frame)
    }

    /// A handle that writes frames without keeping the process alive.
    pub fn writer(&self) -> Writer<F> {
        Writer(Arc::downgrade(&self.core))
    }

    pub fn has_exited(&self) -> bool {
        self.exit.borrow().is_some()
    }

    /// Resolves once the process is gone, with the error requests now fail with.
    pub async fn closed(&self) -> RpcError {
        let mut exit = self.exit.clone();
        exit.wait_for(Option::is_some).await.ok().and_then(|exit| exit.as_ref().map(Exit::error)).unwrap_or(RpcError::Closed)
    }

    /// Kills the process without waiting.
    pub fn kill(&self) {
        if let Some(kill) = self.kill.locked().take() {
            let _ = kill.send(());
        }
    }

    /// Closes stdin so the harness finishes queued work and exits, waits up
    /// to `grace`, then kills it. Returns the exit status when known.
    pub async fn shutdown(&self, grace: Duration) -> Option<ExitStatus> {
        self.core.out.locked().take();
        let mut exit = self.exit.clone();
        if tokio::time::timeout(grace, exit.wait_for(Option::is_some)).await.is_err() {
            self.kill();
        }
        exit.wait_for(Option::is_some).await.ok().and_then(|exit| exit.as_ref().and_then(|e| e.status))
    }
}

impl<F: Framing> Drop for StdioRpc<F> {
    fn drop(&mut self) {
        self.core.out.locked().take();
    }
}

/// Writes frames to a [`StdioRpc`] while it lives; see [`StdioRpc::writer`].
pub struct Writer<F>(std::sync::Weak<Core<F>>);

impl<F> Clone for Writer<F> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<F: Framing> Writer<F> {
    pub fn send(&self, frame: &Json) -> Result<()> {
        self.0.upgrade().ok_or(RpcError::Closed)?.send(frame)
    }
}
