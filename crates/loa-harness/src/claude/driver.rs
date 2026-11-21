//! Runs `claude` headlessly in stream JSON mode with `--permission-prompt-tool
//! stdio` (the Agent SDK's mode) on [`StdioRpc`] with [`ClaudeControl`] framing.

use crate::claude::mapper::{Mapper, Output, UUID_ATTR};
use crate::claude::mcp::{self, HOST_SERVER};
use crate::claude::protocol::{control_error, control_success};
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

const EXECUTABLE: &str = "claude";
/// How long `shutdown` waits for the CLI to exit after stdin closes.
const EXIT_GRACE: Duration = Duration::from_secs(10);

/// The Claude Code harness.
#[derive(Clone, Debug)]
pub struct ClaudeAdapter {
    executable: PathBuf,
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::with_executable(EXECUTABLE)
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

    async fn launch(&self, mut config: SessionConfig, mapper: Mapper, resume: &[String]) -> Result<Box<dyn HarnessSession>> {
        config.executable.get_or_insert_with(|| self.executable.clone());
        Ok(Box::new(ClaudeSession::launch(config, mapper, resume).await?))
    }
}

/// The CLI arguments for a session; `resume` picks up a native session.
fn command_args(config: &SessionConfig, resume: &[String]) -> Vec<String> {
    let mut args: Vec<String> = ["--print", "--output-format", "stream-json", "--verbose", "--input-format", "stream-json"]
        .into_iter()
        .chain(["--forward-subagent-text", "--include-partial-messages", "--permission-prompt-tool", "stdio"])
        .map(str::to_owned)
        .collect();
    let mut flag = |name: &str, value: &str| args.extend([name.to_owned(), value.to_owned()]);
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
    let mut servers = Map::new();
    for server in &config.mcp_servers {
        let entry = match &server.transport {
            McpTransport::Stdio { command, args, env } => json!({"type": "stdio", "command": command, "args": args, "env": env}),
            McpTransport::Http { url, headers } => json!({"type": "http", "url": url, "headers": headers}),
        };
        servers.insert(server.name.clone(), entry);
    }
    if !config.host_tools.is_empty() {
        servers.insert(HOST_SERVER.to_owned(), json!({"type": "sdk", "name": HOST_SERVER}));
    }
    if !servers.is_empty() {
        flag("--mcp-config", &json!({ "mcpServers": servers }).to_string());
    }
    args.extend(resume.iter().cloned());
    args
}

/// `--resume=<id>` (equals form, so an id can't read as a flag), plus the fork flags.
fn resume_args(dom: &Dom, fork: bool) -> Result<Vec<String>> {
    let id = dom.root().and_then(|root| root.str_attr(attr::NATIVE_ID));
    let id = id.ok_or_else(|| AdapterError::Format("the session has no Claude Code session id".into()))?;
    let mut args = vec![format!("--resume={id}")];
    if fork {
        args.push("--fork-session".to_owned());
        args.extend(fork_point(dom).map(|at| format!("--resume-session-at={at}")));
    }
    Ok(args)
}

/// The uuid of the latest transcript line the DOM holds, so a fork of a cut DOM keeps only that much.
fn fork_point(dom: &Dom) -> Option<String> {
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

#[async_trait]
impl Adapter for ClaudeAdapter {
    fn harness(&self) -> &'static str {
        HARNESS
    }

    async fn availability(&self) -> Availability {
        Availability::probe(&self.executable, &["--version"]).await
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: SCHEMA_VERSION,
            features: vec![Feature::Fork, Feature::Compaction, Feature::Subagents, Feature::PlanMode],
            // `set_model` applies from the next request.
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
        self.launch(config, mapper, &[]).await
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let resume = resume_args(dom, false)?;
        self.launch(config, Mapper::attach(sink, dom)?, &resume).await
    }

    async fn fork(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let resume = resume_args(dom, true)?;
        self.launch(config, Mapper::attach(sink, dom)?, &resume).await
    }

    fn import(&self, path: &Path, sink: &dyn PatchSink) -> Result<()> {
        native::import(path, sink)
    }

    fn export(&self, dom: &Dom, out: &Path) -> Result<()> {
        native::export(dom, out)
    }
}

/// Claude Code's SDK control protocol: `control_request` and `control_response`
/// lines correlated by `request_id`. Every other line, the CLI's own control
/// requests included, is an event for the mapper, delivered in order as a
/// notification named by its `type`.
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
        if msg["type"] != "control_response" {
            let method = msg["type"].as_str().unwrap_or_default().to_owned();
            return Some(Incoming::Notification { method, params: msg });
        }
        let mut response = msg["response"].take();
        let id = response["request_id"].as_str().unwrap_or_default().to_owned();
        let result = match response["subtype"].as_str() {
            Some("error") => {
                let message = response["error"].as_str().unwrap_or("control request failed").to_owned();
                Err(RpcError::Command { command: "control".to_owned(), message, code: None })
            }
            _ => Ok(response["response"].take()),
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
        self.state.send_replace(mapper.state());
        out
    }

    fn write(&self, line: &Value) -> Result<()> {
        Ok(self.writer.get().ok_or(AdapterError::Closed)?.send(line)?)
    }
}

#[async_trait]
impl Inbound for Shared {
    async fn on_request(&self, method: &str, _params: Value) -> std::result::Result<Value, RpcError> {
        Err(RpcError::Protocol(format!("unsupported control request `{method}`")))
    }

    async fn on_notification(&self, _kind: &str, msg: Value) {
        match self.mapped(|m| m.handle(msg)) {
            Ok(Some(Output::Write(line))) => {
                if let Err(err) = self.write(&line) {
                    tracing::warn!(%err, "writing to claude failed");
                }
            }
            // Host tools can take a while; the reader keeps going.
            Ok(Some(Output::Mcp { request_id, server, message })) => {
                let Some(shared) = self.me.upgrade() else { return };
                tokio::spawn(async move {
                    let config = &shared.config;
                    let reply = if server == HOST_SERVER {
                        mcp::handle(&config.host_tools, config.host_tool_handler.as_deref(), &config.session_id, &message).await
                    } else {
                        mcp::error(&message, format!("server `{server}` not found"))
                    };
                    if let Err(err) = shared.write(&control_success(&request_id, json!({ "mcp_response": reply }))) {
                        tracing::warn!(%err, "answering an MCP message failed");
                    }
                });
            }
            Ok(None) => {}
            Err(err) => tracing::warn!(%err, "mapping a claude event failed"),
        }
    }

    async fn on_closed(&self, _status: Option<ExitStatus>, stderr: &str) {
        if !stderr.is_empty() {
            tracing::debug!(target: "claude", "{stderr}");
        }
        if let Err(err) = self.mapped(Mapper::exited) {
            tracing::warn!(%err, "recording the claude exit failed");
        }
    }
}

/// A running `claude` process.
struct ClaudeSession {
    shared: Arc<Shared>,
    rpc: StdioRpc<ClaudeControl>,
    /// The models of the `initialize` response; `None` when the CLI lists none.
    models: Option<Vec<HarnessModel>>,
}

impl ClaudeSession {
    /// Spawns the CLI and completes the `initialize` handshake.
    async fn launch(config: SessionConfig, mapper: Mapper, resume: &[String]) -> Result<Self> {
        let mut command = Command::new(config.executable.as_deref().unwrap_or(Path::new(EXECUTABLE)));
        command.args(command_args(&config, resume)).current_dir(&config.cwd).envs(&config.env);
        // Set inside a Claude Code shell; the child must not think it is nested.
        command.env_remove("CLAUDECODE");
        let state = watch::Sender::new(mapper.state());
        let shared = Arc::new_cyclic(|me| Shared { me: me.clone(), mapper: Mutex::new(mapper), state, writer: OnceLock::new(), config });
        let rpc = StdioRpc::spawn(command, ClaudeControl, shared.clone())?;
        let _ = shared.writer.set(rpc.writer());
        let mut session = Self { shared, rpc, models: None };
        let init = session.control("initialize", json!({"hooks": null})).await?;
        session.models = init["models"].as_array().map(|models| models.iter().filter_map(model).collect());
        Ok(session)
    }

    /// Sends a control request and waits for the CLI's answer.
    async fn control(&self, subtype: &str, request: Value) -> Result<Value> {
        let id = Value::from(format!("loa_{}", self.rpc.next_id()));
        self.rpc.request_as(id, subtype, request).await.map_err(|err| match err {
            RpcError::Command { message, .. } => AdapterError::Protocol(message),
            other => other.into(),
        })
    }
}

/// One model of an `initialize` response: `value` is what `set_model` takes.
fn model(model: &Value) -> Option<HarnessModel> {
    let id = model["value"].as_str()?.to_owned();
    let levels = model["supportedEffortLevels"].as_array().into_iter().flatten();
    let efforts = levels.filter_map(|level| level.as_str().map(str::to_owned)).collect();
    let name = model["displayName"].as_str().map_or_else(|| id.clone(), str::to_owned);
    Some(HarnessModel { name, id, provider: "anthropic".to_owned(), efforts })
}

/// An attachment as a content block: images inline, other files by path for Claude Code's Read tool.
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
        let mut content = Value::String(prompt.text.clone());
        if !prompt.attachments.is_empty() {
            let mut blocks = vec![json!({"type": "text", "text": prompt.text})];
            for attachment in &prompt.attachments {
                blocks.push(attachment_block(&attachment.path, attachment.mime.as_deref()).await?);
            }
            content = Value::Array(blocks);
        }
        let uuid = uuid::Uuid::new_v4().to_string();
        let session_id = self.shared.mapper.locked().native_id().unwrap_or("default").to_owned();
        let message = json!({"role": "user", "content": content});
        let line = json!({"type": "user", "message": message, "parent_tool_use_id": null, "session_id": session_id, "uuid": uuid});
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
        // The sender lives in `self.shared`, so it can't drop while we wait.
        let _ = self.shared.state.subscribe().wait_for(|s| matches!(s, SessionState::Idle | SessionState::Closed)).await;
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        self.shared.mapped(Mapper::interrupt)?;
        // Closing stdin ends the CLI's input stream; it exits on its own.
        self.rpc.shutdown(EXIT_GRACE).await;
        self.shared.mapped(Mapper::exited)
    }
}
