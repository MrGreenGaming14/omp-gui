//! Runs `claude` headlessly and drives it over stream JSON.
//!
//! The CLI runs with `--input-format stream-json --output-format stream-json`
//! and `--permission-prompt-tool stdio`, the mode the Agent SDK uses: prompts
//! and control requests go in on stdin, events and the CLI's own control
//! requests (permission prompts, SDK MCP messages) come out on stdout. The
//! process runs on [`StdioRpc`] with [`ClaudeControl`] framing.

use crate::claude::events::{Mapper, Output, UUID_ATTR, control_error, control_success, mcp_response};
use crate::claude::mcp::{self, HOST_SERVER};
use crate::claude::{HARNESS, native};
use crate::kit::rpc::Writer;
use crate::kit::{Framing, Inbound, Incoming, RpcError, StdioRpc};
use async_trait::async_trait;
use base64::Engine as _;
use loa_contracts::harness::{
    Adapter, AdapterError, Answer, Availability, Capabilities, ContextDelivery, Feature, HarnessModel, HarnessSession,
    McpTransport, ModelSelection, ModelSwitch, Prompt, Result, SessionConfig, SessionState,
};
use loa_core::{DecisionKind, Dom, ElementKind, NodeId, PatchSink, SCHEMA_VERSION, SessionMode, attr};
use loa_util::LockExt;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::watch;

/// Executable looked up on `PATH` when the config names none.
const EXECUTABLE: &str = "claude";
/// How long `shutdown` waits for the CLI to exit after stdin closes.
const EXIT_GRACE: Duration = Duration::from_secs(10);

/// The Claude Code harness.
#[derive(Clone, Debug)]
pub struct ClaudeAdapter {
    /// The CLI run when `SessionConfig::executable` is unset, and probed by
    /// [`Adapter::availability`].
    executable: PathBuf,
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self { executable: EXECUTABLE.into() }
    }
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs and probes `executable` instead of `claude` from `PATH`.
    pub fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self { executable: executable.into() }
    }

    /// `config` with this adapter's executable when it names none.
    fn configured(&self, mut config: SessionConfig) -> SessionConfig {
        config.executable.get_or_insert_with(|| self.executable.clone());
        config
    }
}

/// How the CLI picks up its conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    New,
    /// `--resume <id>`.
    Resume { native_id: String },
    /// `--resume <id> --fork-session`, cut after the message `at` when set.
    Fork { native_id: String, at: Option<String> },
}

/// The CLI arguments for a session.
pub fn command_args(config: &SessionConfig, launch: &Launch) -> Vec<String> {
    let mut args: Vec<String> = [
        "--print",
        "--output-format",
        "stream-json",
        "--verbose",
        "--input-format",
        "stream-json",
        "--forward-subagent-text",
        "--include-partial-messages",
        "--permission-prompt-tool",
        "stdio",
    ]
    .map(str::to_owned)
    .into();
    let mut flag = |name: &str, value: &str| {
        args.push(name.to_owned());
        args.push(value.to_owned());
    };
    if let Some(model) = &config.model {
        flag("--model", model);
    }
    if let Some(effort) = &config.effort {
        flag("--effort", effort);
    }
    if config.mode == Some(SessionMode::Plan) {
        flag("--permission-mode", "plan");
    }
    if let Some(context) = config.context.as_deref().filter(|c| !c.is_empty()) {
        flag("--append-system-prompt", context);
    }
    if !config.disabled_tools.is_empty() {
        flag("--disallowedTools", &config.disabled_tools.join(","));
    }
    if let Some(mcp) = mcp_config(config) {
        flag("--mcp-config", &mcp.to_string());
    }
    // Equals form so an id can never be read as a flag.
    match launch {
        Launch::New => {}
        Launch::Resume { native_id } => args.push(format!("--resume={native_id}")),
        Launch::Fork { native_id, at } => {
            args.push(format!("--resume={native_id}"));
            args.push("--fork-session".to_owned());
            if let Some(at) = at {
                args.push(format!("--resume-session-at={at}"));
            }
        }
    }
    args
}

/// The `--mcp-config` JSON: the configured servers plus the host tool server.
fn mcp_config(config: &SessionConfig) -> Option<Value> {
    let mut servers = Map::new();
    for server in &config.mcp_servers {
        let entry = match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                json!({"type": "stdio", "command": command, "args": args, "env": env})
            }
            McpTransport::Http { url, headers } => json!({"type": "http", "url": url, "headers": headers}),
        };
        servers.insert(server.name.clone(), entry);
    }
    if !config.host_tools.is_empty() {
        servers.insert(HOST_SERVER.to_owned(), json!({"type": "sdk", "name": HOST_SERVER}));
    }
    (!servers.is_empty()).then(|| json!({ "mcpServers": servers }))
}

#[async_trait]
impl Adapter for ClaudeAdapter {
    fn harness(&self) -> &'static str {
        HARNESS
    }

    /// `claude --version`, or the configured executable's.
    async fn availability(&self) -> Availability {
        loa_contracts::harness::Availability::probe(&self.executable, &["--version"]).await
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: SCHEMA_VERSION,
            // Fork: `--fork-session` (with `--resume-session-at` for a cut).
            // Compaction: `/compact`. Subagents: the Task/Agent tool.
            // Plan mode: the `plan` permission mode.
            features: vec![Feature::Fork, Feature::Compaction, Feature::Subagents, Feature::PlanMode],
            // `set_model` applies from the next request; one model per session.
            model_switch: ModelSwitch::PerTurn,
            decisions: vec![DecisionKind::Approval, DecisionKind::Question, DecisionKind::Plan],
            context_delivery: vec![ContextDelivery::SystemPrompt],
            host_tools: true,
            mcp_servers: true,
            queue_mid_turn: false,
            effort: true,
            import: true,
            export: true,
        }
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let mapper = Mapper::create(sink, &config)?;
        Ok(Box::new(ClaudeSession::launch(self.configured(config), mapper, &Launch::New).await?))
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let native_id = native_id(dom)?;
        let mapper = Mapper::attach(sink, dom)?;
        Ok(Box::new(ClaudeSession::launch(self.configured(config), mapper, &Launch::Resume { native_id }).await?))
    }

    async fn fork(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let native_id = native_id(dom)?;
        let at = fork_point(dom);
        let mapper = Mapper::attach(sink, dom)?;
        Ok(Box::new(ClaudeSession::launch(self.configured(config), mapper, &Launch::Fork { native_id, at }).await?))
    }

    fn import(&self, native: &Path, sink: &dyn PatchSink) -> Result<()> {
        native::import(native, sink)
    }

    fn export(&self, dom: &Dom, out: &Path) -> Result<()> {
        native::export(dom, out)
    }
}

fn native_id(dom: &Dom) -> Result<String> {
    dom.root()
        .and_then(|root| root.str_attr(attr::NATIVE_ID))
        .map(str::to_owned)
        .ok_or_else(|| AdapterError::Format("the session has no Claude Code session id".into()))
}

/// The uuid of the latest transcript line the DOM holds (`--resume-session-at`):
/// a fork of a DOM cut earlier than the native session keeps only that much.
pub fn fork_point(dom: &Dom) -> Option<String> {
    fn uuid(n: &loa_core::Node) -> Option<&str> {
        match n.kind {
            ElementKind::Message => n.str_attr(attr::ID),
            ElementKind::Call | ElementKind::Result => n.str_attr(UUID_ATTR),
            _ => None,
        }
    }
    // Node ids grow with insertion, so the highest id is the latest line.
    dom.iter().filter_map(|n| Some((n.id, uuid(n)?))).max_by_key(|(id, _)| *id).map(|(_, uuid)| uuid.to_owned())
}

/// Claude Code's SDK control protocol: `control_request` and
/// `control_response` lines correlated by `request_id`. Every other line,
/// the CLI's own control requests included, is an event for the mapper,
/// delivered in order as a notification named by its `type`.
pub struct ClaudeControl;

impl Framing for ClaudeControl {
    fn request(&self, id: &Value, method: &str, mut params: Value) -> Value {
        params["subtype"] = method.into();
        json!({"type": "control_request", "request_id": id, "request": params})
    }

    fn notification(&self, method: &str, mut params: Value) -> Value {
        params["type"] = method.into();
        params
    }

    fn response(&self, id: Value, result: std::result::Result<Value, RpcError>) -> Value {
        let id = id.as_str().unwrap_or_default();
        match result {
            Ok(response) => control_success(id, response),
            Err(RpcError::Command { message, .. }) => control_error(id, &message),
            Err(err) => control_error(id, &err.to_string()),
        }
    }

    fn classify(&self, mut msg: Value) -> Option<Incoming> {
        if msg.get("type").and_then(Value::as_str) != Some("control_response") {
            let method = msg.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
            return Some(Incoming::Notification { method, params: msg });
        }
        let mut response = msg.get_mut("response").map(Value::take).unwrap_or(Value::Null);
        let id = response.get("request_id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let result = match response.get("subtype").and_then(Value::as_str) {
            Some("error") => Err(RpcError::Command {
                command: "control".to_owned(),
                message: response.get("error").and_then(Value::as_str).unwrap_or("control request failed").to_owned(),
                code: None,
            }),
            _ => Ok(response.get_mut("response").map(Value::take).unwrap_or(Value::Null)),
        };
        Some(Incoming::Response { id, result })
    }
}

/// The [`Inbound`] side of a session: the mapper and what it needs to answer.
struct Shared {
    me: Weak<Shared>,
    mapper: Mutex<Mapper>,
    state: watch::Sender<SessionState>,
    writer: OnceLock<Writer<ClaudeControl>>,
    config: SessionConfig,
}

impl Shared {
    /// Runs `f` on the mapper and publishes the resulting state.
    fn mapped<T>(&self, f: impl FnOnce(&mut Mapper) -> T) -> T {
        let mut mapper = self.mapper.locked();
        let out = f(&mut mapper);
        if *self.state.borrow() != SessionState::Closed {
            self.state.send_replace(mapper.state());
        }
        out
    }

    fn write(&self, line: &Value) -> Result<()> {
        Ok(self.writer.get().ok_or(AdapterError::Closed)?.send(line)?)
    }

    fn act(&self, output: Output) {
        match output {
            Output::Write(line) => {
                if let Err(err) = self.write(&line) {
                    tracing::warn!(%err, "writing to claude failed");
                }
            }
            Output::Mcp { request_id, server, message } => {
                // Host tools can take a while; the reader keeps going.
                let Some(shared) = self.me.upgrade() else { return };
                tokio::spawn(async move {
                    let reply = if server == HOST_SERVER {
                        let config = &shared.config;
                        let handler = config.host_tool_handler.as_deref();
                        mcp::handle(&config.host_tools, handler, &config.session_id, &message).await
                    } else {
                        mcp::unknown_server(&server, &message)
                    };
                    if let Err(err) = shared.write(&mcp_response(&request_id, reply)) {
                        tracing::warn!(%err, "answering an MCP message failed");
                    }
                });
            }
        }
    }
}

#[async_trait]
impl Inbound for Shared {
    async fn on_request(&self, method: &str, _params: Value) -> std::result::Result<Value, RpcError> {
        Err(RpcError::Protocol(format!("unsupported control request `{method}`")))
    }

    async fn on_notification(&self, _kind: &str, msg: Value) {
        match self.mapped(|m| m.handle(msg)) {
            Ok(Some(output)) => self.act(output),
            Ok(None) => {}
            Err(err) => tracing::warn!(%err, "mapping a claude event failed"),
        }
    }

    async fn on_closed(&self, _status: Option<ExitStatus>, stderr: &str) {
        if !stderr.is_empty() {
            tracing::debug!(target: "claude", "{stderr}");
        }
        if let Err(err) = self.mapper.locked().exited() {
            tracing::warn!(%err, "recording the claude exit failed");
        }
        self.state.send_replace(SessionState::Closed);
    }
}

/// A running `claude` process.
pub struct ClaudeSession {
    shared: Arc<Shared>,
    rpc: StdioRpc<ClaudeControl>,
    /// The models the CLI listed in its `initialize` response; `None` when it
    /// listed none (older versions).
    models: Option<Vec<HarnessModel>>,
}

/// The `models` of an `initialize` response: `value` is what `set_model`
/// takes, `supportedEffortLevels` the efforts it accepts.
pub fn initialize_models(response: &Value) -> Option<Vec<HarnessModel>> {
    let text = |model: &Value, key: &str| model.get(key).and_then(Value::as_str).map(str::to_owned);
    let models = response.get("models")?.as_array()?.iter().filter_map(|model| {
        let id = text(model, "value")?;
        let levels = model.get("supportedEffortLevels").and_then(Value::as_array).into_iter().flatten();
        let efforts = levels.filter_map(|level| level.as_str().map(str::to_owned)).collect();
        Some(HarnessModel { name: text(model, "displayName").unwrap_or_else(|| id.clone()), id, provider: "anthropic".to_owned(), efforts })
    });
    Some(models.collect())
}

impl ClaudeSession {
    /// Spawns the CLI and completes the `initialize` handshake.
    pub async fn launch(config: SessionConfig, mapper: Mapper, launch: &Launch) -> Result<Self> {
        let mut command = Command::new(config.executable.as_deref().unwrap_or(Path::new(EXECUTABLE)));
        command
            .args(command_args(&config, launch))
            .current_dir(&config.cwd)
            .envs(&config.env)
            // Set inside a Claude Code shell; the child must not think it is nested.
            .env_remove("CLAUDECODE");
        let shared = Arc::new_cyclic(|me| Shared {
            me: me.clone(),
            state: watch::Sender::new(mapper.state()),
            mapper: Mutex::new(mapper),
            writer: OnceLock::new(),
            config,
        });
        let rpc = StdioRpc::spawn(command, ClaudeControl, shared.clone())?;
        let _ = shared.writer.set(rpc.writer());
        let mut session = Self { shared, rpc, models: None };
        let init = session.control("initialize", json!({"hooks": null})).await?;
        session.models = initialize_models(&init);
        Ok(session)
    }

    /// Claude Code's own session id, once the CLI reported it.
    pub fn native_id(&self) -> Option<String> {
        self.shared.mapper.locked().native_id().map(str::to_owned)
    }

    /// Sends a control request and waits for the CLI's answer.
    async fn control(&self, subtype: &str, request: Value) -> Result<Value> {
        let id = Value::from(format!("loa_{}", self.rpc.next_id()));
        self.rpc.request_as(id, subtype, request).await.map_err(|err| match err {
            RpcError::Command { message, .. } => AdapterError::Protocol(message),
            other => other.into(),
        })
    }

    async fn user_message(&self, prompt: &Prompt, uuid: &str) -> Result<Value> {
        let content = if prompt.attachments.is_empty() {
            Value::String(prompt.text.clone())
        } else {
            let mut blocks = vec![json!({"type": "text", "text": prompt.text})];
            for attachment in &prompt.attachments {
                blocks.push(attachment_block(&attachment.path, attachment.mime.as_deref()).await?);
            }
            Value::Array(blocks)
        };
        let session_id = self.native_id().unwrap_or_else(|| "default".to_owned());
        Ok(json!({
            "type": "user",
            "message": {"role": "user", "content": content},
            "parent_tool_use_id": null,
            "session_id": session_id,
            "uuid": uuid,
        }))
    }
}

/// An attachment as a content block: images inline, other files by path for
/// Claude Code's own Read tool.
async fn attachment_block(path: &Path, mime: Option<&str>) -> Result<Value> {
    match mime.or_else(|| loa_util::text::mime_of(path)).filter(|m| m.starts_with("image/") && *m != "image/svg+xml") {
        Some(media_type) => {
            let data = base64::engine::general_purpose::STANDARD.encode(tokio::fs::read(path).await?);
            Ok(json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data}}))
        }
        None => Ok(json!({"type": "text", "text": format!("Attached file: {}", path.display())})),
    }
}

#[async_trait]
impl HarnessSession for ClaudeSession {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        if self.state() == SessionState::Closed {
            return Err(AdapterError::Closed);
        }
        let uuid = uuid::Uuid::new_v4().to_string();
        let line = self.user_message(&prompt, &uuid).await?;
        self.shared.mapped(|m| m.open_turn(&prompt.text, &prompt.by, &uuid))?;
        self.shared.write(&line)
    }

    async fn interrupt(&self) -> Result<()> {
        if !self.shared.mapper.locked().turn_open() {
            return Ok(());
        }
        self.control("interrupt", json!({})).await?;
        self.shared.mapped(Mapper::interrupt)
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        match self.shared.mapped(|m| m.answer(decision, answer))? {
            Some(line) => self.shared.write(&line),
            None => Ok(()),
        }
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        if selection.role.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "model roles" });
        }
        self.control("set_model", json!({"model": selection.model})).await?;
        self.shared.mapper.locked().record_model(&selection.model)
    }

    async fn set_mode(&self, mode: SessionMode) -> Result<()> {
        let native = if mode == SessionMode::Plan { "plan" } else { "default" };
        self.control("set_permission_mode", json!({"mode": native})).await?;
        self.shared.mapper.locked().record_mode(mode)
    }

    /// The models from the CLI's `initialize` response.
    async fn models(&self) -> Result<Vec<HarnessModel>> {
        self.models.clone().ok_or(AdapterError::Unsupported { harness: HARNESS, what: "listing models with this claude version" })
    }

    async fn compact(&self, instructions: Option<String>) -> Result<()> {
        let text = instructions.map_or_else(|| "/compact".to_owned(), |i| format!("/compact {i}"));
        self.send(Prompt { text, attachments: Vec::new(), by: String::new() }).await
    }

    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn state(&self) -> SessionState {
        *self.shared.state.borrow()
    }

    async fn wait_idle(&self) -> Result<()> {
        let mut state = self.shared.state.subscribe();
        // The sender lives in `self.shared`, so it can't drop while we wait.
        let _ = state.wait_for(|s| matches!(s, SessionState::Idle | SessionState::Closed)).await;
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        self.shared.mapped(Mapper::interrupt)?;
        // Closing stdin ends the CLI's input stream; it exits on its own.
        self.rpc.shutdown(EXIT_GRACE).await;
        self.shared.state.send_replace(SessionState::Closed);
        Ok(())
    }
}
