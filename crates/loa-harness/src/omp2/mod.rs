//! omp2 adapter: omp's Rust rewrite (`omp` of the `omp2` branch) in RPC mode, one child per session. After
//! `set_dom_subscription` its stdout carries the session DOM as `patch@1` records, which [`Omp2Mapper`] maps onto
//! the app DOM; prompts, approvals and selects use the RPC v1 commands.

pub mod mapper;
pub mod wire;

pub use mapper::Omp2Mapper;

use crate::kit::rpc::id_key;
use crate::kit::{EventMapper, Framing, Inbound, Incoming, RpcError, StdioRpc, insert_root};
use async_trait::async_trait;
use base64::Engine as _;
use loa_contracts::harness::{
    Adapter, AdapterError, Answer, Availability, AvailabilityStatus, Capabilities, Feature, HarnessModel, HarnessSession,
    ModelSelection, ModelSwitch, Prompt, Result, SessionConfig, SessionState,
};
use loa_core::{DecisionKind, Dom, DomWriter, ElementKind, MemorySink, NodeId, PatchSink, SCHEMA_VERSION, SessionMode, TurnStatus, attr};
use loa_util::LockExt;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::watch;
use wire::{Input, ModelEntry, Reply};

pub const HARNESS: &str = "omp2";
/// The omp2 binary's name.
pub const EXECUTABLE: &str = "omp";
/// `set_thinking_level` levels, lowest first.
const THINKING_LEVELS: [&str; 6] = ["off", "minimal", "low", "medium", "high", "xhigh"];

/// RPC v1: commands are `{id, type, ...params}`, replies `{type: "response", id, success, data?, error?, code?}`;
/// every other line (frames and DOM records) is a notification.
struct Frames;

impl Framing for Frames {
    fn request(&self, id: &Json, method: &str, params: Json) -> Json {
        let mut frame = self.notification(method, params);
        frame["id"] = id.clone();
        frame
    }

    fn notification(&self, method: &str, params: Json) -> Json {
        let mut frame = if let Json::Object(map) = params { map } else { serde_json::Map::new() };
        frame.entry("type").or_insert_with(|| method.into());
        Json::Object(frame)
    }

    fn response(&self, _: Json, _: &str, result: Result<Json, RpcError>) -> Json {
        result.unwrap_or_default()
    }

    fn classify(&self, msg: Json) -> Option<Incoming> {
        if msg["type"] != "response" {
            return Some(Incoming::Notification { method: String::new(), params: msg });
        }
        let text = |key: &str| msg[key].as_str().unwrap_or_default().to_owned();
        let result = match msg["success"].as_bool() {
            Some(true) => Ok(msg.get("data").cloned().unwrap_or_default()),
            _ => Err(RpcError::Command { command: text("command"), message: text("error"), code: msg["code"].as_str().map(str::to_owned) }),
        };
        Some(Incoming::Response { id: id_key(&msg["id"]), result })
    }
}

struct Shared {
    mapper: Mutex<Omp2Mapper>,
    sink: Arc<dyn PatchSink>,
    state: watch::Sender<SessionState>,
}

impl Shared {
    fn with<T>(&self, f: impl FnOnce(&mut Omp2Mapper, &DomWriter) -> Result<T>) -> Result<T> {
        let mut mapper = self.mapper.locked();
        let result = f(&mut mapper, &DomWriter::new(&*self.sink));
        self.state.send_replace(mapper.ledger().state());
        result
    }

    /// Keeps a line the mapper couldn't use as `<ext>` under the session.
    fn keep(&self, msg: &Json, kind: &str, error: impl std::fmt::Display) -> Result<()> {
        let error = error.to_string();
        let attrs = [(attr::HARNESS, HARNESS), (attr::TYPE, kind), ("x-error", error.as_str())];
        self.with(|m, w| Ok(w.insert(m.ledger().session, ElementKind::Ext, attrs, msg.to_string()).map(drop)?))
    }
}

#[async_trait]
impl Inbound for Shared {
    async fn on_request(&self, method: &str, _: Json) -> Result<Json, RpcError> {
        Err(RpcError::Protocol(format!("omp2 sent an unexpected request `{method}`")))
    }

    async fn on_notification(&self, _: &str, msg: Json) {
        let result = match Input::deserialize(&msg) {
            Ok(input) => match self.with(|m, w| m.apply(input, w)) {
                Err(error @ AdapterError::Protocol(_)) => self.keep(&msg, "untranslated", error),
                other => other.map(drop),
            },
            Err(error) => self.keep(&msg, "unparsed", error),
        };
        if let Err(error) = result {
            tracing::error!(%error, "omp2 event handling failed");
        }
    }

    async fn on_closed(&self, _: Option<std::process::ExitStatus>, _: &str) {
        let _ = self.with(|m, _| {
            m.ledger_mut().close();
            Ok(())
        });
    }
}

/// One running omp2 session.
pub struct Omp2Session {
    shared: Arc<Shared>,
    rpc: StdioRpc<Frames>,
}

impl Omp2Session {
    async fn call(&self, method: &str, params: Json) -> Result<Json> {
        let reply = self.rpc.request_as(Json::from(self.rpc.next_id()), method, params).await;
        reply.map_err(|e| if e.code() == Some("session_busy") { AdapterError::Busy } else { e.into() })
    }

    fn set_root(&self, name: &str, value: &str) -> Result<()> {
        self.shared.with(|m, w| Ok(w.set(m.ledger().session, name, value)?))
    }
}

#[async_trait]
impl HarnessSession for Omp2Session {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        match self.state() {
            SessionState::Closed => return Err(AdapterError::Closed),
            SessionState::Idle => {}
            _ => return Err(AdapterError::Busy),
        }
        let (mut params, mut images) = (json!({ "message": prompt.text }), Vec::new());
        for attachment in &prompt.attachments {
            let mime = attachment.mime.as_deref().or_else(|| loa_util::text::mime_of(&attachment.path)).filter(|m| m.starts_with("image/"));
            let mime = mime.ok_or(AdapterError::Unsupported { harness: HARNESS, what: "attachments other than images" })?;
            let data = base64::engine::general_purpose::STANDARD.encode(tokio::fs::read(&attachment.path).await?);
            images.push(json!({ "data": data, "mimeType": mime }));
        }
        if !images.is_empty() {
            params["images"] = images.into();
        }
        self.shared.with(|m, w| m.begin(w, &prompt).map(drop))?;
        if let Err(error) = self.call("prompt", params).await {
            self.shared.with(|m, w| m.finish(w, TurnStatus::Error))?;
            return Err(error);
        }
        Ok(())
    }

    async fn interrupt(&self) -> Result<()> {
        if matches!(self.state(), SessionState::Running | SessionState::Waiting) {
            self.call("abort", json!({})).await?;
        }
        Ok(())
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        let Some(reply) = self.shared.with(|m, w| m.answer(decision, &answer, w))? else { return Ok(()) };
        let frame = serde_json::to_value(&reply).unwrap_or_default();
        match reply {
            Reply::Approve { .. } => self.call("approve", frame).await.map(drop),
            Reply::ExtensionUiResponse { .. } => Ok(self.rpc.send(&frame)?),
        }
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        if selection.role.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "setting a model role over RPC" });
        }
        let (provider, model) = selection.model.split_once('/').ok_or_else(|| AdapterError::Protocol(format!("omp2 models are `provider/model`, got `{}`", selection.model)))?;
        self.call("set_model", json!({ "provider": provider, "modelId": model })).await?;
        self.set_root(attr::MODEL, &selection.model)
    }

    /// Reasoning models take every thinking level.
    async fn models(&self) -> Result<Vec<HarnessModel>> {
        let data = self.call("get_available_models", json!({})).await?;
        let models = Vec::<ModelEntry>::deserialize(&data["models"]).map_err(|e| AdapterError::Protocol(format!("get_available_models: {e}")))?;
        let efforts = |reasoning: bool| if reasoning { THINKING_LEVELS.map(str::to_owned).to_vec() } else { Vec::new() };
        Ok(models.into_iter().map(|m| HarnessModel { id: format!("{}/{}", m.provider, m.id), name: if m.name.is_empty() { m.id } else { m.name }, efforts: efforts(m.reasoning), provider: m.provider }).collect())
    }

    async fn set_effort(&self, effort: String) -> Result<()> {
        self.call("set_thinking_level", json!({ "level": effort })).await?;
        self.set_root(attr::EFFORT, &effort)
    }

    async fn compact(&self, instructions: Option<String>) -> Result<()> {
        self.call("compact", instructions.map_or_else(|| json!({}), |text| json!({ "customInstructions": text }))).await.map(drop)
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

    /// `quit` lets omp2 flush its journal; closing stdin ends it either way.
    async fn shutdown(&self) -> Result<()> {
        let _ = self.rpc.send(&json!({ "type": "quit", "id": "loa-quit" }));
        self.rpc.shutdown(Duration::from_secs(5)).await;
        self.shared.with(|m, _| {
            m.ledger_mut().close();
            Ok(())
        })
    }
}

/// Starts omp2 and maps its DOM onto the app's.
pub struct Omp2Adapter {
    /// Run when `SessionConfig::executable` is unset.
    executable: PathBuf,
}

impl Omp2Adapter {
    pub fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self { executable: executable.into() }
    }

    fn command(&self, config: &SessionConfig, resume: Option<&str>) -> Result<Command> {
        let refuse = |what| Err(AdapterError::Unsupported { harness: HARNESS, what });
        if !config.mcp_servers.is_empty() || !config.host_tools.is_empty() || !config.disabled_tools.is_empty() {
            return refuse("configuring tools over omp2 RPC");
        }
        if config.context.is_some() {
            return refuse("injecting project context over omp2 RPC");
        }
        if config.model_role.is_some() || config.mode.is_some_and(|mode| mode != SessionMode::Normal) {
            return refuse("model roles or mode over omp2 RPC");
        }
        let mut command = Command::new(loa_util::exec::resolve(config.executable.as_ref().unwrap_or(&self.executable)));
        command.args(["--mode", "rpc"]).current_dir(&config.cwd).envs(&config.env);
        for (flag, value) in [("--resume", resume), ("--model", config.model.as_deref()), ("--thinking", config.effort.as_deref())] {
            command.args(value.map(|value| [flag, value]).into_iter().flatten());
        }
        Ok(command)
    }

    /// Spawns omp2 and subscribes to its DOM; returns the session and omp2's own session id.
    async fn launch(&self, config: &SessionConfig, sink: Arc<dyn PatchSink>, mapper: Omp2Mapper, resume: Option<&str>) -> Result<(Omp2Session, Option<String>)> {
        let shared = Arc::new(Shared { state: watch::Sender::new(mapper.ledger().state()), mapper: Mutex::new(mapper), sink });
        let session = Omp2Session { rpc: StdioRpc::spawn(self.command(config, resume)?, Frames, shared.clone())?, shared };
        let setup = async {
            // An `omp` without the DOM subscription refuses it.
            let subscribed = session.rpc.request_as(Json::from(session.rpc.next_id()), "set_dom_subscription", json!({ "enabled": true })).await;
            match subscribed {
                Err(RpcError::Command { message, .. }) => {
                    let detail = if message.is_empty() { "`set_dom_subscription` was refused".to_owned() } else { message };
                    return Err(AdapterError::Version { harness: HARNESS, detail });
                }
                other => other?,
            };
            Ok(session.call("get_state", json!({})).await?["sessionId"].as_str().map(str::to_owned))
        };
        match setup.await {
            Ok(native) => Ok((session, native)),
            Err(error) => {
                let _ = session.shutdown().await;
                Err(error)
            }
        }
    }
}

#[async_trait]
impl Adapter for Omp2Adapter {
    fn harness(&self) -> &'static str {
        HARNESS
    }

    /// Ready only when the executable accepts the DOM subscription; an `omp` that isn't omp2 is incompatible.
    async fn availability(&self) -> Availability {
        let found = Availability::probe(&self.executable, &["--version"]).await;
        let Some(executable) = found.executable.clone().filter(|_| found.is_ready()) else { return found };
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let config = SessionConfig { cwd, executable: Some(executable.clone()), session_id: "availability-probe".into(), ..SessionConfig::default() };
        let probe = async {
            let sink = Arc::new(MemorySink::new());
            let root = insert_root(&DomWriter::new(&*sink), HARNESS, &config)?;
            self.launch(&config, sink, Omp2Mapper::new(root), None).await?.0.shutdown().await
        };
        let shown = executable.display();
        match tokio::time::timeout(Availability::PROBE_TIMEOUT, probe).await {
            Ok(Ok(())) => found,
            Ok(Err(AdapterError::Version { detail, .. })) => Availability {
                executable: Some(executable.clone()),
                version: found.version.clone(),
                ..Availability::unavailable(AvailabilityStatus::Incompatible, format!("{shown} doesn't stream omp2's DOM: {detail}"))
            },
            Ok(Err(error)) => Availability::unavailable(AvailabilityStatus::Failed, format!("{shown}: {error}")),
            Err(_) => Availability::unavailable(AvailabilityStatus::Failed, format!("{shown} didn't answer in RPC mode")),
        }
    }

    fn capabilities(&self) -> Capabilities {
        let features = [Feature::Compaction, Feature::Subagents, Feature::OmpTools, Feature::AdvisorRules];
        let builder = Capabilities::builder().schema(SCHEMA_VERSION).features(&features).model_switch(ModelSwitch::PerTurn);
        builder.decisions(&[DecisionKind::Approval, DecisionKind::Question]).effort().build()
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        self.command(&config, None)?; // refuse before writing the app DOM
        let w = DomWriter::new(&*sink);
        let root = insert_root(&w, HARNESS, &config)?;
        let (session, native) = self.launch(&config, sink.clone(), Omp2Mapper::new(root), None).await?;
        if let Some(native) = native {
            w.set(root, attr::NATIVE_ID, native)?;
        }
        Ok(Box::new(session))
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let native = dom.root().and_then(|root| root.str_attr(attr::NATIVE_ID)).ok_or_else(|| AdapterError::Format("missing omp2 native session id".into()))?;
        Ok(Box::new(self.launch(&config, sink, Omp2Mapper::from_dom(dom), Some(native)).await?.0))
    }
}
