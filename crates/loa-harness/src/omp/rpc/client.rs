//! The process side: `omp --mode rpc` on [`StdioRpc`] with [`OmpFrames`],
//! the `ready` handshake and protocol negotiation, host callbacks, and
//! everything else delivered in order as [`Incoming`].

use super::command::{
    CancellationResult, Command, CompactionResult, HostToolDefinition, HostUriScheme, ImageContent, MessageUpdates,
    ModelInfo, PromptAck, Ready, SessionState, StreamingBehavior, SubagentLevel,
};
use super::event::{Frame, HostToolCall, HostUriRequest, Response, UiRequest};
use super::frames::OmpFrames;
use super::inbound::{HostToolOutput, HostUriOutput, UiResponse};
use super::Event;
use crate::kit::rpc::{self, Inbound, Result, RpcError, StdioRpc};
use async_trait::async_trait;
use loa_util::LockExt;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// Lowest protocol version this client speaks.
pub const MIN_PROTOCOL: u32 = 1;
/// Highest protocol version this client speaks.
pub const MAX_PROTOCOL: u32 = 2;

/// `--mode` of the child process.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// `rpc`: extension UI, approvals and login prompts reach the host.
    #[default]
    Rpc,
    /// `rpc-ui`: also enables the tool UI context, so the `ask` tool works.
    RpcUi,
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

    fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.executable);
        command.arg("--mode").arg(if self.mode == Mode::RpcUi { "rpc-ui" } else { "rpc" });
        if self.no_ui {
            command.arg("--no-ui");
        }
        command.args(&self.args).envs(self.env.iter().map(|(k, v)| (k, v)));
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command
    }
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self::new("omp")
    }
}

impl std::fmt::Debug for SpawnOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("SpawnOptions");
        d.field("executable", &self.executable).field("mode", &self.mode).field("args", &self.args);
        d.field("cwd", &self.cwd).finish_non_exhaustive()
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

/// omp's side of the kit: the ready handshake, host callbacks and in-order delivery.
struct OmpInbound {
    events: mpsc::Sender<Incoming>,
    ready: Mutex<Option<oneshot::Sender<Ready>>>,
    host: Option<Arc<dyn HostHandler>>,
}

#[async_trait]
impl Inbound for OmpInbound {
    async fn on_request(&self, method: &str, params: Value) -> Result<Value> {
        let id = params["id"].as_str().unwrap_or_default().to_owned();
        let host = self.host.as_ref();
        Ok(match method {
            "host_tool_call" => match host {
                Some(host) => host.tool_call(HostToolCall::deserialize(&params)?).await,
                None => HostToolOutput::error("this host serves no tools"),
            }
            .result_json(&id),
            _ => match host {
                Some(host) => host.uri_request(HostUriRequest::deserialize(&params)?).await,
                None => HostUriOutput::Error("this host serves no URI schemes".to_owned()),
            }
            .result_json(&id),
        })
    }

    async fn on_notification(&self, method: &str, params: Value) {
        // A dropped `Events` only means nobody listens; responses still flow.
        let incoming = match method {
            "ready" => {
                let ready = Ready::deserialize(&params).unwrap_or_default();
                if let Some(tx) = self.ready.locked().take() {
                    let _ = tx.send(ready);
                }
                return;
            }
            "response" => match Response::deserialize(&params) {
                Ok(response) => Incoming::Response(response),
                Err(_) => Incoming::Event(Event::Unknown(params)),
            },
            _ => match Frame::from_json(params) {
                Frame::Event(event) => Incoming::Event(event),
                Frame::Ui(request) => Incoming::Ui(request),
            },
        };
        let _ = self.events.send(incoming).await;
    }

    async fn on_closed(&self, status: Option<ExitStatus>, stderr: &str) {
        let _ = self.events.send(Incoming::Exited { status, stderr: stderr.to_owned() }).await;
    }
}

struct Inner {
    rpc: StdioRpc<OmpFrames>,
    ready: Ready,
    protocol: AtomicU32,
}

/// Answers UI requests for an [`RpcClient`] without keeping omp running;
/// see [`RpcClient::ui_responder`].
#[derive(Clone)]
pub struct UiResponder(rpc::Writer<OmpFrames>);

impl UiResponder {
    pub async fn respond(&self, response: &UiResponse) -> Result<()> {
        self.0.send(&response.to_json())
    }
}

/// A running `omp --mode rpc` process. Cheap to clone; every clone talks to
/// the same process, which is killed when the last clone drops.
#[derive(Clone)]
pub struct RpcClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for RpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcClient").field("protocol", &self.protocol_version()).finish_non_exhaustive()
    }
}

/// Runs `<executable> --version` and returns what it printed, trimmed.
pub async fn omp_version(executable: impl Into<PathBuf>) -> Result<String> {
    let out = loa_util::exec::probe_cli(&executable.into(), &["--version"], Duration::from_secs(30)).await?;
    Ok(out.trim().to_owned())
}

impl RpcClient {
    /// Starts omp, waits for its `ready` frame and negotiates the protocol
    /// version. Fails with [`RpcError::Version`] when omp offers no version in
    /// `MIN_PROTOCOL..=MAX_PROTOCOL`.
    pub async fn spawn(opts: SpawnOptions) -> Result<(RpcClient, Events)> {
        let (events, rx) = mpsc::channel(opts.event_capacity.max(1));
        let (ready_tx, ready_rx) = oneshot::channel();
        let inbound = OmpInbound { events, ready: Mutex::new(Some(ready_tx)), host: opts.host.clone() };
        let rpc = StdioRpc::spawn(opts.command(), OmpFrames::default(), Arc::new(inbound))?;
        let ready = match tokio::time::timeout(opts.ready_timeout, ready_rx).await {
            Ok(Ok(ready)) => ready,
            Ok(Err(_)) => return Err(rpc.closed().await),
            Err(_) => {
                rpc.kill();
                return Err(RpcError::StartTimeout(opts.ready_timeout));
            }
        };
        let client = RpcClient { inner: Arc::new(Inner { rpc, ready, protocol: AtomicU32::new(1) }) };
        let offered = client.inner.ready.offered_versions();
        let Some(chosen) = offered.iter().copied().filter(|v| (MIN_PROTOCOL..=MAX_PROTOCOL).contains(v)).max() else {
            client.kill();
            return Err(RpcError::Version { offered, min: MIN_PROTOCOL, max: MAX_PROTOCOL });
        };
        let chosen = if chosen >= 2 && !opts.negotiate { 1 } else { chosen };
        if chosen >= 2 {
            client.request_unit(Command::NegotiateProtocol { protocol_version: chosen }).await?;
        }
        client.inner.protocol.store(chosen, Ordering::Release);
        Ok((client, Events { rx }))
    }

    /// The `ready` frame omp sent at startup.
    pub fn ready(&self) -> &Ready {
        &self.inner.ready
    }

    /// The protocol version in use after negotiation.
    pub fn protocol_version(&self) -> u32 {
        self.inner.protocol.load(Ordering::Acquire)
    }

    pub fn has_exited(&self) -> bool {
        self.inner.rpc.has_exited()
    }

    /// A fresh command id. Reserve one when frames that will refer to the
    /// command (such as a prompt's `prompt_result`) must be expected before
    /// it is sent, then send it with [`RpcClient::request_as`].
    pub fn reserve_id(&self) -> String {
        format!("c{}", self.inner.rpc.next_id())
    }

    /// Sends `command` under `id` and returns the response `data`.
    pub async fn request_as(&self, id: String, command: Command) -> Result<Value> {
        let frame = serde_json::to_value(&command)?;
        let method = frame["type"].as_str().unwrap_or_default().to_owned();
        self.inner.rpc.request_as(Value::String(id), &method, frame).await
    }

    /// Sends `command` and returns the response `data`.
    pub async fn request_raw(&self, command: Command) -> Result<Value> {
        self.request_as(self.reserve_id(), command).await
    }

    /// Sends `command` and decodes the response `data` as `T`; a missing
    /// `data` decodes like an empty object.
    pub async fn request<T: DeserializeOwned>(&self, command: Command) -> Result<T> {
        rpc::decode(self.request_raw(command).await?)
    }

    /// Sends `command` and ignores the response `data`.
    pub async fn request_unit(&self, command: Command) -> Result<()> {
        self.request_raw(command).await.map(drop)
    }

    /// Answers a dialog UI request.
    pub async fn respond_ui(&self, response: &UiResponse) -> Result<()> {
        self.inner.rpc.send(&response.to_json())
    }

    /// Answers UI requests like [`RpcClient::respond_ui`] without keeping omp
    /// running: once every `RpcClient` clone is dropped, the writes fail.
    pub fn ui_responder(&self) -> UiResponder {
        UiResponder(self.inner.rpc.writer())
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
        rpc::decode(self.request_as(id, Command::Prompt { message: message.into(), images, streaming_behavior }).await?)
    }

    /// Queues `message` as steering for the running turn.
    pub async fn steer(&self, message: impl Into<String>) -> Result<()> {
        self.request_unit(Command::Steer { message: message.into(), images: Vec::new() }).await
    }

    /// Withdraws queued user input and aborts, atomically. Unlike `abort`,
    /// which leaves queued follow-ups waiting, every withdrawn prompt then
    /// completes `aborted`.
    pub async fn abort_and_restore_queue(&self) -> Result<()> {
        self.request_unit(Command::AbortAndRestoreQueue).await
    }

    pub async fn fork(&self, entry_id: Option<String>) -> Result<CancellationResult> {
        self.request(Command::Fork { entry_id }).await
    }

    pub async fn set_model(&self, provider: impl Into<String>, model_id: impl Into<String>) -> Result<ModelInfo> {
        self.request(Command::SetModel { provider: provider.into(), model_id: model_id.into() }).await
    }

    pub async fn get_available_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(self.request::<List<ModelInfo>>(Command::GetAvailableModels).await?.models)
    }

    /// Replaces the host tools. Returns the registered names.
    pub async fn set_host_tools(&self, tools: Vec<HostToolDefinition>) -> Result<Vec<String>> {
        Ok(self.request::<List<String>>(Command::SetHostTools { tools }).await?.tool_names)
    }

    /// Replaces the host URI schemes. Returns the registered schemes.
    pub async fn set_host_uri_schemes(&self, schemes: Vec<HostUriScheme>) -> Result<Vec<String>> {
        Ok(self.request::<List<String>>(Command::SetHostUriSchemes { schemes }).await?.schemes)
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
        self.inner.rpc.kill();
    }

    /// Closes omp's stdin so it finishes queued work and exits, waits up to
    /// `grace`, then kills it. Returns the exit status when known.
    pub async fn shutdown(&self, grace: Duration) -> Result<Option<ExitStatus>> {
        Ok(self.inner.rpc.shutdown(grace).await)
    }
}

/// The list fields of `get_available_models`, `set_host_tools` and `set_host_uri_schemes`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct List<T> {
    #[serde(default = "Vec::new")]
    models: Vec<T>,
    #[serde(default = "Vec::new")]
    tool_names: Vec<T>,
    #[serde(default = "Vec::new")]
    schemes: Vec<T>,
}
