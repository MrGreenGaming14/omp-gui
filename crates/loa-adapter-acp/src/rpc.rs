//! JSON-RPC 2.0 over newline delimited JSON, the ACP stdio transport.
//!
//! [`Rpc`] is the outgoing half: requests, notifications and responses to the
//! agent's own requests. [`read_loop`] is the incoming half: it routes
//! responses to waiting requests and hands the agent's requests and
//! notifications to an [`Inbound`] handler, in the order they arrived.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// Why a request to the agent failed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum RpcError {
    /// The agent answered with a JSON-RPC error.
    #[error("{message} (code {code})")]
    Remote { code: i64, message: String },
    /// The connection closed before the response arrived.
    #[error("the agent connection is closed")]
    Closed,
}

impl RpcError {
    pub fn remote(code: i64, message: impl Into<String>) -> Self {
        RpcError::Remote { code, message: message.into() }
    }
}

impl From<RpcError> for loa_adapter::AdapterError {
    fn from(e: RpcError) -> Self {
        match e {
            RpcError::Remote { .. } => loa_adapter::AdapterError::Protocol(e.to_string()),
            RpcError::Closed => loa_adapter::AdapterError::Closed,
        }
    }
}

pub type RpcResult = Result<Value, RpcError>;

/// Receives what the agent sends besides responses.
pub trait Inbound: Send + Sync + 'static {
    /// A request from the agent. The handler answers it through
    /// [`Rpc::respond`], now or later.
    fn on_request(self: &Arc<Self>, id: Value, method: &str, params: Value);
    fn on_notification(&self, method: &str, params: Value);
    /// Called for every response before the waiting request sees it, so the
    /// handler orders its own bookkeeping with the notifications around it.
    fn on_response(&self, id: u64, result: &RpcResult);
    /// The agent closed its output.
    fn on_closed(&self);
}

/// Outgoing half of the connection.
pub struct Rpc {
    /// `None` once closed: dropping the sender ends the writer task, which
    /// closes the agent's input.
    out: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<RpcResult>>>,
    next_id: AtomicU64,
}

impl Rpc {
    /// Starts the writer task on `writer`.
    pub fn new<W: AsyncWrite + Send + Unpin + 'static>(mut writer: W) -> Arc<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if let Err(e) = writer.write_all(&line).await {
                    tracing::debug!("acp: write to agent failed: {e}");
                    break;
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
            let _ = writer.shutdown().await;
        });
        Arc::new(Self { out: Mutex::new(Some(tx)), pending: Mutex::new(HashMap::new()), next_id: AtomicU64::new(1) })
    }

    /// Reserves the id of the next request, for callers that must know it
    /// before the request goes out.
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Sends request `id` and returns the receiver of its response.
    pub fn start_request(&self, id: u64, method: &str, params: Value) -> Result<oneshot::Receiver<RpcResult>, RpcError> {
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.send(&line) {
            lock(&self.pending).remove(&id);
            return Err(e);
        }
        Ok(rx)
    }

    pub async fn request(&self, method: &str, params: Value) -> RpcResult {
        let rx = self.start_request(self.next_id(), method, params)?;
        rx.await.unwrap_or(Err(RpcError::Closed))
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    /// Answers request `id` from the agent.
    pub fn respond(&self, id: Value, result: RpcResult) {
        let line = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(RpcError::Remote { code, message }) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
            Err(RpcError::Closed) => return,
        };
        if self.send(&line).is_err() {
            tracing::debug!("acp: response dropped, connection closed");
        }
    }

    /// Closes the agent's input and fails every waiting request.
    pub fn close(&self) {
        lock(&self.out).take();
        lock(&self.pending).clear();
    }

    fn resolve(&self, id: u64, result: RpcResult) {
        if let Some(tx) = lock(&self.pending).remove(&id) {
            let _ = tx.send(result);
        }
    }

    fn send(&self, message: &Value) -> Result<(), RpcError> {
        // Serializing a `Value` can't fail: its map keys are strings.
        let mut line = serde_json::to_vec(message).map_err(|_| RpcError::Closed)?;
        line.push(b'\n');
        let out = lock(&self.out);
        out.as_ref().ok_or(RpcError::Closed)?.send(line).map_err(|_| RpcError::Closed)
    }
}

/// Reads messages from the agent until its output closes.
pub async fn read_loop<R, H>(reader: R, rpc: Arc<Rpc>, handler: Arc<H>)
where
    R: AsyncRead + Unpin,
    H: Inbound,
{
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                tracing::debug!("acp: read from agent failed: {e}");
                break;
            }
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(text) {
            Ok(message) => message,
            Err(e) => {
                tracing::warn!("acp: skipping a line that isn't JSON: {e}");
                continue;
            }
        };
        dispatch(message, &rpc, &handler);
    }
    rpc.close();
    handler.on_closed();
}

fn dispatch<H: Inbound>(mut message: Value, rpc: &Rpc, handler: &Arc<H>) {
    let Some(object) = message.as_object_mut() else {
        tracing::warn!("acp: skipping a message that isn't an object");
        return;
    };
    let params = object.remove("params").unwrap_or(Value::Null);
    let id = object.remove("id");
    if let Some(Value::String(method)) = object.remove("method") {
        match id {
            Some(id) => handler.on_request(id, &method, params),
            None => handler.on_notification(&method, params),
        }
        return;
    }
    let Some(id) = id.as_ref().and_then(Value::as_u64) else {
        tracing::warn!("acp: skipping a response with an id this client never sent");
        return;
    };
    let result = match object.remove("error") {
        Some(error) => Err(RpcError::Remote {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error.get("message").and_then(Value::as_str).unwrap_or("unknown error").to_owned(),
        }),
        None => Ok(object.remove("result").unwrap_or(Value::Null)),
    };
    handler.on_response(id, &result);
    rpc.resolve(id, result);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded maps stay consistent across a panic: each update is one call.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
