//! `codex app-server` on [`StdioRpc`]: notifications and server requests reach the [`Mapper`] in read order.

use super::{CodexAdapter, HARNESS};
use super::mapper::{Mapper, Reply};
use crate::kit::rpc::Writer;
use crate::kit::{Framing, Inbound, JsonRpc2, RpcError, StdioRpc};
use async_trait::async_trait;
use loa_contracts::harness::{
    AdapterError, Answer, HarnessModel, HarnessSession, HostTools, McpTransport, ModelSelection, Prompt, Result,
    SessionConfig, SessionState,
};
use loa_core::{DomWriter, NodeId, PatchSink, SessionMode};
use loa_util::LockExt;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
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
    sink: Arc<dyn PatchSink>,
    writer: OnceLock<Writer<JsonRpc2>>,
    state: watch::Sender<SessionState>,
    session_id: String,
    host_tools: Option<Arc<dyn HostTools>>,
}

fn tool_result(outcome: std::result::Result<String, String>) -> Json {
    let (text, success) = outcome.map_or_else(|text| (text, false), |text| (text, true));
    json!({ "contentItems": [{ "type": "inputText", "text": text }], "success": success })
}

impl Shared {
    /// Runs `f` on the mapper and publishes the resulting state.
    fn mapped<T>(&self, f: impl FnOnce(&mut Mapper, &DomWriter) -> Result<T>) -> Result<T> {
        let mut mapper = self.mapper.locked();
        let mapper = mapper.as_mut().ok_or(AdapterError::Closed)?;
        let out = f(mapper, &DomWriter::new(&*self.sink));
        if *self.state.borrow() != SessionState::Closed {
            self.state.send_replace(mapper.state());
        }
        out
    }

    fn respond(&self, id: Json, result: std::result::Result<Json, RpcError>) -> Result<()> {
        Ok(self.writer.get().ok_or(AdapterError::Closed)?.send(&FRAMES.response(id, "", result))?)
    }

    fn reply(&self, reply: Reply) -> Result<()> {
        match reply {
            Reply::Result { id, result } => self.respond(id, Ok(result)),
            Reply::Error { id, code, message } => {
                self.respond(id, Err(RpcError::Command { command: String::new(), message, code: Some(code.to_string()) }))
            }
            Reply::HostTool { id, tool, arguments } => {
                let (Some(handler), Some(writer)) = (self.host_tools.clone(), self.writer.get().cloned()) else {
                    return self.respond(id, Ok(tool_result(Err(format!("no handler for app tool `{tool}`")))));
                };
                let session = self.session_id.clone();
                tokio::spawn(async move {
                    let outcome = handler.call(&session, &tool, &arguments).await;
                    if let Err(error) = writer.send(&FRAMES.response(id, "", Ok(tool_result(outcome)))) {
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
        let reply = match self.mapped(|m, w| m.handle(Some(id.clone()), method, &params, w)) {
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
        match self.mapped(|m, w| m.handle(None, method, &params, w)) {
            Ok(_) | Err(AdapterError::Closed) => {}
            Err(error) => tracing::warn!(%error, "codex: couldn't record an event"),
        }
    }

    async fn on_closed(&self, _status: Option<ExitStatus>, stderr: &str) {
        if !stderr.is_empty() {
            tracing::debug!(target: "codex", "{stderr}");
        }
        if let Err(error) = self.mapped(|m, w| m.process_exited("codex exited", w)) {
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

/// A TOML inline table of string values; JSON string escapes are valid TOML.
fn toml_table(entries: &BTreeMap<String, String>) -> String {
    let body: Vec<String> = entries.iter().map(|(k, v)| format!("{}={}", Json::from(k.as_str()), Json::from(v.as_str()))).collect();
    format!("{{{}}}", body.join(","))
}

/// The `-c key=value` overrides that hand the session's MCP servers to Codex.
fn mcp_overrides(config: &SessionConfig) -> Vec<String> {
    let mut args = Vec::new();
    for server in &config.mcp_servers {
        // Codex splits `-c` keys on dots and doesn't unquote.
        let name: String = server.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
        let key = format!("mcp_servers.{name}");
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

/// One `model/list` entry; hidden models are left out.
fn listed_model(entry: &Json) -> Option<HarnessModel> {
    if entry.get("hidden").and_then(Json::as_bool) == Some(true) {
        return None;
    }
    let id = entry.get("model").or_else(|| entry.get("id")).and_then(Json::as_str)?.to_owned();
    let name = entry.get("displayName").and_then(Json::as_str).unwrap_or(&id).to_owned();
    let levels = entry.get("supportedReasoningEfforts").and_then(Json::as_array).into_iter().flatten();
    let efforts = levels.filter_map(|level| level.get("reasoningEffort").unwrap_or(level).as_str().map(str::to_owned)).collect();
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
    let (method, thread_id, last_turn_id) = match *open {
        Open::Start => {
            params["approvalPolicy"] = "on-request".into();
            params["sandbox"] = "workspace-write".into();
            if !config.host_tools.is_empty() {
                let tools = config.host_tools.iter().map(|t| {
                    let schema = serde_json::from_str(&t.input_schema).unwrap_or_else(|_| json!({ "type": "object" }));
                    json!({ "type": "function", "name": t.name, "description": t.description, "inputSchema": schema })
                });
                params["dynamicTools"] = Json::Array(tools.collect());
            }
            return ("thread/start", params);
        }
        Open::Resume { thread_id } => ("thread/resume", thread_id, None),
        Open::Fork { thread_id, last_turn_id } => ("thread/fork", thread_id, last_turn_id),
    };
    params["threadId"] = thread_id.into();
    params["excludeTurns"] = true.into();
    if let Some(turn) = last_turn_id {
        params["lastTurnId"] = turn.into();
    }
    (method, params)
}

impl CodexSession {
    pub(crate) async fn open(
        adapter: &CodexAdapter,
        config: SessionConfig,
        sink: Arc<dyn PatchSink>,
        open: Open<'_>,
        attach: impl FnOnce(&str, Option<&str>, &DomWriter) -> Result<Mapper>,
    ) -> Result<Self> {
        if !config.host_tools.is_empty() && config.host_tool_handler.is_none() {
            return Err(AdapterError::Protocol("host tools need a host tool handler".to_owned()));
        }
        let mut command = Command::new(config.executable.as_deref().unwrap_or(&adapter.executable));
        command.args(&adapter.args).args(mcp_overrides(&config)).args(["app-server", "--listen", "stdio://"]).current_dir(&config.cwd).envs(&config.env);
        let shared = Arc::new(Shared {
            mapper: Mutex::new(None),
            sink,
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
        let thread = opened.pointer("/thread/id").and_then(Json::as_str);
        session.thread_id = thread.ok_or_else(|| AdapterError::Protocol(format!("{method} returned no thread id")))?.to_owned();
        let mapper = attach(&session.thread_id, opened.get("model").and_then(Json::as_str), &DomWriter::new(&*session.shared.sink))?;
        let model = mapper.model().map(str::to_owned);
        session.settings = Mutex::new(TurnSettings { model, effort: config.effort, mode: config.mode });
        session.shared.state.send_replace(mapper.state());
        *session.shared.mapper.locked() = Some(mapper);
        Ok(session)
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
            params["collaborationMode"] = json!({
                "mode": if mode == SessionMode::Plan { "plan" } else { "default" },
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
        self.shared.mapped(|m, w| m.begin_turn(&prompt, w))?;
        let outcome = self.request("turn/start", self.turn_params(&prompt)).await;
        self.shared.mapped(|m, w| match &outcome {
            Ok(result) => result.pointer("/turn/id").and_then(Json::as_str).map_or(Ok(()), |id| m.turn_accepted(id, w)),
            Err(error) => m.turn_rejected(&error.to_string(), w),
        })?;
        outcome.map(drop)
    }

    async fn interrupt(&self) -> Result<()> {
        let Some(turn_id) = self.shared.mapped(|m, _| Ok(m.turn_id().map(str::to_owned)))? else { return Ok(()) };
        self.request("turn/interrupt", json!({ "threadId": self.thread_id, "turnId": turn_id })).await.map(drop)
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        match self.shared.mapped(|m, w| m.answer(decision, &answer, w))? {
            Some(reply) => self.shared.reply(reply),
            None => Ok(()),
        }
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        if selection.role.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "model roles" });
        }
        self.settings.locked().model = Some(selection.model.clone());
        self.shared.mapped(|m, w| m.set_model(&selection.model, w))
    }

    async fn set_mode(&self, mode: SessionMode) -> Result<()> {
        self.settings.locked().mode = Some(mode);
        self.shared.mapped(|m, w| m.set_mode(mode, w))
    }

    /// Every page of `model/list`.
    async fn models(&self) -> Result<Vec<HarnessModel>> {
        let mut models = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = self.request("model/list", cursor.as_ref().map_or_else(|| json!({}), |c| json!({ "cursor": c }))).await?;
            let data = page.get("data").and_then(Json::as_array);
            models.extend(data.ok_or_else(|| AdapterError::Protocol("model/list returned no data".to_owned()))?.iter().filter_map(listed_model));
            match page.get("nextCursor").and_then(Json::as_str) {
                Some(next) if cursor.as_deref() != Some(next) => cursor = Some(next.to_owned()),
                _ => return Ok(models),
            }
        }
    }

    /// Codex takes the effort with each `turn/start`; it applies from the next turn.
    async fn set_effort(&self, effort: String) -> Result<()> {
        self.settings.locked().effort = Some(effort.clone());
        self.shared.mapped(|m, w| m.set_effort(&effort, w))
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
