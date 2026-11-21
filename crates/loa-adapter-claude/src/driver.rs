//! Runs `claude` headlessly and drives it over stream JSON.
//!
//! The CLI runs with `--input-format stream-json --output-format stream-json`
//! and `--permission-prompt-tool stdio`, the mode the Agent SDK uses: prompts
//! and control requests go in on stdin, events and the CLI's own control
//! requests (permission prompts, SDK MCP messages) come out on stdout.

use loa_util::LockExt;
use crate::events::{Mapper, Output, UUID_ATTR, mcp_response};
use crate::mcp::{self, HOST_SERVER};
use crate::{HARNESS, native};
use async_trait::async_trait;
use loa_contracts::harness::{
    Adapter, AdapterError, Answer, Availability, Capabilities, ContextDelivery, Feature, HarnessModel, HarnessSession,
    McpTransport, ModelSelection, ModelSwitch, Prompt, Result, SessionConfig, SessionState,
};
use loa_core::{DecisionKind, Dom, ElementKind, NodeId, PatchSink, SCHEMA_VERSION, SessionMode, attr};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

/// Executable looked up on `PATH` when the config names none.
const EXECUTABLE: &str = "claude";
/// Lines of stderr kept for error messages.
const STDERR_LINES: usize = 20;
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

/// State shared by the session handle and its reader task.
struct Shared {
    mapper: Mutex<Mapper>,
    state: watch::Sender<SessionState>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    /// Control requests the host sent, by request id.
    controls: Mutex<HashMap<String, oneshot::Sender<std::result::Result<Value, String>>>>,
    next_request: AtomicU64,
    stderr: Mutex<VecDeque<String>>,
    config: SessionConfig,
}


impl Shared {
    fn publish(&self, mapper: &Mapper) {
        if *self.state.borrow() != SessionState::Closed {
            self.state.send_replace(mapper.state());
        }
    }

    fn closed(&self) -> bool {
        *self.state.borrow() == SessionState::Closed
    }

    async fn write(&self, line: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(line).map_err(|e| AdapterError::Protocol(e.to_string()))?;
        bytes.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        let stdin = stdin.as_mut().ok_or(AdapterError::Closed)?;
        stdin.write_all(&bytes).await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Sends a control request and waits for the CLI's answer.
    async fn control(&self, request: Value) -> Result<Value> {
        let id = format!("loa_{}", self.next_request.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.controls.locked().insert(id.clone(), tx);
        let line = json!({"type": "control_request", "request_id": id, "request": request});
        if let Err(err) = self.write(&line).await {
            self.controls.locked().remove(&id);
            return Err(err);
        }
        match rx.await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) => Err(AdapterError::Protocol(error)),
            Err(_) => Err(self.exit_error()),
        }
    }

    fn exit_error(&self) -> AdapterError {
        let stderr = self.stderr.locked();
        if stderr.is_empty() {
            return AdapterError::Closed;
        }
        let tail: Vec<&str> = stderr.iter().map(String::as_str).collect();
        AdapterError::Protocol(format!("claude exited: {}", tail.join("\n")))
    }

    async fn act(self: &Arc<Self>, output: Output) {
        match output {
            Output::Write(line) => {
                if let Err(err) = self.write(&line).await {
                    tracing::warn!(%err, "writing to claude failed");
                }
            }
            Output::Mcp { request_id, server, message } => {
                // Host tools can take a while; the reader keeps going.
                let shared = Arc::clone(self);
                tokio::spawn(async move {
                    let reply = if server == HOST_SERVER {
                        let config = &shared.config;
                        let handler = config.host_tool_handler.as_deref();
                        mcp::handle(&config.host_tools, handler, &config.session_id, &message).await
                    } else {
                        mcp::unknown_server(&server, &message)
                    };
                    if let Err(err) = shared.write(&mcp_response(&request_id, reply)).await {
                        tracing::warn!(%err, "answering an MCP message failed");
                    }
                });
            }
            Output::ControlResponse { request_id, result } => {
                if let Some(waiter) = self.controls.locked().remove(&request_id) {
                    let _ = waiter.send(result);
                }
            }
        }
    }

    fn exited(&self) {
        {
            let mut mapper = self.mapper.locked();
            if let Err(err) = mapper.exited() {
                tracing::warn!(%err, "recording the claude exit failed");
            }
        }
        self.state.send_replace(SessionState::Closed);
        // Dropping the senders fails every waiting control request.
        self.controls.locked().clear();
    }
}

async fn read_stdout(shared: Arc<Shared>, stdout: ChildStdout) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(err) => {
                tracing::warn!(%err, "reading claude stdout failed");
                break;
            }
        };
        let line = line.trim();
        // Some builds print diagnostics on stdout.
        if !line.starts_with('{') {
            continue;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(err) => {
                tracing::warn!(%err, "claude printed a malformed line");
                continue;
            }
        };
        let output = {
            let mut mapper = shared.mapper.locked();
            let output = mapper.handle(message);
            shared.publish(&mapper);
            output
        };
        match output {
            Ok(Some(output)) => shared.act(output).await,
            Ok(None) => {}
            Err(err) => tracing::warn!(%err, "mapping a claude event failed"),
        }
    }
    shared.exited();
}

/// A running `claude` process.
pub struct ClaudeSession {
    shared: Arc<Shared>,
    child: tokio::sync::Mutex<Option<Child>>,
    reader: Mutex<Option<JoinHandle<()>>>,
    /// The models the CLI listed in its `initialize` response; `None` when it
    /// listed none (older versions).
    models: Option<Vec<HarnessModel>>,
}

/// The `models` of an `initialize` response: `value` is what `set_model`
/// takes, `supportedEffortLevels` the efforts it accepts.
pub fn initialize_models(response: &Value) -> Option<Vec<HarnessModel>> {
    let models = response.get("models")?.as_array()?;
    let text = |model: &Value, key: &str| model.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(
        models
            .iter()
            .filter_map(|model| {
                let id = text(model, "value")?;
                let efforts = model.get("supportedEffortLevels").and_then(Value::as_array).map_or_else(Vec::new, |levels| {
                    levels.iter().filter_map(|level| level.as_str().map(str::to_owned)).collect()
                });
                Some(HarnessModel {
                    name: text(model, "displayName").unwrap_or_else(|| id.clone()),
                    id,
                    provider: "anthropic".to_owned(),
                    efforts,
                })
            })
            .collect(),
    )
}

impl ClaudeSession {
    /// Spawns the CLI and completes the `initialize` handshake.
    pub async fn launch(config: SessionConfig, mapper: Mapper, launch: &Launch) -> Result<Self> {
        let executable = config.executable.clone().unwrap_or_else(|| EXECUTABLE.into());
        let mut command = Command::new(&executable);
        command
            .args(command_args(&config, launch))
            .current_dir(&config.cwd)
            .envs(&config.env)
            // Set inside a Claude Code shell; the child must not think it is nested.
            .env_remove("CLAUDECODE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => AdapterError::NotInstalled(executable.display().to_string()),
            _ => AdapterError::Io(err),
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(AdapterError::Protocol("claude started without piped stdio".into()));
        };
        let (state, _) = watch::channel(mapper.state());
        let shared = Arc::new(Shared {
            mapper: Mutex::new(mapper),
            state,
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            controls: Mutex::new(HashMap::new()),
            next_request: AtomicU64::new(1),
            stderr: Mutex::new(VecDeque::with_capacity(STDERR_LINES)),
            config,
        });
        let stderr_shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "claude", "{line}");
                let mut tail = stderr_shared.stderr.locked();
                if tail.len() == STDERR_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });
        let reader = tokio::spawn(read_stdout(Arc::clone(&shared), stdout));
        let mut session =
            Self { shared, child: tokio::sync::Mutex::new(Some(child)), reader: Mutex::new(Some(reader)), models: None };
        let init = session.shared.control(json!({"subtype": "initialize", "hooks": null})).await?;
        session.models = initialize_models(&init);
        Ok(session)
    }

    /// Claude Code's own session id, once the CLI reported it.
    pub fn native_id(&self) -> Option<String> {
        self.shared.mapper.locked().native_id().map(str::to_owned)
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
    let guessed = path.extension().and_then(|e| e.to_str()).and_then(|e| match e.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    });
    match mime.or(guessed).filter(|m| m.starts_with("image/")) {
        Some(media_type) => {
            let bytes = tokio::fs::read(path).await?;
            Ok(json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": base64(&bytes)}}))
        }
        None => Ok(json!({"type": "text", "text": format!("Attached file: {}", path.display())})),
    }
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[async_trait]
impl HarnessSession for ClaudeSession {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        if self.shared.closed() {
            return Err(AdapterError::Closed);
        }
        let uuid = uuid::Uuid::new_v4().to_string();
        let line = self.user_message(&prompt, &uuid).await?;
        {
            let mut mapper = self.shared.mapper.locked();
            mapper.open_turn(&prompt.text, &prompt.by, &uuid)?;
            self.shared.publish(&mapper);
        }
        self.shared.write(&line).await
    }

    async fn interrupt(&self) -> Result<()> {
        if !self.shared.mapper.locked().turn_open() {
            return Ok(());
        }
        self.shared.control(json!({"subtype": "interrupt"})).await?;
        let mut mapper = self.shared.mapper.locked();
        mapper.interrupt()?;
        self.shared.publish(&mapper);
        Ok(())
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        let response = {
            let mut mapper = self.shared.mapper.locked();
            let response = mapper.answer(decision, answer)?;
            self.shared.publish(&mapper);
            response
        };
        match response {
            Some(line) => self.shared.write(&line).await,
            None => Ok(()),
        }
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        if selection.role.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "model roles" });
        }
        self.shared.control(json!({"subtype": "set_model", "model": selection.model})).await?;
        self.shared.mapper.locked().record_model(&selection.model)
    }

    async fn set_mode(&self, mode: SessionMode) -> Result<()> {
        let native = match mode {
            SessionMode::Plan => "plan",
            SessionMode::Normal => "default",
        };
        self.shared.control(json!({"subtype": "set_permission_mode", "mode": native})).await?;
        self.shared.mapper.locked().record_mode(mode)
    }

    /// The models from the CLI's `initialize` response.
    async fn models(&self) -> Result<Vec<HarnessModel>> {
        self.models.clone().ok_or(AdapterError::Unsupported { harness: HARNESS, what: "listing models with this claude version" })
    }

    async fn compact(&self, instructions: Option<String>) -> Result<()> {
        let text = match instructions {
            Some(instructions) => format!("/compact {instructions}"),
            None => "/compact".to_owned(),
        };
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
        {
            let mut mapper = self.shared.mapper.locked();
            mapper.interrupt()?;
            self.shared.publish(&mapper);
        }
        // Closing stdin ends the CLI's input stream; it exits on its own.
        self.shared.stdin.lock().await.take();
        if let Some(mut child) = self.child.lock().await.take() {
            match tokio::time::timeout(EXIT_GRACE, child.wait()).await {
                Ok(status) => {
                    status?;
                }
                Err(_) => child.kill().await?,
            }
        }
        let reader = self.reader.locked().take();
        if let Some(reader) = reader {
            let _ = reader.await;
        }
        self.shared.state.send_replace(SessionState::Closed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        let cases = [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("foobar", "Zm9vYmFy")];
        for (plain, encoded) in cases {
            assert_eq!(base64(plain.as_bytes()), encoded);
        }
    }
}
