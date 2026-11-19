//! The host side of the stub protocol.

use crate::proto::{
    self, Command, DirEntry, ExecRequest, ExitStatus, MAGIC, Metadata, ProtoError, Request, Response,
};
use crate::provider::Channel;
use crate::{Result, SandboxError};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

type Routes = HashMap<u32, mpsc::UnboundedSender<Response>>;

/// A connection to one stub. Requests may run concurrently; answers are
/// routed back by request id.
pub struct StubClient {
    writer: tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    routes: Arc<Mutex<Option<Routes>>>,
    next_id: AtomicU32,
    reader: JoinHandle<()>,
}

/// Everything a command printed and how it ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: ExitStatus,
}

/// Output of a running command, as it arrives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(ExitStatus),
}

/// A command started with [`StubClient::spawn`].
pub struct ExecHandle {
    pub id: u32,
    rx: mpsc::UnboundedReceiver<Response>,
    exited: bool,
}

impl ExecHandle {
    /// The next chunk of output, then the exit status, then `None`.
    pub async fn next(&mut self) -> Result<Option<ExecEvent>> {
        if self.exited {
            return Ok(None);
        }
        match self.rx.recv().await {
            Some(Response::Stdout { data, .. }) => Ok(Some(ExecEvent::Stdout(data))),
            Some(Response::Stderr { data, .. }) => Ok(Some(ExecEvent::Stderr(data))),
            Some(Response::Exit { status, .. }) => {
                self.exited = true;
                Ok(Some(ExecEvent::Exit(status)))
            }
            Some(Response::Error { message, .. }) => Err(SandboxError::Stub(message)),
            Some(other) => Err(unexpected(&other)),
            None => Err(SandboxError::Disconnected),
        }
    }

    /// Collects the rest of the output.
    pub async fn wait(mut self) -> Result<ExecOutput> {
        let mut out = ExecOutput::default();
        loop {
            match self.rx.recv().await {
                Some(Response::Stdout { data, .. }) => out.stdout.extend_from_slice(&data),
                Some(Response::Stderr { data, .. }) => out.stderr.extend_from_slice(&data),
                Some(Response::Exit { status, .. }) => {
                    out.status = status;
                    return Ok(out);
                }
                Some(Response::Error { message, .. }) => return Err(SandboxError::Stub(message)),
                Some(other) => return Err(unexpected(&other)),
                None => return Err(SandboxError::Disconnected),
            }
        }
    }
}

fn unexpected(response: &Response) -> SandboxError {
    SandboxError::Protocol(ProtoError(format!("unexpected answer {response:?}")))
}

/// Answers that end a request.
fn is_final(response: &Response) -> bool {
    !matches!(response, Response::Stdout { .. } | Response::Stderr { .. } | Response::Ready { .. })
}

async fn read_frame(r: &mut (impl AsyncRead + Unpin), buf: &mut Vec<u8>) -> std::io::Result<bool> {
    let mut header = [0; 4];
    match r.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
        Err(e) => return Err(e),
    }
    let len = proto::frame_len(header)?;
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf).await?;
    Ok(true)
}

impl StubClient {
    /// Waits for the stub's hello on `channel`, skipping anything printed
    /// before it, and starts routing its answers.
    pub async fn connect(channel: Channel) -> Result<Self> {
        let mut reader = BufReader::new(channel.reader);
        let mut matched = 0;
        while matched < MAGIC.len() {
            let byte = match reader.read_u8().await {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(SandboxError::Disconnected),
                Err(e) => return Err(e.into()),
            };
            matched = proto::advance_magic(matched, byte);
        }
        let mut buf = Vec::new();
        if !read_frame(&mut reader, &mut buf).await? {
            return Err(SandboxError::Disconnected);
        }
        match Response::decode(&buf)? {
            Response::Ready { version } if version == proto::VERSION => {}
            Response::Ready { version } => {
                return Err(SandboxError::Stub(format!(
                    "stub speaks protocol {version}, the host speaks {}",
                    proto::VERSION
                )));
            }
            other => return Err(unexpected(&other)),
        }
        let routes = Arc::new(Mutex::new(Some(Routes::new())));
        let reader = tokio::spawn(route(reader, Arc::clone(&routes)));
        Ok(Self { writer: tokio::sync::Mutex::new(channel.writer), routes, next_id: AtomicU32::new(1), reader })
    }

    fn id(&self) -> u32 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn send(&self, request: &Request) -> Result<()> {
        let mut out = Vec::new();
        request.encode(&mut out);
        let mut writer = self.writer.lock().await;
        writer.write_all(&out).await.map_err(|_| SandboxError::Disconnected)?;
        writer.flush().await.map_err(|_| SandboxError::Disconnected)
    }

    /// Registers the route for `id`, then sends `request`.
    async fn call(&self, id: u32, request: &Request) -> Result<mpsc::UnboundedReceiver<Response>> {
        let (tx, rx) = mpsc::unbounded_channel();
        {
            let mut routes = self.routes.lock().unwrap_or_else(PoisonError::into_inner);
            routes.as_mut().ok_or(SandboxError::Disconnected)?.insert(id, tx);
        }
        self.send(request).await?;
        Ok(rx)
    }

    async fn reply(&self, id: u32, request: &Request) -> Result<Response> {
        let mut rx = self.call(id, request).await?;
        match rx.recv().await.ok_or(SandboxError::Disconnected)? {
            Response::Error { message, .. } => Err(SandboxError::Stub(message)),
            other => Ok(other),
        }
    }

    /// Starts a command and returns its output stream.
    pub async fn spawn(&self, exec: ExecRequest) -> Result<ExecHandle> {
        let id = self.id();
        let rx = self.call(id, &Request::Exec { id, exec }).await?;
        Ok(ExecHandle { id, rx, exited: false })
    }

    /// Runs a command to the end.
    pub async fn exec(&self, exec: ExecRequest) -> Result<ExecOutput> {
        self.spawn(exec).await?.wait().await
    }

    /// Runs a shell script in `cwd` (relative to the stub root).
    pub async fn sh(&self, script: &str, timeout_ms: Option<u64>) -> Result<ExecOutput> {
        self.exec(ExecRequest {
            command: Command::Shell(script.to_owned()),
            cwd: None,
            env: Vec::new(),
            stdin: None,
            timeout_ms,
        })
        .await
    }

    /// Kills a command started with [`StubClient::spawn`].
    pub async fn kill(&self, exec_id: u32) -> Result<()> {
        self.send(&Request::Kill { id: exec_id }).await
    }

    pub async fn read_file(&self, path: &str, limit: Option<u64>) -> Result<Vec<u8>> {
        let id = self.id();
        match self.reply(id, &Request::ReadFile { id, path: path.to_owned(), limit }).await? {
            Response::Data { data, .. } => Ok(data),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn write_file(&self, path: &str, data: Vec<u8>, mode: Option<u32>) -> Result<()> {
        let id = self.id();
        match self.reply(id, &Request::WriteFile { id, path: path.to_owned(), data, mode }).await? {
            Response::Done { .. } => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>> {
        let id = self.id();
        match self.reply(id, &Request::ListDir { id, path: path.to_owned() }).await? {
            Response::Entries { entries, .. } => Ok(entries),
            other => Err(unexpected(&other)),
        }
    }

    /// `None` when `path` doesn't exist.
    pub async fn stat(&self, path: &str) -> Result<Option<Metadata>> {
        let id = self.id();
        match self.reply(id, &Request::Stat { id, path: path.to_owned() }).await? {
            Response::Stat { meta, .. } => Ok(meta),
            other => Err(unexpected(&other)),
        }
    }

    /// Asks the stub to kill its commands and exit.
    pub async fn shutdown(&self) -> Result<()> {
        self.send(&Request::Shutdown).await
    }
}

impl Drop for StubClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Reads answers until the stream ends and hands each to its request.
async fn route(mut reader: impl AsyncRead + Unpin, routes: Arc<Mutex<Option<Routes>>>) {
    let mut buf = Vec::new();
    while let Ok(true) = read_frame(&mut reader, &mut buf).await {
        let Ok(response) = Response::decode(&buf) else { break };
        let Some(id) = response.id() else { continue };
        let mut guard = routes.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(routes) = guard.as_mut() else { break };
        let done = is_final(&response);
        if let Some(tx) = routes.get(&id) {
            // A dropped receiver only means the caller stopped listening.
            let _ = tx.send(response);
        }
        if done {
            routes.remove(&id);
        }
    }
    // Dropping every sender wakes waiting callers with `Disconnected`.
    routes.lock().unwrap_or_else(PoisonError::into_inner).take();
}
