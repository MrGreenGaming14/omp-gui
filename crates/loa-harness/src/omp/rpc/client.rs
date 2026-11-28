//! The process side: `omp --mode rpc` on [`StdioRpc`] with [`OmpFrames`],
//! the `ready` handshake and protocol negotiation, host callbacks, and
//! everything else delivered in order as [`Incoming`].

use loa_util::exec::NoWindow;
use super::command::{Command, Ready};
use super::event::{Frame, HostToolCall, UiRequest};
use super::frames::OmpFrames;
use super::inbound::{HostToolOutput, UiResponse};
use super::Event;
use crate::kit::rpc::{self, Inbound, Result, RpcError, StdioRpc};
use async_trait::async_trait;
use loa_util::LockExt;
use serde::de::DeserializeOwned;
use serde::Deserialize as _;
use serde_json::Value;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
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
    /// Variables of this process the child must not inherit.
    pub env_remove: Vec<OsString>,
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
            env_remove: Vec::new(),
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
        let mut command = tokio::process::Command::new(loa_util::exec::resolve(&self.executable));
        command.no_window();
        command.arg("--mode").arg(if self.mode == Mode::RpcUi { "rpc-ui" } else { "rpc" });
        if self.no_ui {
            command.arg("--no-ui");
        }
        for key in &self.env_remove {
            command.env_remove(key);
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
    /// omp is gone. Always the last item.
    Exited { status: Option<ExitStatus>, stderr: String },
}

/// The stream of [`Incoming`] frames, in the order omp wrote them; it ends
/// after [`Incoming::Exited`].
pub type Events = mpsc::Receiver<Incoming>;

/// omp's side of the kit: the ready handshake, host callbacks and in-order delivery.
struct OmpInbound {
    events: mpsc::Sender<Incoming>,
    ready: Mutex<Option<oneshot::Sender<Ready>>>,
    host: Option<Arc<dyn HostHandler>>,
}

#[async_trait]
impl Inbound for OmpInbound {
    /// Host URI requests fail: this client registers no schemes.
    async fn on_request(&self, method: &str, params: Value) -> Result<Value> {
        let output = match (method, &self.host) {
            ("host_tool_call", Some(host)) => host.tool_call(HostToolCall::deserialize(&params)?).await,
            ("host_tool_call", None) => HostToolOutput::error("this host serves no tools"),
            _ => return Err(RpcError::Protocol("this host serves no URI schemes".to_owned())),
        };
        Ok(output.result_json(params["id"].as_str().unwrap_or_default()))
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
            // A response no command waits for, such as a `parse` failure.
            "response" => Incoming::Event(Event::Unknown(params)),
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

/// An [`RpcClient`] that doesn't keep omp running; see [`RpcClient::downgrade`].
#[derive(Clone)]
pub struct WeakRpcClient(Weak<Inner>);

impl WeakRpcClient {
    /// The client, while any [`RpcClient`] clone is alive.
    pub fn upgrade(&self) -> Option<RpcClient> {
        self.0.upgrade().map(|inner| RpcClient { inner })
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
    let out = loa_util::exec::probe_cli(&loa_util::exec::resolve(&executable.into()), &["--version"], Duration::from_secs(30)).await?;
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
            client.request::<Value>(Command::NegotiateProtocol { protocol_version: chosen }).await?;
        }
        client.inner.protocol.store(chosen, Ordering::Release);
        Ok((client, rx))
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

    /// Sends `command` under `id` and decodes the response `data` as `T`; a
    /// missing `data` decodes like an empty object.
    pub async fn request_as<T: DeserializeOwned>(&self, id: String, command: Command) -> Result<T> {
        let frame = serde_json::to_value(&command)?;
        let method = frame["type"].as_str().unwrap_or_default().to_owned();
        rpc::decode(self.inner.rpc.request_as(Value::String(id), &method, frame).await?)
    }

    /// [`RpcClient::request_as`] under a fresh id.
    pub async fn request<T: DeserializeOwned>(&self, command: Command) -> Result<T> {
        self.request_as(self.reserve_id(), command).await
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

    /// A handle that sends commands while the session holds the client, but
    /// doesn't keep omp running once every `RpcClient` clone is dropped.
    pub fn downgrade(&self) -> WeakRpcClient {
        WeakRpcClient(Arc::downgrade(&self.inner))
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
