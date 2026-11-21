//! The Codex process driver: `codex app-server` on [`StdioRpc`] with
//! [`JsonRpc2`] framing. Notifications and server requests reach the
//! [`Mapper`] in read order; its replies go back through the same pipe.

use crate::codex::mapper::{Mapper, Reply, host_tool_result};
use crate::codex::{CodexAdapter, HARNESS};
use crate::kit::rpc::Writer;
use crate::kit::{Framing, Inbound, JsonRpc2, RpcError, StdioRpc};
use async_trait::async_trait;
use loa_contracts::harness::{
    AdapterError, Answer, HarnessModel, HarnessSession, HostTools, McpTransport, ModelSelection, Prompt, Result,
    SessionConfig, SessionState,
};
use loa_core::{NodeId, SessionMode};
use loa_util::LockExt;
use serde_json::{Value as Json, json};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::watch;

/// Codex's app server speaks JSON-RPC without the `jsonrpc` header.
const FRAMES: JsonRpc2 = JsonRpc2 { header: false };

/// Settings applied to every `turn/start`.
#[derive(Default)]
struct TurnSettings {
    model: Option<String>,
    effort: Option<String>,
    mode: Option<SessionMode>,
}

/// The [`Inbound`] side: the mapper, once the thread exists.
struct Shared {
    mapper: Mutex<Option<Mapper>>,
    writer: OnceLock<Writer<JsonRpc2>>,
    state: watch::Sender<SessionState>,
    session_id: String,
    host_tools: Option<Arc<dyn HostTools>>,
}

impl Shared {
    /// Runs `f` on the mapper and publishes the resulting state.
    fn mapped<T>(&self, f: impl FnOnce(&mut Mapper) -> Result<T>) -> Result<T> {
        let mut mapper = self.mapper.locked();
        let mapper = mapper.as_mut().ok_or(AdapterError::Closed)?;
        let out = f(mapper);
        if *self.state.borrow() != SessionState::Closed {
            self.state.send_replace(mapper.state());
        }
        out
    }

    fn write(&self, frame: &Json) -> Result<()> {
        Ok(self.writer.get().ok_or(AdapterError::Closed)?.send(frame)?)
    }

    fn reply(&self, reply: Reply) -> Result<()> {
        match reply {
            Reply::Result { id, result } => self.write(&FRAMES.response(id, Ok(result))),
            Reply::Error { id, code, message } => {
                let error = RpcError::Command { command: String::new(), message, code: Some(code.to_string()) };
                self.write(&FRAMES.response(id, Err(error)))
            }
            Reply::HostTool { id, tool, arguments } => {
                let (Some(handler), Some(writer)) = (self.host_tools.clone(), self.writer.get().cloned()) else {
                    let result = host_tool_result(Err(format!("no handler for app tool `{tool}`")));
                    return self.write(&FRAMES.response(id, Ok(result)));
                };
                let session = self.session_id.clone();
                tokio::spawn(async move {
                    let outcome = handler.call(&session, &tool, &arguments).await;
                    if let Err(error) = writer.send(&FRAMES.response(id, Ok(host_tool_result(outcome)))) {
                        tracing::warn!(%error, "codex: couldn't return an app tool result");
                    }
                });
                Ok(())
            }
        }
    }
}

#[async_trait]
impl Inbound for Shared {
    async fn on_request(&self, method: &str, _params: Json) -> std::result::Result<Json, RpcError> {
        Err(RpcError::Protocol(format!("{method} isn't served on its own task")))
    }

    fn defers_requests(&self) -> bool {
        true
    }

    async fn on_deferred(&self, id: Json, method: &str, params: Json) {
        let reply = match self.mapped(|m| m.request(id.clone(), method, &params)) {
            Ok(reply) => reply,
            // Requests before the thread exists (config warnings) can't be served yet.
            Err(AdapterError::Closed) => Some(Reply::Error { id, code: -32601, message: "not ready".to_owned() }),
            Err(error) => Some(Reply::Error { id, code: -32602, message: error.to_string() }),
        };
        if let Some(Err(error)) = reply.map(|r| self.reply(r)) {
            tracing::warn!(%error, "codex: couldn't answer a request");
        }
    }

    async fn on_notification(&self, method: &str, params: Json) {
        // Notifications before the thread exists carry nothing for the session.
        if let Err(error) = self.mapped(|m| m.notification(method, &params))
            && !matches!(error, AdapterError::Closed)
        {
            tracing::warn!(%error, "codex: couldn't record an event");
        }
    }

    async fn on_closed(&self, _status: Option<ExitStatus>, stderr: &str) {
        if !stderr.is_empty() {
            tracing::debug!(target: "codex", "{stderr}");
        }
        if let Err(error) = self.mapped(|m| m.process_exited("codex exited")) {
            tracing::warn!(%error, "codex: couldn't record the exit");
        }
        self.state.send_replace(SessionState::Closed);
    }
}

/// A running Codex thread.
pub struct CodexSession {
    shared: Arc<Shared>,
    rpc: StdioRpc<JsonRpc2>,
    thread_id: String,
    settings: Mutex<TurnSettings>,
}

/// How a session attaches to its Codex thread.
pub(crate) enum Open<'a> {
    Start,
    Resume { thread_id: &'a str },
    Fork { thread_id: &'a str, last_turn_id: Option<&'a str> },
}

/// `[A-Za-z0-9_-]` only: Codex splits `-c` keys on dots and doesn't unquote.
fn config_key(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

/// A TOML inline table of string values. JSON string escapes are valid TOML.
fn toml_table(entries: &std::collections::BTreeMap<String, String>) -> String {
    let body: Vec<String> = entries.iter().map(|(k, v)| format!("{}={}", Json::from(k.as_str()), Json::from(v.as_str()))).collect();
    format!("{{{}}}", body.join(","))
}

/// The `-c key=value` overrides that hand the session's MCP servers to Codex.
pub fn mcp_overrides(config: &SessionConfig) -> Vec<String> {
    let mut args = Vec::new();
    for server in &config.mcp_servers {
        let key = format!("mcp_servers.{}", config_key(&server.name));
        match &server.transport {
            McpTransport::Stdio { command, args: command_args, env } => {
                args.push(format!("{key}.command={}", Json::from(command.as_str())));
                args.push(format!("{key}.args={}", Json::from(command_args.clone())));
                if !env.is_empty() {
                    args.push(format!("{key}.env={}", toml_table(env)));
                }
            }
            McpTransport::Http { url, headers } => {
                args.push(format!("{key}.url={}", Json::from(url.as_str())));
                if !headers.is_empty() {
                    args.push(format!("{key}.http_headers={}", toml_table(headers)));
                }
            }
        }
    }
    args.into_iter().flat_map(|a| ["-c".to_owned(), a]).collect()
}

/// The `input` list of `turn/start`.
fn turn_input(prompt: &Prompt) -> Json {
    let text = |text: String| json!({ "type": "text", "text": text, "text_elements": [] });
    let mut input = vec![text(prompt.text.clone())];
    for attachment in &prompt.attachments {
        let path = attachment.path.to_string_lossy();
        let mime = attachment.mime.as_deref().or_else(|| loa_util::text::mime_of(&attachment.path));
        input.push(match mime.is_some_and(|m| m.starts_with("image/")) {
            true => json!({ "type": "localImage", "path": path }),
            false => text(format!("Attached file: {path}")),
        });
    }
    Json::Array(input)
}

/// One `model/list` entry: `model` is what `turn/start` takes,
/// `supportedReasoningEfforts` the efforts it accepts. Hidden models are left
/// out.
fn listed_model(entry: &Json) -> Option<HarnessModel> {
    if entry.get("hidden").and_then(Json::as_bool) == Some(true) {
        return None;
    }
    let id = entry.get("model").or_else(|| entry.get("id")).and_then(Json::as_str)?.to_owned();
    let name = entry.get("displayName").and_then(Json::as_str).unwrap_or(&id).to_owned();
    let efforts = entry.get("supportedReasoningEfforts").and_then(Json::as_array).map_or_else(Vec::new, |levels| {
        levels.iter().filter_map(|level| level.get("reasoningEffort").unwrap_or(level).as_str().map(str::to_owned)).collect()
    });
    Some(HarnessModel { id, name, provider: "openai".to_owned(), efforts })
}

/// The `thread/start`, `thread/resume` or `thread/fork` request opening `open`.
fn open_request(config: &SessionConfig, open: &Open<'_>) -> (&'static str, Json) {
    let mut params = json!({ "cwd": config.cwd.to_string_lossy() });
    if let Some(model) = &config.model {
        params["model"] = model.as_str().into();
    }
    if let Some(context) = &config.context {
        params["developerInstructions"] = context.as_str().into();
    }
    let (method, thread_id, last_turn_id) = match open {
        Open::Start => ("thread/start", None, None),
        Open::Resume { thread_id } => ("thread/resume", Some(*thread_id), None),
        Open::Fork { thread_id, last_turn_id } => ("thread/fork", Some(*thread_id), *last_turn_id),
    };
    match thread_id {
        None => {
            params["approvalPolicy"] = "on-request".into();
            params["sandbox"] = "workspace-write".into();
            if !config.host_tools.is_empty() {
                let tools = config.host_tools.iter().map(|t| {
                    let schema = serde_json::from_str(&t.input_schema).unwrap_or_else(|_| json!({ "type": "object" }));
                    json!({ "type": "function", "name": t.name, "description": t.description, "inputSchema": schema })
                });
                params["dynamicTools"] = Json::Array(tools.collect());
            }
        }
        Some(thread_id) => {
            params["threadId"] = thread_id.into();
            params["excludeTurns"] = true.into();
            if let Some(turn) = last_turn_id {
                params["lastTurnId"] = turn.into();
            }
        }
    }
    (method, params)
}

impl CodexSession {
    pub(crate) async fn open(
        adapter: &CodexAdapter,
        config: SessionConfig,
        open: Open<'_>,
        attach: impl FnOnce(&str, Option<&str>) -> Result<Mapper>,
    ) -> Result<Self> {
        if !config.host_tools.is_empty() && config.host_tool_handler.is_none() {
            return Err(AdapterError::Protocol("host tools need a host tool handler".to_owned()));
        }
        let mut command = Command::new(config.executable.as_ref().unwrap_or(&adapter.executable));
        command
            .args(&adapter.args)
            .args(mcp_overrides(&config))
            .args(["app-server", "--listen", "stdio://"])
            .current_dir(&config.cwd)
            .envs(&config.env);
        let shared = Arc::new(Shared {
            mapper: Mutex::new(None),
            writer: OnceLock::new(),
            state: watch::Sender::new(SessionState::Idle),
            session_id: config.session_id.clone(),
            host_tools: config.host_tool_handler.clone(),
        });
        let rpc = StdioRpc::spawn(command, FRAMES, shared.clone())?;
        let _ = shared.writer.set(rpc.writer());
        let mut session = Self { shared, rpc, thread_id: String::new(), settings: Mutex::default() };
        let client = json!({
            "clientInfo": { "name": "loa", "title": "loa", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "experimentalApi": true },
        });
        session.request("initialize", client).await?;
        session.rpc.notify("initialized", Json::Null)?;
        let (method, params) = open_request(&config, &open);
        let opened = session.request(method, params).await?;
        session.thread_id = opened
            .pointer("/thread/id")
            .and_then(Json::as_str)
            .ok_or_else(|| AdapterError::Protocol(format!("{method} returned no thread id")))?
            .to_owned();
        let mapper = attach(&session.thread_id, opened.get("model").and_then(Json::as_str))?;
        session.settings = Mutex::new(TurnSettings {
            model: mapper.model().map(str::to_owned),
            effort: config.effort.clone(),
            mode: config.mode,
        });
        session.shared.state.send_replace(mapper.state());
        *session.shared.mapper.locked() = Some(mapper);
        Ok(session)
    }

    /// Codex's id for the thread this session drives.
    pub fn thread_id(&self) -> String {
        self.thread_id.clone()
    }

    async fn request(&self, method: &str, params: Json) -> Result<Json> {
        self.rpc.request_as(self.rpc.next_id().into(), method, params).await.map_err(|error| match error {
            RpcError::Command { message, .. } => AdapterError::Protocol(format!("{method}: {message}")),
            other => other.into(),
        })
    }

    /// The `turn/start` parameters for `prompt` under the current settings.
    fn turn_params(&self, prompt: &Prompt) -> Json {
        let mut params = json!({ "threadId": self.thread_id, "input": turn_input(prompt) });
        let settings = self.settings.locked();
        if let Some(model) = &settings.model {
            params["model"] = model.as_str().into();
        }
        if let Some(effort) = &settings.effort {
            params["effort"] = effort.as_str().into();
        }
        if let (Some(mode), Some(model)) = (settings.mode, &settings.model) {
            let kind = if mode == SessionMode::Plan { "plan" } else { "default" };
            params["collaborationMode"] = json!({
                "mode": kind,
                "settings": { "model": model, "reasoning_effort": settings.effort, "developer_instructions": null },
            });
        }
        params
    }
}

#[async_trait]
impl HarnessSession for CodexSession {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        if self.state() == SessionState::Closed {
            return Err(AdapterError::Closed);
        }
        self.shared.mapped(|m| m.begin_turn(&prompt))?;
        let outcome = self.request("turn/start", self.turn_params(&prompt)).await;
        self.shared.mapped(|m| match &outcome {
            Ok(result) => result.pointer("/turn/id").and_then(Json::as_str).map_or(Ok(()), |id| m.turn_accepted(id)),
            Err(error) => m.turn_rejected(&error.to_string()),
        })?;
        outcome.map(drop)
    }

    async fn interrupt(&self) -> Result<()> {
        let Some(turn_id) = self.shared.mapped(|m| Ok(m.turn_id().map(str::to_owned)))? else { return Ok(()) };
        self.request("turn/interrupt", json!({ "threadId": self.thread_id, "turnId": turn_id })).await.map(drop)
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        match self.shared.mapped(|m| m.answer(decision, &answer))? {
            Some(reply) => self.shared.reply(reply),
            None => Ok(()),
        }
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        if selection.role.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "model roles" });
        }
        self.settings.locked().model = Some(selection.model.clone());
        self.shared.mapped(|m| m.set_model(&selection.model))
    }

    async fn set_mode(&self, mode: SessionMode) -> Result<()> {
        self.settings.locked().mode = Some(mode);
        self.shared.mapped(|m| m.set_mode(mode))
    }

    /// Every page of `model/list`.
    async fn models(&self) -> Result<Vec<HarnessModel>> {
        let mut models = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor.as_ref().map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            let page = self.request("model/list", params).await?;
            let data = page
                .get("data")
                .and_then(Json::as_array)
                .ok_or_else(|| AdapterError::Protocol("model/list returned no data".to_owned()))?;
            models.extend(data.iter().filter_map(listed_model));
            match page.get("nextCursor").and_then(Json::as_str) {
                Some(next) if cursor.as_deref() != Some(next) => cursor = Some(next.to_owned()),
                _ => return Ok(models),
            }
        }
    }

    /// Codex takes the effort with each `turn/start`; it applies from the next turn.
    async fn set_effort(&self, effort: String) -> Result<()> {
        self.settings.locked().effort = Some(effort.clone());
        self.shared.mapped(|m| m.set_effort(&effort))
    }

    async fn compact(&self, instructions: Option<String>) -> Result<()> {
        if instructions.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "compaction instructions" });
        }
        self.request("thread/compact/start", json!({ "threadId": self.thread_id })).await.map(drop)
    }

    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn state(&self) -> SessionState {
        *self.shared.state.borrow()
    }

    async fn wait_idle(&self) -> Result<()> {
        let mut state = self.shared.state.subscribe();
        state.wait_for(|s| matches!(s, SessionState::Idle | SessionState::Closed)).await.map_err(|_| AdapterError::Closed)?;
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        // Closing stdin ends the app server; the kill covers one that hangs.
        self.rpc.shutdown(Duration::from_secs(5)).await;
        self.shared.state.send_replace(SessionState::Closed);
        Ok(())
    }
}
