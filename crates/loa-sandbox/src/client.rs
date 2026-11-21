//! The host side of the stub protocol.

use crate::proto::{self, DirEntry, ExecRequest, ExitStatus, Request, Response};
use crate::provider::Channel;
use crate::{Result, SandboxError};
use loa_util::LockExt;
use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

type Routes = Arc<Mutex<Option<HashMap<u32, mpsc::UnboundedSender<Response>>>>>;

/// A connection to one stub. Requests may run concurrently; answers are
/// routed back by request id.
pub struct StubClient {
    writer: tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    routes: Routes,
    next_id: AtomicU32,
}

/// Everything a command printed and how it ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: ExitStatus,
}

/// The error for an answer that isn't the one asked for.
fn refused(response: Response) -> SandboxError {
    match response {
        Response::Error { message, .. } => SandboxError::Stub(message),
        other => SandboxError::Stub(format!("unexpected answer {other:?}")),
    }
}

impl StubClient {
    /// Waits for the stub's hello on `channel`, skipping anything printed
    /// before it, and starts routing its answers.
    pub async fn connect(channel: Channel) -> Result<Self> {
        let routes = Routes::new(Mutex::new(Some(HashMap::new())));
        let (ready, hello) = oneshot::channel();
        let reader = tokio_util::io::SyncIoBridge::new(channel.reader);
        let task_routes = Arc::clone(&routes);
        tokio::task::spawn_blocking(move || route(BufReader::new(reader), &task_routes, ready));
        hello.await.map_err(|_| SandboxError::Disconnected)??;
        Ok(Self { writer: tokio::sync::Mutex::new(channel.writer), routes, next_id: AtomicU32::new(1) })
    }

    async fn send(&self, request: &Request) -> Result<()> {
        let mut writer = self.writer.lock().await;
        writer.write_all(&proto::frame(request)).await.map_err(|_| SandboxError::Disconnected)?;
        writer.flush().await.map_err(|_| SandboxError::Disconnected)
    }

    /// Registers a route for a new id, then sends the request made with it.
    async fn call(&self, make: impl FnOnce(u32) -> Request) -> Result<mpsc::UnboundedReceiver<Response>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        self.routes.locked().as_mut().ok_or(SandboxError::Disconnected)?.insert(id, tx);
        self.send(&make(id)).await?;
        Ok(rx)
    }

    /// Sends a request and picks the value out of its one answer.
    async fn request<T>(&self, make: impl FnOnce(u32) -> Request, pick: fn(Response) -> Result<T, Response>) -> Result<T> {
        let mut rx = self.call(make).await?;
        pick(rx.recv().await.ok_or(SandboxError::Disconnected)?).map_err(refused)
    }

    /// Runs a command to the end, collecting its output.
    pub async fn exec(&self, exec: ExecRequest) -> Result<ExecOutput> {
        let mut rx = self.call(|id| Request::Exec { id, exec }).await?;
        let mut out = ExecOutput::default();
        loop {
            match rx.recv().await.ok_or(SandboxError::Disconnected)? {
                Response::Stdout { data, .. } => out.stdout.extend(data),
                Response::Stderr { data, .. } => out.stderr.extend(data),
                Response::Exit { status, .. } => return Ok(ExecOutput { status, ..out }),
                other => return Err(refused(other)),
            }
        }
    }

    pub async fn read_file(&self, path: &str, limit: Option<u64>) -> Result<Vec<u8>> {
        let path = path.to_owned();
        self.request(|id| Request::ReadFile { id, path, limit }, |r| if let Response::Data { data, .. } = r { Ok(data) } else { Err(r) }).await
    }

    pub async fn write_file(&self, path: &str, data: Vec<u8>) -> Result<()> {
        let path = path.to_owned();
        self.request(|id| Request::WriteFile { id, path, data }, |r| if let Response::Done { .. } = r { Ok(()) } else { Err(r) }).await
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>> {
        let path = path.to_owned();
        self.request(|id| Request::ListDir { id, path }, |r| if let Response::Entries { entries, .. } = r { Ok(entries) } else { Err(r) }).await
    }

    /// Asks the stub to kill its commands and exit.
    pub async fn shutdown(&self) -> Result<()> {
        self.send(&Request::Shutdown).await
    }
}

/// Reads the hello, reports it on `ready`, then hands each answer to its
/// request until the stream ends. Runs on a blocking thread over the shared
/// std frame reader.
fn route(mut reader: impl Read, routes: &Routes, ready: oneshot::Sender<Result<()>>) {
    let mut buf = Vec::new();
    let hello = match proto::skip_to_magic(&mut reader).and_then(|_| proto::read_frame(&mut reader, &mut buf)) {
        Ok(false) => Err(SandboxError::Disconnected),
        Ok(true) => match proto::decode(&buf) {
            Ok(Response::Ready { version }) if version == proto::VERSION => Ok(()),
            Ok(Response::Ready { version }) => {
                Err(SandboxError::Stub(format!("stub speaks protocol {version}, the host speaks {}", proto::VERSION)))
            }
            Ok(other) => Err(refused(other)),
            Err(e) => Err(e.into()),
        },
        Err(e) => Err(e.into()),
    };
    let connected = hello.is_ok();
    if ready.send(hello).is_ok() && connected {
        while let Ok(true) = proto::read_frame(&mut reader, &mut buf) {
            let Ok(response) = proto::decode::<Response>(&buf) else { break };
            let Some(id) = response.id() else { continue };
            let mut guard = routes.locked();
            let Some(routes) = guard.as_mut() else { break };
            let done = !matches!(response, Response::Stdout { .. } | Response::Stderr { .. });
            // A dropped receiver only means the caller stopped listening.
            let _ = routes.get(&id).map(|tx| tx.send(response));
            if done {
                routes.remove(&id);
            }
        }
    }
    // Dropping every sender wakes waiting callers with `Disconnected`.
    routes.locked().take();
}
