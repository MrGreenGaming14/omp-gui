//! An attached OpenCode server session and its event-stream worker.

use loa_util::LockExt;
use super::client::{Client, EventStream, ServerAuth};
use super::ids;
use super::mapper::{ChildSessions, EventMapper, Reply, RootInfo};
use super::native;
use async_trait::async_trait;
use loa_contracts::harness::{Adapter, AdapterError, Answer, Availability, AvailabilityStatus, Capabilities, ContextDelivery, Feature, HarnessModel, HarnessSession, ModelSelection, ModelSwitch, Prompt, Result, SessionConfig, SessionState};
use loa_core::{DecisionKind, Dom, NodeId, PatchSink, SCHEMA_VERSION, SessionMode, attr, now_ms};
use serde_json::{Value as Json, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// How long [`Adapter::availability`] waits for the server.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Connects to a caller-owned `opencode serve` instance; never reads credentials
/// from the process environment or launches the CLI.
#[derive(Clone)]
pub struct OpenCodeAdapter {
    base: String,
    auth: Option<ServerAuth>,
    children: Option<Arc<dyn ChildSessions>>,
}

impl OpenCodeAdapter {
    /// `base` is the server URL; `auth` is explicit HTTP basic authentication.
    pub fn new(base: impl Into<String>, auth: Option<ServerAuth>) -> Self {
        Self { base: base.into(), auth, children: None }
    }

    /// Installs the app's child-session store for task subagents.
    pub fn with_child_sessions(mut self, children: Arc<dyn ChildSessions>) -> Self {
        self.children = Some(children);
        self
    }

    fn client(&self, config: &SessionConfig) -> Client {
        Client::new(&self.base, &config.cwd.to_string_lossy(), self.auth.clone())
    }

    async fn attach(&self, client: Client, sink: Arc<dyn PatchSink>, mapper: EventMapper, events: EventStream, config: SessionConfig) -> Result<Box<dyn HarnessSession>> {
        let inner = Arc::new(Inner {
            client,
            sink,
            mapper: Mutex::new(mapper.with_child_sessions(self.children.clone())),
            notify: Notify::new(),
            context: config.context,
            mode: Mutex::new(config.mode.unwrap_or(SessionMode::Normal)),
            closed: Mutex::new(false),
        });
        let worker = tokio::spawn(read_events(inner.clone(), events));
        Ok(Box::new(OpenCodeSession { inner, worker: Mutex::new(Some(worker)) }))
    }

    /// The server's base URL.
    pub fn base(&self) -> &str { &self.base }
}

#[async_trait]
impl Adapter for OpenCodeAdapter {
    fn harness(&self) -> &'static str { "opencode" }

    /// Probes the server: `GET /global/health` (its version), else
    /// `GET /config/providers` on servers without the health route.
    async fn availability(&self) -> Availability {
        let directory = std::env::current_dir().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
        let client = Client::new(&self.base, &directory, self.auth.clone());
        let unreachable = |error: reqwest::Error| {
            // A refused or silent port is the common case; reqwest's own text
            // for it only repeats the URL.
            let reason = if error.is_connect() || error.is_timeout() {
                format!("no OpenCode server at {}; start `opencode serve`", client.base())
            } else {
                format!("no OpenCode server at {} (start `opencode serve`): {error}", client.base())
            };
            Availability::unavailable(AvailabilityStatus::Unreachable, reason)
        };
        let ready = |version: Option<String>| Availability { detail: Some(version.clone().unwrap_or_else(|| client.base().to_owned())), ..Availability::ready(None, version) };
        match client.probe("/global/health", PROBE_TIMEOUT).await {
            Err(error) => unreachable(error),
            Ok((status, health)) if status.is_success() => ready(health.get("version").and_then(Json::as_str).map(str::to_owned)),
            Ok(_) => match client.probe("/config/providers", PROBE_TIMEOUT).await {
                Err(error) => unreachable(error),
                Ok((status, _)) if status.is_success() => ready(None),
                Ok((status, _)) => Availability::unavailable(
                    AvailabilityStatus::Incompatible,
                    format!("the server at {} answered HTTP {status}", client.base()),
                ),
            },
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: SCHEMA_VERSION,
            features: vec![Feature::Fork, Feature::Rewind, Feature::Compaction, Feature::Subagents, Feature::PlanMode],
            model_switch: ModelSwitch::PerTurn,
            decisions: vec![DecisionKind::Approval, DecisionKind::Question],
            context_delivery: vec![ContextDelivery::SystemPrompt],
            host_tools: false,
            mcp_servers: true,
            queue_mid_turn: false,
            effort: false,
            import: true,
            export: true,
        }
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        if !config.host_tools.is_empty() || !config.disabled_tools.is_empty() {
            return Err(AdapterError::Unsupported { harness: "opencode", what: "host tools or disabled tools" });
        }
        let client = self.client(&config);
        let events = client.events().await?;
        let native = client.create_session(json!({})).await?;
        for server in &config.mcp_servers {
            let transport = match &server.transport {
                loa_contracts::harness::McpTransport::Http { url, headers } => json!({ "type": "remote", "url": url, "headers": headers }),
                loa_contracts::harness::McpTransport::Stdio { command, args, env } => json!({ "type": "local", "command": std::iter::once(command.clone()).chain(args.iter().cloned()).collect::<Vec<_>>(), "environment": env }),
            };
            client.add_mcp(&server.name, transport).await?;
        }
        let mapper = EventMapper::start(&*sink, RootInfo {
            session_id: config.session_id.clone(), node: config.node.clone(),
            cwd: Some(config.cwd.to_string_lossy().into_owned()), project: config.project.clone(),
            model: config.model.clone(), model_role: config.model_role.clone(),
            effort: config.effort.clone(), mode: config.mode, native,
            ..RootInfo::default()
        }, now_ms())?;
        self.attach(client, sink, mapper, events, config).await
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let client = self.client(&config);
        let events = client.events().await?;
        let mut mapper = EventMapper::from_dom(dom)?;
        let native_id = mapper.native_id().to_owned();
        if native_id.is_empty() { return Err(AdapterError::Format("session has no OpenCode native_id".into())); }
        client.session(&native_id).await?;
        mapper.close_stale(&*sink, now_ms())?;
        self.attach(client, sink, mapper, events, config).await
    }

    async fn fork(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let client = self.client(&config);
        let events = client.events().await?;
        let mut mapper = EventMapper::from_dom(dom)?;
        let source = mapper.native_id().to_owned();
        if source.is_empty() { return Err(AdapterError::Format("fork has no OpenCode native_id".into())); }
        let native = client.fork(&source, None).await?;
        let id = native.get("id").and_then(Json::as_str).ok_or_else(|| AdapterError::Protocol("fork response has no id".into()))?;
        mapper.set_native_id(&*sink, id, now_ms())?;
        mapper.close_stale(&*sink, now_ms())?;
        self.attach(client, sink, mapper, events, config).await
    }

    fn import(&self, path: &Path, sink: &dyn PatchSink) -> Result<()> { native::import(path, sink) }
    fn export(&self, dom: &Dom, path: &Path) -> Result<()> { native::export(dom, path) }
}

struct Inner {
    client: Client,
    sink: Arc<dyn PatchSink>,
    mapper: Mutex<EventMapper>,
    notify: Notify,
    context: Option<String>,
    mode: Mutex<SessionMode>,
    closed: Mutex<bool>,
}

impl Inner {
    fn closed(&self) -> bool {
        *self.closed.locked()
    }
    fn id(&self) -> String { self.mapper.locked().native_id().to_owned() }
}

/// The models of `GET /config/providers`, as `provider/model` ids.
pub fn provider_models(providers: &Json) -> Vec<HarnessModel> {
    let Some(list) = providers.get("providers").and_then(Json::as_array) else { return Vec::new() };
    list.iter()
        .filter_map(|provider| Some((provider.get("id")?.as_str()?, provider.get("models")?.as_object()?)))
        .flat_map(|(provider, models)| {
            models.iter().map(move |(key, model)| {
                let id = model.get("id").and_then(Json::as_str).unwrap_or(key);
                HarnessModel {
                    id: format!("{provider}/{id}"),
                    name: model.get("name").and_then(Json::as_str).unwrap_or(id).to_owned(),
                    provider: provider.to_owned(),
                    efforts: Vec::new(),
                }
            })
        })
        .collect()
}

async fn read_events(inner: Arc<Inner>, mut events: EventStream) {
    loop {
        match events.next().await {
            Ok(Some(event)) => {
                let result = inner.mapper.locked().apply(&*inner.sink, event, now_ms());
                if let Err(error) = result {
                    tracing::error!(%error, "OpenCode event mapping failed");
                }
                inner.notify.notify_waiters();
            }
            Ok(None) => break,
            Err(error) => {
                tracing::error!(%error, "OpenCode event stream failed");
                break;
            }
        }
    }
    *inner.closed.locked() = true;
    inner.notify.notify_waiters();
}

/// One live OpenCode session. Dropping it detaches the event listener but does
/// not kill the caller-owned OpenCode server.
pub struct OpenCodeSession {
    inner: Arc<Inner>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for OpenCodeSession {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.locked().take() { worker.abort(); }
    }
}

#[async_trait]
impl HarnessSession for OpenCodeSession {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        if self.inner.closed() { return Err(AdapterError::Closed); }
        let ts = now_ms();
        let id = ids::ascending("msg", ts);
        let native_id = self.inner.id();
        let mut parts = vec![json!({ "type": "text", "text": prompt.text })];
        for attachment in &prompt.attachments {
            let url = reqwest::Url::from_file_path(&attachment.path).map_err(|_| AdapterError::Format("attachment requires an absolute file path".into()))?;
            let filename = attachment.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            parts.push(json!({ "type": "file", "url": url.as_str(), "filename": filename, "mime": attachment.mime.as_deref().unwrap_or("application/octet-stream") }));
        }
        let model = self.inner.mapper.locked().model().map(str::to_owned);
        let mut body = json!({ "messageID": id, "parts": parts });
        if let Some(model) = model {
            let (provider, model_id) = model.split_once('/').ok_or_else(|| AdapterError::Format("model must be provider/model".into()))?;
            body["model"] = json!({ "providerID": provider, "modelID": model_id });
        }
        if let Some(context) = &self.inner.context { body["system"] = json!(context); }
        let mode = *self.inner.mode.locked();
        if mode == SessionMode::Plan { body["agent"] = json!("plan"); }
        let turn = {
            let mut mapper = self.inner.mapper.locked();
            if mapper.state() != SessionState::Idle { return Err(AdapterError::Busy); }
            mapper.begin_turn(&*self.inner.sink, &id, &prompt.text, &prompt.by, ts)?
        };
        if let Err(error) = self.inner.client.prompt_async(&native_id, body).await {
            self.inner.mapper.locked().fail_turn(&*self.inner.sink, turn, &error.to_string(), now_ms())?;
            self.inner.notify.notify_waiters();
            return Err(error);
        }
        Ok(())
    }

    async fn interrupt(&self) -> Result<()> {
        if self.inner.closed() { return Err(AdapterError::Closed); }
        if self.state() == SessionState::Idle { return Ok(()); }
        self.inner.client.abort(&self.inner.id()).await?;
        self.inner.mapper.locked().cancel_turn(&*self.inner.sink, now_ms())?;
        self.inner.notify.notify_waiters();
        Ok(())
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        if self.inner.closed() { return Err(AdapterError::Closed); }
        let reply = self.inner.mapper.locked().plan_reply(decision, &answer)?;
        match reply {
            Reply::Permission { request_id, reply, message } => self.inner.client.permission_reply(&request_id, reply, message.as_deref()).await?,
            Reply::Question { request_id, answers } => self.inner.client.question_reply(&request_id, &answers).await?,
            Reply::RejectQuestion { request_id } => self.inner.client.question_reject(&request_id).await?,
            Reply::Later => {}
        }
        self.inner.mapper.locked().record_answer(&*self.inner.sink, decision, &answer, now_ms())?;
        self.inner.notify.notify_waiters();
        Ok(())
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        if selection.role.is_some() { return Err(AdapterError::Unsupported { harness: "opencode", what: "model roles" }); }
        if !selection.model.contains('/') { return Err(AdapterError::Format("model must be provider/model".into())); }
        if self.state() != SessionState::Idle { return Err(AdapterError::Busy); }
        self.inner.mapper.locked().set_model(&*self.inner.sink, &selection.model, now_ms())
    }

    /// The models of the server's connected providers.
    async fn models(&self) -> Result<Vec<HarnessModel>> {
        Ok(provider_models(&self.inner.client.providers().await?))
    }

    async fn set_mode(&self, mode: SessionMode) -> Result<()> {
        if self.state() != SessionState::Idle { return Err(AdapterError::Busy); }
        *self.inner.mode.locked() = mode;
        let root = self.inner.mapper.locked().root();
        self.inner.sink.commit_at(loa_core::Draft::set(root, attr::MODE, mode), now_ms())?;
        Ok(())
    }

    async fn compact(&self, instructions: Option<String>) -> Result<()> {
        if instructions.is_some() { return Err(AdapterError::Unsupported { harness: "opencode", what: "custom compaction instructions" }); }
        let model = self.inner.mapper.locked().model().map(str::to_owned).ok_or_else(|| AdapterError::Format("compaction requires a selected provider/model".into()))?;
        let (provider, model_id) = model.split_once('/').ok_or_else(|| AdapterError::Format("model must be provider/model".into()))?;
        self.inner.client.summarize(&self.inner.id(), provider, model_id).await
    }

    fn harness(&self) -> &'static str { "opencode" }
    fn state(&self) -> SessionState { if self.inner.closed() { SessionState::Closed } else { self.inner.mapper.locked().state() } }

    async fn wait_idle(&self) -> Result<()> {
        loop {
            let notified = self.inner.notify.notified();
            match self.state() {
                SessionState::Idle => return Ok(()),
                SessionState::Closed => return Err(AdapterError::Closed),
                _ => notified.await,
            }
        }
    }

    async fn shutdown(&self) -> Result<()> {
        *self.inner.closed.locked() = true;
        if let Some(worker) = self.worker.locked().take() { worker.abort(); }
        self.inner.notify.notify_waiters();
        Ok(())
    }
}
