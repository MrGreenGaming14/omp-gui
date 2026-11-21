//! The process side: spawning `omp --mode rpc`, correlating responses,
//! serving host callbacks and delivering everything else as [`Incoming`].

use loa_util::LockExt;
use crate::chunk::Reassembler;
use crate::command::{
    CancellationResult, Command, CompactionResult, HostToolDefinition, HostUriScheme, ImageContent, LoginProvider,
    MessageUpdates, ModelInfo, PromptAck, Ready, RestoredQueue, SessionState, StreamingBehavior, SubagentLevel,
};
use crate::error::{Result, RpcError};
use crate::event::{Event, Frame, HostToolCall, HostUriRequest, Response, UiRequest};
use crate::inbound::{HostToolOutput, HostUriOutput, UiResponse};
use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;

/// Lowest protocol version this client speaks.
pub const MIN_PROTOCOL: u32 = 1;
/// Highest protocol version this client speaks.
pub const MAX_PROTOCOL: u32 = 2;

/// Bytes of omp's stderr kept for error reports.
const STDERR_TAIL: usize = 8 * 1024;
/// Reassembly limit used until omp advertises its own.
const DEFAULT_REASSEMBLY_LIMIT: u64 = 64 * 1024 * 1024;

/// `--mode` of the child process.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// `rpc`: extension UI, approvals and login prompts reach the host.
    #[default]
    Rpc,
    /// `rpc-ui`: also enables the tool UI context, so the `ask` tool works.
    RpcUi,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Rpc => "rpc",
            Mode::RpcUi => "rpc-ui",
        }
    }
}

/// Serves calls omp makes back into the host.
#[async_trait]
pub trait HostHandler: Send + Sync {
    /// Runs a host tool registered with `set_host_tools`.
    async fn tool_call(&self, call: HostToolCall) -> HostToolOutput;

    /// Reads or writes a URL of a scheme registered with `set_host_uri_schemes`.
    async fn uri_request(&self, request: HostUriRequest) -> HostUriOutput {
        HostUriOutput::Error(format!("no host handler for {}", request.url))
    }
}

/// How to start omp.
#[derive(Clone)]
pub struct SpawnOptions {
    /// The executable. A bare name is looked up on `PATH` by the OS.
    pub executable: PathBuf,
    pub mode: Mode,
    /// Pass `--no-ui`: extensions run headless.
    pub no_ui: bool,
    /// Further CLI arguments, after `--mode`.
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Extra environment variables for the child.
    pub env: Vec<(OsString, OsString)>,
    /// How long to wait for the `ready` frame.
    pub ready_timeout: Duration,
    /// Negotiate protocol v2 when omp offers it.
    pub negotiate: bool,
    /// Frames buffered for the [`Events`] reader before omp is backpressured.
    pub event_capacity: usize,
    pub host: Option<Arc<dyn HostHandler>>,
}

impl SpawnOptions {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            mode: Mode::Rpc,
            no_ui: false,
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            ready_timeout: Duration::from_secs(60),
            negotiate: true,
            event_capacity: 1024,
            host: None,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self::new("omp")
    }
}

impl std::fmt::Debug for SpawnOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnOptions")
            .field("executable", &self.executable)
            .field("mode", &self.mode)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

/// Everything omp sends that isn't the response to a command of this client.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Event(Event),
    /// A UI request. Dialogs need [`RpcClient::respond_ui`].
    Ui(UiRequest),
    /// A response no command waits for, such as a `parse` failure.
    Response(Response),
    /// omp is gone. Always the last item.
    Exited { status: Option<ExitStatus>, stderr: String },
}

/// The stream of [`Incoming`] frames, in the order omp wrote them.
#[derive(Debug)]
pub struct Events {
    rx: mpsc::Receiver<Incoming>,
}

impl Events {
    /// The next frame; `None` after [`Incoming::Exited`] was delivered.
    pub async fn recv(&mut self) -> Option<Incoming> {
        self.rx.recv().await
    }
}

#[derive(Clone, Debug)]
struct ExitInfo {
    status: Option<ExitStatus>,
    stderr: String,
}

#[derive(Default)]
struct Pending {
    waiting: HashMap<String, oneshot::Sender<Result<Response>>>,
    exited: Option<ExitInfo>,
}

/// State shared by the client handles and the reader task.
struct Shared {
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    pending: Mutex<Pending>,
    host_tasks: Mutex<HashMap<String, AbortHandle>>,
    stderr: Mutex<VecDeque<u8>>,
}


/// Decodes response `data` as `T`. A success response may omit `data`
/// (`data?` in omp's RPC docs; omp 19 answers `prompt` that way), which
/// decodes like an empty object so types whose fields all have defaults
/// still parse.
fn decode<T: DeserializeOwned>(data: Option<Value>) -> Result<T> {
    let data = match data {
        None | Some(Value::Null) => Value::Object(serde_json::Map::new()),
        Some(data) => data,
    };
    Ok(serde_json::from_value(data)?)
}

/// Target of the per-frame trace events (`RUST_LOG=loa_omp_rpc::frames=trace`).
const FRAMES: &str = "loa_omp_rpc::frames";

impl Shared {
    async fn write_line(&self, mut line: Vec<u8>) -> Result<()> {
        tracing::trace!(target: FRAMES, frame = %String::from_utf8_lossy(&line), "to omp");
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        let Some(pipe) = stdin.as_mut() else {
            return Err(self.gone());
        };
        let written = async {
            pipe.write_all(&line).await?;
            pipe.flush().await
        }
        .await;
        written.map_err(|err| if err.kind() == std::io::ErrorKind::BrokenPipe { self.gone() } else { err.into() })
    }

    async fn write_json(&self, value: &Value) -> Result<()> {
        self.write_line(serde_json::to_vec(value)?).await
    }

    fn gone(&self) -> RpcError {
        match &self.pending.locked().exited {
            Some(exit) => RpcError::Exited { status: exit.status, stderr: exit.stderr.clone() },
            None => RpcError::Closed,
        }
    }

    fn stderr_tail(&self) -> String {
        let tail = self.stderr.locked();
        let (a, b) = tail.as_slices();
        let mut bytes = Vec::with_capacity(a.len() + b.len());
        bytes.extend_from_slice(a);
        bytes.extend_from_slice(b);
        String::from_utf8_lossy(&bytes).trim().to_owned()
    }
}

struct Inner {
    shared: Arc<Shared>,
    ready: Ready,
    protocol: AtomicU32,
    next_id: AtomicU64,
    exit: watch::Receiver<Option<ExitInfo>>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Closing stdin lets omp shut down cleanly; `kill_on_drop` on the child
        // covers an omp that ignores EOF once the reader task ends.
        if let Ok(mut stdin) = self.shared.stdin.try_lock() {
            stdin.take();
        }
    }
}

/// Answers UI requests for an [`RpcClient`]; see [`RpcClient::ui_responder`].
#[derive(Clone)]
pub struct UiResponder {
    shared: Arc<Shared>,
}

impl UiResponder {
    pub async fn respond(&self, response: &UiResponse) -> Result<()> {
        self.shared.write_json(&response.to_json()).await
    }
}

/// A running `omp --mode rpc` process. Cheap to clone; every clone talks to
/// the same process.
#[derive(Clone)]
pub struct RpcClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for RpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcClient").field("protocol", &self.protocol_version()).finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct Outgoing<'a> {
    id: &'a str,
    #[serde(flatten)]
    command: &'a Command,
}

/// Runs `<executable> --version` and returns what it printed, trimmed.
pub async fn omp_version(executable: impl Into<PathBuf>) -> Result<String> {
    let executable = executable.into();
    let output = tokio::process::Command::new(&executable)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|err| spawn_error(err, &executable))?;
    if !output.status.success() {
        return Err(RpcError::Exited {
            status: Some(output.status),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn spawn_error(err: std::io::Error, executable: &std::path::Path) -> RpcError {
    if err.kind() == std::io::ErrorKind::NotFound {
        RpcError::NotFound(executable.display().to_string())
    } else {
        RpcError::Io(err)
    }
}

fn spawn_child(opts: &SpawnOptions) -> Result<Child> {
    let mut command = tokio::process::Command::new(&opts.executable);
    command.arg("--mode").arg(opts.mode.as_str());
    if opts.no_ui {
        command.arg("--no-ui");
    }
    command
        .args(&opts.args)
        .envs(opts.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &opts.cwd {
        command.current_dir(cwd);
    }
    // A freshly written executable (a staged update, a test fake) can still be
    // open for writing in a child forked by another thread; that clears once
    // the child execs, so a busy executable is retried briefly.
    loa_util::exec::retry_busy(|| command.spawn()).map_err(|err| spawn_error(err, &opts.executable))
}

impl RpcClient {
    /// Starts omp, waits for its `ready` frame and negotiates the protocol
    /// version. Fails with [`RpcError::Version`] when omp offers no version in
    /// `MIN_PROTOCOL..=MAX_PROTOCOL`.
    pub async fn spawn(opts: SpawnOptions) -> Result<(RpcClient, Events)> {
        let mut child = spawn_child(&opts)?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(RpcError::Protocol("child pipes were not created".to_owned()));
        };
        let shared = Arc::new(Shared {
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            pending: Mutex::new(Pending::default()),
            host_tasks: Mutex::new(HashMap::new()),
            stderr: Mutex::new(VecDeque::with_capacity(STDERR_TAIL)),
        });
        let stderr_task = tokio::spawn(collect_stderr(stderr, shared.clone()));
        let (events_tx, events_rx) = mpsc::channel(opts.event_capacity.max(1));
        let (ready_tx, ready_rx) = oneshot::channel();
        let (exit_tx, exit_rx) = watch::channel(None);
        let (kill_tx, kill_rx) = oneshot::channel();
        let reader = Reader {
            shared: shared.clone(),
            events: events_tx,
            ready: Some(ready_tx),
            host: opts.host.clone(),
            chunks: Reassembler::new(DEFAULT_REASSEMBLY_LIMIT),
        };
        tokio::spawn(reader.run(stdout, child, stderr_task, exit_tx, kill_rx));

        let ready = match tokio::time::timeout(opts.ready_timeout, ready_rx).await {
            Ok(Ok(ready)) => ready,
            Ok(Err(_)) => {
                let mut exit = exit_rx.clone();
                let info = exit.wait_for(Option::is_some).await.ok().and_then(|info| info.clone());
                return Err(match info {
                    Some(info) => RpcError::Exited { status: info.status, stderr: info.stderr },
                    None => RpcError::Closed,
                });
            }
            Err(_) => {
                let _ = kill_tx.send(());
                return Err(RpcError::StartTimeout(opts.ready_timeout));
            }
        };
        let client = RpcClient {
            inner: Arc::new(Inner {
                shared,
                ready,
                protocol: AtomicU32::new(1),
                next_id: AtomicU64::new(1),
                exit: exit_rx,
                kill: Mutex::new(Some(kill_tx)),
            }),
        };
        let offered = client.inner.ready.offered_versions();
        let chosen = offered.iter().copied().filter(|v| (MIN_PROTOCOL..=MAX_PROTOCOL).contains(v)).max();
        let Some(chosen) = chosen else {
            client.kill();
            return Err(RpcError::Version { offered, min: MIN_PROTOCOL, max: MAX_PROTOCOL });
        };
        let chosen = if chosen >= 2 && !opts.negotiate { 1 } else { chosen };
        if chosen >= 2 {
            client.request_unit(Command::NegotiateProtocol { protocol_version: chosen }).await?;
        }
        client.inner.protocol.store(chosen, Ordering::Release);
        Ok((client, Events { rx: events_rx }))
    }

    /// The `ready` frame omp sent at startup.
    pub fn ready(&self) -> &Ready {
        &self.inner.ready
    }

    /// The protocol version in use after negotiation.
    pub fn protocol_version(&self) -> u32 {
        self.inner.protocol.load(Ordering::Acquire)
    }

    /// Whether omp has exited.
    pub fn has_exited(&self) -> bool {
        self.inner.exit.borrow().is_some()
    }

    /// Sends `command` and waits for its response. Returns the response `data`.
    pub async fn request_raw(&self, command: Command) -> Result<Option<Value>> {
        self.request_as(self.reserve_id(), command).await
    }

    /// A fresh command id. Reserve one when frames that will refer to the
    /// command (such as a prompt's `prompt_result`) must be expected before
    /// it is sent, then send it with [`RpcClient::request_as`].
    pub fn reserve_id(&self) -> String {
        format!("c{}", self.inner.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// [`RpcClient::request_raw`] under an id from [`RpcClient::reserve_id`].
    pub async fn request_as(&self, id: String, command: Command) -> Result<Option<Value>> {
        let shared = &self.inner.shared;
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = shared.pending.locked();
            if let Some(exit) = &pending.exited {
                return Err(RpcError::Exited { status: exit.status, stderr: exit.stderr.clone() });
            }
            pending.waiting.insert(id.clone(), tx);
        }
        let line = serde_json::to_vec(&Outgoing { id: &id, command: &command })?;
        if let Err(err) = shared.write_line(line).await {
            shared.pending.locked().waiting.remove(&id);
            return Err(err);
        }
        let response = rx.await.map_err(|_| shared.gone())??;
        if !response.success {
            return Err(RpcError::Command {
                command: response.command,
                message: response.error.unwrap_or_default(),
                code: response.code,
            });
        }
        Ok(response.data)
    }

    /// Sends `command` and decodes the response `data` as `T`; a missing
    /// `data` decodes like an empty object.
    pub async fn request<T: DeserializeOwned>(&self, command: Command) -> Result<T> {
        decode(self.request_raw(command).await?)
    }

    /// Sends `command` and ignores the response `data`.
    pub async fn request_unit(&self, command: Command) -> Result<()> {
        self.request_raw(command).await.map(drop)
    }

    /// Answers a dialog UI request.
    pub async fn respond_ui(&self, response: &UiResponse) -> Result<()> {
        self.inner.shared.write_json(&response.to_json()).await
    }

    /// Answers UI requests like [`RpcClient::respond_ui`] without keeping omp
    /// running: once every `RpcClient` clone is dropped, omp's stdin closes
    /// and the responder's writes fail.
    pub fn ui_responder(&self) -> UiResponder {
        UiResponder { shared: self.inner.shared.clone() }
    }

    pub async fn prompt(
        &self,
        message: impl Into<String>,
        images: Vec<ImageContent>,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> Result<PromptAck> {
        self.prompt_as(self.reserve_id(), message, images, streaming_behavior).await
    }

    /// [`RpcClient::prompt`] under an id from [`RpcClient::reserve_id`]; the
    /// prompt's `prompt_result` carries `id`.
    pub async fn prompt_as(
        &self,
        id: String,
        message: impl Into<String>,
        images: Vec<ImageContent>,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> Result<PromptAck> {
        decode(self.request_as(id, Command::Prompt { message: message.into(), images, streaming_behavior }).await?)
    }

    /// Queues `message` as steering for the running turn.
    pub async fn steer(&self, message: impl Into<String>) -> Result<()> {
        self.request_unit(Command::Steer { message: message.into(), images: Vec::new() }).await
    }

    pub async fn abort(&self) -> Result<()> {
        self.request_unit(Command::Abort).await
    }

    /// Withdraws queued user input and aborts, atomically. Unlike `abort`,
    /// which leaves queued follow-ups waiting (and their prompts and the
    /// running one unsettled), every withdrawn prompt then completes
    /// `aborted`.
    pub async fn abort_and_restore_queue(&self) -> Result<RestoredQueue> {
        self.request(Command::AbortAndRestoreQueue).await
    }

    pub async fn fork(&self, entry_id: Option<String>) -> Result<CancellationResult> {
        self.request(Command::Fork { entry_id }).await
    }

    pub async fn set_model(&self, provider: impl Into<String>, model_id: impl Into<String>) -> Result<ModelInfo> {
        self.request(Command::SetModel { provider: provider.into(), model_id: model_id.into() }).await
    }

    pub async fn get_available_models(&self) -> Result<Vec<ModelInfo>> {
        #[derive(serde::Deserialize)]
        struct Models {
            models: Vec<ModelInfo>,
        }
        Ok(self.request::<Models>(Command::GetAvailableModels).await?.models)
    }

    pub async fn get_login_providers(&self) -> Result<Vec<LoginProvider>> {
        #[derive(serde::Deserialize)]
        struct Providers {
            providers: Vec<LoginProvider>,
        }
        Ok(self.request::<Providers>(Command::GetLoginProviders).await?.providers)
    }

    /// Runs an OAuth login. The flow arrives as `open_url` and `input` UI requests.
    pub async fn login(&self, provider_id: impl Into<String>) -> Result<()> {
        self.request_unit(Command::Login { provider_id: provider_id.into() }).await
    }

    /// Replaces the host tools. Returns the registered names.
    pub async fn set_host_tools(&self, tools: Vec<HostToolDefinition>) -> Result<Vec<String>> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Names {
            tool_names: Vec<String>,
        }
        Ok(self.request::<Names>(Command::SetHostTools { tools }).await?.tool_names)
    }

    /// Replaces the host URI schemes. Returns the registered schemes.
    pub async fn set_host_uri_schemes(&self, schemes: Vec<HostUriScheme>) -> Result<Vec<String>> {
        #[derive(serde::Deserialize)]
        struct Schemes {
            schemes: Vec<String>,
        }
        Ok(self.request::<Schemes>(Command::SetHostUriSchemes { schemes }).await?.schemes)
    }

    pub async fn get_state(&self) -> Result<SessionState> {
        self.request(Command::GetState).await
    }

    pub async fn compact(&self, custom_instructions: Option<String>) -> Result<CompactionResult> {
        self.request(Command::Compact { custom_instructions }).await
    }

    pub async fn set_subagent_subscription(&self, level: SubagentLevel) -> Result<()> {
        self.request_unit(Command::SetSubagentSubscription { level }).await
    }

    pub async fn set_event_filter(&self, events: Option<Vec<String>>, message_updates: Option<MessageUpdates>) -> Result<()> {
        self.request_unit(Command::SetEventFilter { events, message_updates }).await
    }

    pub async fn set_ask_dialog(&self, enabled: bool) -> Result<()> {
        self.request_unit(Command::SetAskDialog { enabled }).await
    }

    pub async fn set_thinking_level(&self, level: impl Into<String>) -> Result<()> {
        self.request_unit(Command::SetThinkingLevel { level: level.into() }).await
    }

    /// Kills omp without waiting.
    pub fn kill(&self) {
        if let Some(kill) = self.inner.kill.locked().take() {
            let _ = kill.send(());
        }
    }

    /// Closes omp's stdin so it finishes queued work and exits, waits up to
    /// `grace`, then kills it. Returns the exit status when known.
    pub async fn shutdown(&self, grace: Duration) -> Result<Option<ExitStatus>> {
        self.inner.shared.stdin.lock().await.take();
        let mut exit = self.inner.exit.clone();
        let waited = tokio::time::timeout(grace, exit.wait_for(Option::is_some)).await.map(drop);
        if waited.is_err() {
            self.kill();
        }
        let info = exit.wait_for(Option::is_some).await.ok().and_then(|info| info.clone());
        for (_, task) in self.inner.shared.host_tasks.locked().drain() {
            task.abort();
        }
        Ok(info.and_then(|info| info.status))
    }
}

async fn collect_stderr(mut stderr: tokio::process::ChildStderr, shared: Arc<Shared>) {
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut tail = shared.stderr.locked();
                tail.extend(&buf[..n]);
                let excess = tail.len().saturating_sub(STDERR_TAIL);
                tail.drain(..excess);
            }
        }
    }
}

struct Reader {
    shared: Arc<Shared>,
    events: mpsc::Sender<Incoming>,
    ready: Option<oneshot::Sender<Ready>>,
    host: Option<Arc<dyn HostHandler>>,
    chunks: Reassembler,
}

impl Reader {
    async fn run(
        mut self,
        stdout: ChildStdout,
        mut child: Child,
        stderr_task: tokio::task::JoinHandle<()>,
        exit_tx: watch::Sender<Option<ExitInfo>>,
        mut kill_rx: oneshot::Receiver<()>,
    ) {
        let mut stdout = BufReader::new(stdout);
        let mut line = Vec::with_capacity(4096);
        let killed = loop {
            line.clear();
            tokio::select! {
                read = stdout.read_until(b'\n', &mut line) => match read {
                    Ok(0) | Err(_) => break false,
                    Ok(_) => self.handle_line(&line).await,
                },
                _ = &mut kill_rx => break true,
            }
        };
        let status = if killed {
            let _ = child.kill().await;
            child.wait().await.ok()
        } else {
            tokio::select! {
                status = child.wait() => status.ok(),
                _ = kill_rx => {
                    let _ = child.kill().await;
                    child.wait().await.ok()
                }
            }
        };
        // stderr closes with the process; a grandchild holding it open must
        // not keep the exit report back forever.
        let _ = tokio::time::timeout(Duration::from_secs(2), stderr_task).await;
        let info = ExitInfo { status, stderr: self.shared.stderr_tail() };
        self.shared.stdin.lock().await.take();
        let waiting = {
            let mut pending = self.shared.pending.locked();
            pending.exited = Some(info.clone());
            std::mem::take(&mut pending.waiting)
        };
        for (_, tx) in waiting {
            let _ = tx.send(Err(RpcError::Exited { status: info.status, stderr: info.stderr.clone() }));
        }
        for (_, task) in self.shared.host_tasks.locked().drain() {
            task.abort();
        }
        exit_tx.send_replace(Some(info.clone()));
        let _ = self.events.send(Incoming::Exited { status: info.status, stderr: info.stderr }).await;
    }

    async fn handle_line(&mut self, line: &[u8]) {
        let line = line.trim_ascii();
        tracing::trace!(target: FRAMES, frame = %String::from_utf8_lossy(line), "from omp");
        if line.is_empty() {
            return;
        }
        let frame = match Frame::parse(line) {
            Ok(frame) => frame,
            Err(err) => {
                tracing::warn!(%err, "omp wrote a line that isn't a JSON frame");
                return;
            }
        };
        let frame = match frame {
            Frame::Chunk(chunk) => match self.chunks.push(&chunk) {
                Ok(None) => return,
                Ok(Some(bytes)) => match std::str::from_utf8(&bytes).map_err(|e| e.to_string()).and_then(|text| {
                    Frame::parse(text.as_bytes()).map_err(|e| e.to_string())
                }) {
                    Ok(Frame::Chunk(_)) => {
                        tracing::warn!("a reassembled frame was itself a chunk");
                        return;
                    }
                    Ok(frame) => frame,
                    Err(err) => {
                        tracing::warn!(%err, "a reassembled frame is invalid");
                        return;
                    }
                },
                Err(err) => {
                    tracing::warn!(%err, "dropped an invalid chunk sequence");
                    return;
                }
            },
            other => {
                if let Some(err) = self.chunks.interrupt() {
                    tracing::warn!(%err, "dropped an interrupted chunk sequence");
                }
                other
            }
        };
        self.dispatch(frame).await;
    }

    async fn dispatch(&mut self, frame: Frame) {
        match frame {
            Frame::Ready(ready) => {
                if let Some(limit) = ready.max_reassembled_frame_bytes {
                    self.chunks = Reassembler::new(limit);
                }
                if let Some(tx) = self.ready.take() {
                    let _ = tx.send(ready);
                }
            }
            Frame::Response(response) => {
                let waiter = response.id.as_ref().and_then(|id| self.shared.pending.locked().waiting.remove(id));
                match waiter {
                    Some(tx) => {
                        let _ = tx.send(Ok(response));
                    }
                    None => self.deliver(Incoming::Response(response)).await,
                }
            }
            Frame::Event(event) => self.deliver(Incoming::Event(event)).await,
            Frame::Ui(request) => self.deliver(Incoming::Ui(request)).await,
            Frame::HostToolCall(call) => self.host_tool_call(call),
            Frame::HostUriRequest(request) => self.host_uri_request(request),
            Frame::HostToolCancel { target_id, .. } | Frame::HostUriCancel { target_id, .. } => {
                if let Some(task) = self.shared.host_tasks.locked().remove(&target_id) {
                    task.abort();
                }
            }
            Frame::Chunk(_) => {}
        }
    }

    async fn deliver(&self, incoming: Incoming) {
        // A dropped `Events` only means nobody listens; responses still flow.
        let _ = self.events.send(incoming).await;
    }

    fn host_tool_call(&self, call: HostToolCall) {
        let shared = self.shared.clone();
        let host = self.host.clone();
        let id = call.id.clone();
        // Held across the spawn so the task's own removal can't run before the insert.
        let mut tasks = self.shared.host_tasks.locked();
        let task = tokio::spawn({
            let id = id.clone();
            async move {
                let output = match host {
                    Some(host) => host.tool_call(call).await,
                    None => HostToolOutput::error("this host serves no tools"),
                };
                shared.host_tasks.locked().remove(&id);
                if let Err(err) = shared.write_json(&output.result_json(&id)).await {
                    tracing::warn!(%err, "couldn't send a host tool result");
                }
            }
        });
        tasks.insert(id, task.abort_handle());
    }

    fn host_uri_request(&self, request: HostUriRequest) {
        let shared = self.shared.clone();
        let host = self.host.clone();
        let id = request.id.clone();
        // Held across the spawn so the task's own removal can't run before the insert.
        let mut tasks = self.shared.host_tasks.locked();
        let task = tokio::spawn({
            let id = id.clone();
            async move {
                let output = match host {
                    Some(host) => host.uri_request(request).await,
                    None => HostUriOutput::Error("this host serves no URI schemes".to_owned()),
                };
                shared.host_tasks.locked().remove(&id);
                if let Err(err) = shared.write_json(&output.result_json(&id)).await {
                    tracing::warn!(%err, "couldn't send a host URI result");
                }
            }
        });
        tasks.insert(id, task.abort_handle());
    }
}

/// Sends a partial result for a running host tool call.
pub async fn send_tool_update(client: &RpcClient, call_id: &str, partial: &HostToolOutput) -> Result<()> {
    client.inner.shared.write_json(&partial.update_json(call_id)).await
}
