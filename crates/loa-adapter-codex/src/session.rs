//! The Codex process driver: `codex app-server` over stdio JSON-RPC.
//!
//! One process per session. The reader task feeds every notification and
//! server request to the [`Mapper`], writes the replies, and resolves the
//! responses to the requests this side sent.

use crate::mapper::{Mapper, Reply, host_tool_result};
use crate::{CodexAdapter, HARNESS};
use async_trait::async_trait;
use loa_adapter::{
    AdapterError, Answer, Attachment, HarnessSession, HostTools, McpTransport, ModelSelection, Prompt, Result,
    SessionConfig, SessionState,
};
use loa_core::{NodeId, SessionMode};
use serde_json::{Value as Json, json};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, watch};

type Responder = oneshot::Sender<std::result::Result<Json, String>>;

/// Settings applied to every `turn/start`.
#[derive(Default)]
struct TurnSettings {
    model: Option<String>,
    effort: Option<String>,
    mode: Option<SessionMode>,
}

struct Shared {
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    pending: Mutex<HashMap<i64, Responder>>,
    next_id: AtomicI64,
    mapper: Mutex<Mapper>,
    state: watch::Sender<SessionState>,
    thread_id: Mutex<String>,
    settings: Mutex<TurnSettings>,
    session_id: String,
    host_tools: Option<Arc<dyn HostTools>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Every critical section here is a plain field update or a mapper call
    // that commits drafts; a panic in one leaves no half-written invariant.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn publish_state(&self) {
        let state = lock(&self.mapper).state();
        self.state.send_replace(state);
    }

    async fn write(&self, message: &Json) -> Result<()> {
        let mut line = message.to_string();
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        let pipe = stdin.as_mut().ok_or(AdapterError::Closed)?;
        pipe.write_all(line.as_bytes()).await?;
        pipe.flush().await?;
        Ok(())
    }

    async fn request(&self, method: &str, params: Json) -> Result<Json> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        if let Err(error) = self.write(&json!({ "id": id, "method": method, "params": params })).await {
            lock(&self.pending).remove(&id);
            return Err(error);
        }
        match rx.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(message)) => Err(AdapterError::Protocol(format!("{method}: {message}"))),
            Err(_) => Err(AdapterError::Closed),
        }
    }

    async fn send_reply(self: &Arc<Self>, reply: Reply) -> Result<()> {
        match reply {
            Reply::Result { id, result } => self.write(&json!({ "id": id, "result": result })).await,
            Reply::Error { id, code, message } => {
                self.write(&json!({ "id": id, "error": { "code": code, "message": message } })).await
            }
            Reply::HostTool { id, tool, arguments } => {
                let Some(handler) = self.host_tools.clone() else {
                    let result = host_tool_result(Err(format!("no handler for app tool `{tool}`")));
                    return self.write(&json!({ "id": id, "result": result })).await;
                };
                let shared = Arc::clone(self);
                tokio::spawn(async move {
                    let outcome = handler.call(&shared.session_id, &tool, &arguments).await;
                    let message = json!({ "id": id, "result": host_tool_result(outcome) });
                    if let Err(error) = shared.write(&message).await {
                        tracing::warn!(%error, "codex: couldn't return an app tool result");
                    }
                });
                Ok(())
            }
        }
    }

    /// Handles one line from Codex's stdout.
    async fn incoming(self: &Arc<Self>, line: &str) {
        let message: Json = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                tracing::debug!(%error, "codex: skipping a non-JSON line");
                return;
            }
        };
        let method = message.get("method").and_then(Json::as_str);
        let id = message.get("id").filter(|id| !id.is_null());
        let params = message.get("params").unwrap_or(&Json::Null);
        let outcome = match (method, id) {
            (Some(method), Some(id)) => {
                let reply = lock(&self.mapper).request(id.clone(), method, params);
                match reply {
                    Ok(Some(reply)) => self.send_reply(reply).await,
                    Ok(None) => Ok(()),
                    Err(error) => {
                        let response = json!({
                            "id": id,
                            "error": { "code": -32602, "message": error.to_string() }
                        });
                        self.write(&response).await
                    }
                }
            }
            (Some(method), None) => lock(&self.mapper).notification(method, params),
            (None, Some(id)) => {
                let responder = id.as_i64().and_then(|id| lock(&self.pending).remove(&id));
                if let Some(responder) = responder {
                    let outcome = match message.get("error") {
                        Some(error) => Err(error.get("message").and_then(Json::as_str).unwrap_or("error").to_owned()),
                        None => Ok(message.get("result").cloned().unwrap_or(Json::Null)),
                    };
                    // The requester may have gone away; nothing waits for the result then.
                    let _ = responder.send(outcome);
                }
                Ok(())
            }
            (None, None) => Ok(()),
        };
        if let Err(error) = outcome {
            tracing::warn!(%error, "codex: couldn't record an event");
        }
        self.publish_state();
    }

    /// Codex's stdout closed.
    fn exited(&self, detail: &str) {
        for (_, responder) in lock(&self.pending).drain() {
            // Nothing waits when the requester was dropped.
            let _ = responder.send(Err(detail.to_owned()));
        }
        if let Err(error) = lock(&self.mapper).process_exited(detail) {
            tracing::warn!(%error, "codex: couldn't record the exit");
        }
        self.state.send_replace(SessionState::Closed);
    }

    fn thread_id(&self) -> String {
        lock(&self.thread_id).clone()
    }
}

/// A running Codex thread.
pub struct CodexSession {
    shared: Arc<Shared>,
    child: tokio::sync::Mutex<Child>,
    state: watch::Receiver<SessionState>,
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

fn is_image(attachment: &Attachment) -> bool {
    if let Some(mime) = &attachment.mime {
        return mime.starts_with("image/");
    }
    let extension = attachment.path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    matches!(extension.as_deref(), Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"))
}

/// The `input` list of `turn/start`.
fn turn_input(prompt: &Prompt) -> Json {
    let mut input = vec![json!({ "type": "text", "text": prompt.text, "text_elements": [] })];
    for attachment in &prompt.attachments {
        let path = attachment.path.to_string_lossy();
        if is_image(attachment) {
            input.push(json!({ "type": "localImage", "path": path }));
        } else {
            input.push(json!({ "type": "text", "text": format!("Attached file: {path}"), "text_elements": [] }));
        }
    }
    Json::Array(input)
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
        let executable = config.executable.clone().unwrap_or_else(|| adapter.executable.clone());
        let mut command = Command::new(&executable);
        command
            .args(&adapter.args)
            .args(mcp_overrides(&config))
            .args(["app-server", "--listen", "stdio://"])
            .current_dir(&config.cwd)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => AdapterError::NotInstalled(executable.to_string_lossy().into_owned()),
            _ => AdapterError::Io(error),
        })?;
        let stdin = child.stdin.take().ok_or(AdapterError::Closed)?;
        let stdout = child.stdout.take().ok_or(AdapterError::Closed)?;
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "codex", "{line}");
                }
            });
        }

        let (pending_tx, mut pending_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if pending_tx.send(line).is_err() {
                            return;
                        }
                    }
                    Ok(None) | Err(_) => return,
                }
            }
        });

        // Handshake before the mapper exists: lines are answered directly.
        let mut handshake = Handshake { stdin, pending_rx: &mut pending_rx, next_id: 0 };
        handshake
            .request(
                "initialize",
                json!({
                    "clientInfo": { "name": "loa", "title": "loa", "version": env!("CARGO_PKG_VERSION") },
                    "capabilities": { "experimentalApi": true },
                }),
            )
            .await?;
        handshake.notify("initialized").await?;
        let cwd = config.cwd.to_string_lossy().into_owned();
        let mut params = json!({ "cwd": cwd });
        if let Some(object) = params.as_object_mut() {
            if let Some(model) = &config.model {
                object.insert("model".to_owned(), Json::from(model.as_str()));
            }
            if let Some(context) = &config.context {
                object.insert("developerInstructions".to_owned(), Json::from(context.as_str()));
            }
            if !config.host_tools.is_empty() {
                let tools: Vec<Json> = config
                    .host_tools
                    .iter()
                    .map(|t| {
                        let schema = serde_json::from_str(&t.input_schema).unwrap_or_else(|_| json!({ "type": "object" }));
                        json!({ "type": "function", "name": t.name, "description": t.description, "inputSchema": schema })
                    })
                    .collect();
                object.insert("dynamicTools".to_owned(), Json::Array(tools));
            }
        }
        let (method, params) = match open {
            Open::Start => {
                if let Some(object) = params.as_object_mut() {
                    object.insert("approvalPolicy".to_owned(), json!("on-request"));
                    object.insert("sandbox".to_owned(), json!("workspace-write"));
                }
                ("thread/start", params)
            }
            Open::Resume { thread_id } => {
                if let Some(object) = params.as_object_mut() {
                    object.insert("threadId".to_owned(), Json::from(thread_id));
                    object.insert("excludeTurns".to_owned(), Json::Bool(true));
                    object.remove("dynamicTools");
                }
                ("thread/resume", params)
            }
            Open::Fork { thread_id, last_turn_id } => {
                if let Some(object) = params.as_object_mut() {
                    object.insert("threadId".to_owned(), Json::from(thread_id));
                    object.insert("excludeTurns".to_owned(), Json::Bool(true));
                    object.remove("dynamicTools");
                    if let Some(turn) = last_turn_id {
                        object.insert("lastTurnId".to_owned(), Json::from(turn));
                    }
                }
                ("thread/fork", params)
            }
        };
        let opened = handshake.request(method, params).await?;
        let thread_id = opened
            .get("thread")
            .and_then(|t| t.get("id"))
            .and_then(Json::as_str)
            .ok_or_else(|| AdapterError::Protocol(format!("{method} returned no thread id")))?
            .to_owned();
        let model = opened.get("model").and_then(Json::as_str);
        let mapper = attach(&thread_id, model)?;
        let initial_state = mapper.state();
        let (state_tx, state_rx) = watch::channel(initial_state);
        let shared = Arc::new(Shared {
            stdin: tokio::sync::Mutex::new(Some(handshake.stdin)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(handshake.next_id),
            settings: Mutex::new(TurnSettings {
                model: mapper.model().map(str::to_owned),
                effort: config.effort.clone(),
                mode: config.mode,
            }),
            mapper: Mutex::new(mapper),
            state: state_tx,
            thread_id: Mutex::new(thread_id),
            session_id: config.session_id.clone(),
            host_tools: config.host_tool_handler.clone(),
        });
        let pump = Arc::clone(&shared);
        tokio::spawn(async move {
            while let Some(line) = pending_rx.recv().await {
                pump.incoming(&line).await;
            }
            // The reader ended: stdout closed.
            pump.exited("codex exited");
        });
        Ok(Self { shared, child: tokio::sync::Mutex::new(child), state: state_rx })
    }

    /// Codex's id for the thread this session drives.
    pub fn thread_id(&self) -> String {
        self.shared.thread_id()
    }

    fn mapper(&self) -> MutexGuard<'_, Mapper> {
        lock(&self.shared.mapper)
    }
}

/// The request/response exchange that runs before the reader task owns stdout.
struct Handshake<'a> {
    stdin: ChildStdin,
    pending_rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<String>,
    next_id: i64,
}

impl Handshake<'_> {
    async fn write(&mut self, message: &Json) -> Result<()> {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn notify(&mut self, method: &str) -> Result<()> {
        self.write(&json!({ "method": method })).await
    }

    async fn request(&mut self, method: &str, params: Json) -> Result<Json> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({ "id": id, "method": method, "params": params })).await?;
        while let Some(line) = self.pending_rx.recv().await {
            let Ok(message) = serde_json::from_str::<Json>(&line) else { continue };
            if message.get("method").is_some() {
                // Notifications before the thread exists (config warnings,
                // account updates) carry nothing for the session.
                if let Some(request_id) = message.get("id") {
                    let reply = json!({ "id": request_id, "error": { "code": -32601, "message": "not ready" } });
                    self.write(&reply).await?;
                }
                continue;
            }
            if message.get("id").and_then(Json::as_i64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let text = error.get("message").and_then(Json::as_str).unwrap_or("error");
                return Err(AdapterError::Protocol(format!("{method}: {text}")));
            }
            return Ok(message.get("result").cloned().unwrap_or(Json::Null));
        }
        Err(AdapterError::Protocol(format!("codex exited during {method}")))
    }
}

#[async_trait]
impl HarnessSession for CodexSession {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        if *self.state.borrow() == SessionState::Closed {
            return Err(AdapterError::Closed);
        }
        self.mapper().begin_turn(&prompt)?;
        self.shared.publish_state();
        let mut params = json!({ "threadId": self.shared.thread_id(), "input": turn_input(&prompt) });
        {
            let settings = lock(&self.shared.settings);
            if let Some(object) = params.as_object_mut() {
                if let Some(model) = &settings.model {
                    object.insert("model".to_owned(), Json::from(model.as_str()));
                }
                if let Some(effort) = &settings.effort {
                    object.insert("effort".to_owned(), Json::from(effort.as_str()));
                }
                if let (Some(mode), Some(model)) = (settings.mode, &settings.model) {
                    let kind = if mode == SessionMode::Plan { "plan" } else { "default" };
                    object.insert(
                        "collaborationMode".to_owned(),
                        json!({
                            "mode": kind,
                            "settings": { "model": model, "reasoning_effort": settings.effort, "developer_instructions": null },
                        }),
                    );
                }
            }
        }
        let outcome = self.shared.request("turn/start", params).await;
        let recorded = match &outcome {
            Ok(result) => match result.get("turn").and_then(|t| t.get("id")).and_then(Json::as_str) {
                Some(turn_id) => self.mapper().turn_accepted(turn_id),
                None => Ok(()),
            },
            Err(error) => self.mapper().turn_rejected(&error.to_string()),
        };
        self.shared.publish_state();
        recorded?;
        outcome.map(|_| ())
    }

    async fn interrupt(&self) -> Result<()> {
        let Some(turn_id) = self.mapper().turn_id().map(str::to_owned) else { return Ok(()) };
        self.shared.request("turn/interrupt", json!({ "threadId": self.shared.thread_id(), "turnId": turn_id })).await?;
        Ok(())
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        let reply = self.mapper().answer(decision, &answer)?;
        self.shared.publish_state();
        if let Some(reply) = reply {
            self.shared.send_reply(reply).await?;
        }
        Ok(())
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        if selection.role.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "model roles" });
        }
        lock(&self.shared.settings).model = Some(selection.model.clone());
        self.mapper().set_model(&selection.model)
    }

    async fn set_mode(&self, mode: SessionMode) -> Result<()> {
        lock(&self.shared.settings).mode = Some(mode);
        self.mapper().set_mode(mode)
    }

    async fn compact(&self, instructions: Option<String>) -> Result<()> {
        if instructions.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "compaction instructions" });
        }
        self.shared.request("thread/compact/start", json!({ "threadId": self.shared.thread_id() })).await?;
        Ok(())
    }

    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn state(&self) -> SessionState {
        *self.state.borrow()
    }

    async fn wait_idle(&self) -> Result<()> {
        let mut state = self.state.clone();
        state
            .wait_for(|s| matches!(s, SessionState::Idle | SessionState::Closed))
            .await
            .map_err(|_| AdapterError::Closed)?;
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        // Closing stdin ends the app server; the kill covers one that hangs.
        self.shared.stdin.lock().await.take();
        let mut child = self.child.lock().await;
        if child.try_wait()?.is_none() {
            child.kill().await?;
        }
        child.wait().await?;
        self.shared.state.send_replace(SessionState::Closed);
        Ok(())
    }
}
