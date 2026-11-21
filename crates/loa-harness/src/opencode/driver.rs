//! A session on a caller-owned `opencode serve` and its event-stream worker.

use super::client::{Client, EventStream, ServerAuth};
use super::mapper::{Mapper, RootInfo};
use super::{HARNESS, OpenCodeNative, ids};
use crate::kit::NativeCodec;
use async_trait::async_trait;
use loa_contracts::harness::{
    Adapter, AdapterError, Answer, Availability, AvailabilityStatus, Capabilities, ContextDelivery, Feature, HarnessModel, HarnessSession,
    McpTransport, ModelSelection, ModelSwitch, Prompt, Result, SessionConfig, SessionState,
};
use loa_core::{DecisionKind, Dom, DomWriter, NodeId, PatchSink, SCHEMA_VERSION, SessionMode, attr};
use loa_util::LockExt;
use reqwest::Method;
use serde_json::{Value as Json, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// How long [`Adapter::availability`] waits for the server.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Connects to a caller-owned `opencode serve`; never launches it or reads credentials from the environment.
#[derive(Clone)]
pub struct OpenCodeAdapter {
    base: String,
    auth: Option<ServerAuth>,
}

impl OpenCodeAdapter {
    /// `base` is the server URL; `auth` is explicit HTTP basic auth.
    pub fn new(base: impl Into<String>, auth: Option<ServerAuth>) -> Self {
        Self { base: base.into(), auth }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn client(&self, config: &SessionConfig) -> Client {
        Client::new(&self.base, &config.cwd.to_string_lossy(), self.auth.clone())
    }

    /// Opens the event stream, then `mapper(client, sink)` builds the mapper, then the worker runs.
    async fn attach(&self, config: SessionConfig, sink: Arc<dyn PatchSink>, mapper: impl AsyncFnOnce(&Client, &DomWriter) -> Result<Mapper>) -> Result<Box<dyn HarnessSession>> {
        let client = self.client(&config);
        let events = client.events().await?;
        let mapper = mapper(&client, &DomWriter::new(&*sink)).await?;
        let inner = Arc::new(Inner {
            client,
            sink,
            mapper: Mutex::new(mapper),
            notify: Notify::new(),
            context: config.context,
            mode: Mutex::new(config.mode.unwrap_or(SessionMode::Normal)),
            closed: Mutex::new(false),
        });
        let worker = tokio::spawn(read_events(inner.clone(), events));
        Ok(Box::new(OpenCodeSession { inner, worker: Mutex::new(Some(worker)) }))
    }
}

/// The mapper of a resumed or forked session, with its native id checked.
fn resumed(dom: &Dom) -> Result<(Mapper, String)> {
    let mapper = Mapper::from_dom(dom)?;
    let id = mapper.native_id().to_owned();
    if id.is_empty() {
        return Err(AdapterError::Format("session has no OpenCode native_id".into()));
    }
    Ok((mapper, id))
}

#[async_trait]
impl Adapter for OpenCodeAdapter {
    fn harness(&self) -> &'static str {
        HARNESS
    }

    /// `GET /global/health` (its version), else `GET /config/providers` on servers without it.
    async fn availability(&self) -> Availability {
        let directory = std::env::current_dir().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
        let client = Client::new(&self.base, &directory, self.auth.clone());
        let base = client.base().to_owned();
        let unreachable = |error: reqwest::Error| {
            // A refused or silent port is the common case; reqwest's text for it only repeats the URL.
            let reason = if error.is_connect() || error.is_timeout() {
                format!("no OpenCode server at {base}; start `opencode serve`")
            } else {
                format!("no OpenCode server at {base} (start `opencode serve`): {error}")
            };
            Availability::unavailable(AvailabilityStatus::Unreachable, reason)
        };
        let ready = |version: Option<String>| Availability { detail: Some(version.clone().unwrap_or_else(|| base.clone())), ..Availability::ready(None, version) };
        match client.probe("/global/health", PROBE_TIMEOUT).await {
            Err(error) => unreachable(error),
            Ok((status, health)) if status.is_success() => ready(health.get("version").and_then(Json::as_str).map(str::to_owned)),
            Ok(_) => match client.probe("/config/providers", PROBE_TIMEOUT).await {
                Err(error) => unreachable(error),
                Ok((status, _)) if status.is_success() => ready(None),
                Ok((status, _)) => Availability::unavailable(AvailabilityStatus::Incompatible, format!("the server at {base} answered HTTP {status}")),
            },
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::builder()
            .schema(SCHEMA_VERSION)
            .features(&[Feature::Fork, Feature::Rewind, Feature::Compaction, Feature::Subagents, Feature::PlanMode])
            .model_switch(ModelSwitch::PerTurn)
            .decisions(&[DecisionKind::Approval, DecisionKind::Question])
            .context(&[ContextDelivery::SystemPrompt])
            .mcp_servers()
            .import()
            .export()
            .build()
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        if !config.host_tools.is_empty() || !config.disabled_tools.is_empty() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "host tools or disabled tools" });
        }
        let root = RootInfo {
            session_id: config.session_id.clone(),
            node: config.node.clone(),
            cwd: Some(config.cwd.to_string_lossy().into_owned()),
            project: config.project.clone(),
            model: config.model.clone(),
            model_role: config.model_role.clone(),
            effort: config.effort.clone(),
            mode: config.mode,
            ..RootInfo::default()
        };
        let servers = config.mcp_servers.clone();
        self.attach(config, sink, async move |client: &Client, w: &DomWriter| {
            let native = client.json(Method::POST, "/session", Some(json!({}))).await?;
            for server in &servers {
                let config = match &server.transport {
                    McpTransport::Http { url, headers } => json!({ "type": "remote", "url": url, "headers": headers }),
                    McpTransport::Stdio { command, args, env } => {
                        json!({ "type": "local", "command": std::iter::once(command).chain(args).collect::<Vec<_>>(), "environment": env })
                    }
                };
                client.endpoint(Method::POST, "/mcp", Some(json!({ "name": server.name, "config": config }))).await?;
            }
            Mapper::start(w, RootInfo { native, ..root })
        })
        .await
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let (mut mapper, id) = resumed(dom)?;
        self.attach(config, sink, async move |client: &Client, w: &DomWriter| {
            client.endpoint(Method::GET, &format!("/session/{id}"), None).await?;
            mapper.cancel_turn(w)?;
            Ok(mapper)
        })
        .await
    }

    async fn fork(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let (mut mapper, source) = resumed(dom)?;
        self.attach(config, sink, async move |client: &Client, w: &DomWriter| {
            let native: Json = client.json(Method::POST, &format!("/session/{source}/fork"), Some(json!({}))).await?;
            let id = native.get("id").and_then(Json::as_str).ok_or_else(|| AdapterError::Protocol("fork response has no id".into()))?;
            mapper.set_native_id(w, id)?;
            mapper.cancel_turn(w)?;
            Ok(mapper)
        })
        .await
    }

    fn import(&self, path: &Path, sink: &dyn PatchSink) -> Result<()> {
        OpenCodeNative::import(path, sink)
    }

    fn export(&self, dom: &Dom, path: &Path) -> Result<()> {
        OpenCodeNative::export(dom, path)
    }
}

struct Inner {
    client: Client,
    sink: Arc<dyn PatchSink>,
    mapper: Mutex<Mapper>,
    notify: Notify,
    context: Option<String>,
    mode: Mutex<SessionMode>,
    closed: Mutex<bool>,
}

impl Inner {
    fn closed(&self) -> bool {
        *self.closed.locked()
    }

    fn id(&self) -> String {
        self.mapper.locked().native_id().to_owned()
    }

    /// Runs `f` on the mapper with a writer to the session, then wakes waiters.
    fn write<T>(&self, f: impl FnOnce(&mut Mapper, &DomWriter) -> Result<T>) -> Result<T> {
        let result = f(&mut self.mapper.locked(), &DomWriter::new(&*self.sink));
        self.notify.notify_waiters();
        result
    }

    fn model(&self) -> Result<(String, String)> {
        let model = self.mapper.locked().model().map(str::to_owned).ok_or_else(|| AdapterError::Format("no provider/model is selected".into()))?;
        let (provider, id) = model.split_once('/').ok_or_else(|| AdapterError::Format("model must be provider/model".into()))?;
        Ok((provider.to_owned(), id.to_owned()))
    }
}

/// The models of `GET /config/providers`, as `provider/model` ids.
pub fn provider_models(providers: &Json) -> Vec<HarnessModel> {
    let Some(list) = providers.get("providers").and_then(Json::as_array) else { return Vec::new() };
    let models = list.iter().filter_map(|provider| Some((provider.get("id")?.as_str()?, provider.get("models")?.as_object()?)));
    models
        .flat_map(|(provider, models)| {
            models.iter().map(move |(key, model)| {
                let id = model.get("id").and_then(Json::as_str).unwrap_or(key);
                let name = model.get("name").and_then(Json::as_str).unwrap_or(id).to_owned();
                HarnessModel { id: format!("{provider}/{id}"), name, provider: provider.to_owned(), efforts: Vec::new() }
            })
        })
        .collect()
}

async fn read_events(inner: Arc<Inner>, mut events: EventStream) {
    loop {
        match events.next().await {
            Ok(Some(event)) => {
                if let Err(error) = inner.write(|mapper, w| mapper.apply_event(w, event)) {
                    tracing::error!(%error, "OpenCode event mapping failed");
                }
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

/// One live session. Dropping it detaches the event listener; the server keeps running.
pub struct OpenCodeSession {
    inner: Arc<Inner>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for OpenCodeSession {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.locked().take() {
            worker.abort();
        }
    }
}

impl OpenCodeSession {
    fn open(&self) -> Result<&Inner> {
        if self.inner.closed() { Err(AdapterError::Closed) } else { Ok(&self.inner) }
    }

    fn idle(&self) -> Result<()> {
        if self.state() == SessionState::Idle { Ok(()) } else { Err(AdapterError::Busy) }
    }
}

#[async_trait]
impl HarnessSession for OpenCodeSession {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        let inner = self.open()?;
        let id = ids::ascending("msg", loa_core::now_ms());
        let mut parts = vec![json!({ "type": "text", "text": prompt.text })];
        for attachment in &prompt.attachments {
            let url = reqwest::Url::from_file_path(&attachment.path).map_err(|_| AdapterError::Format("attachment requires an absolute file path".into()))?;
            let filename = attachment.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            parts.push(json!({ "type": "file", "url": url.as_str(), "filename": filename, "mime": attachment.mime.as_deref().unwrap_or("application/octet-stream") }));
        }
        let mut body = json!({ "messageID": id, "parts": parts });
        if inner.mapper.locked().model().is_some() {
            let (provider, model) = inner.model()?;
            body["model"] = json!({ "providerID": provider, "modelID": model });
        }
        if let Some(context) = &inner.context {
            body["system"] = json!(context);
        }
        if *inner.mode.locked() == SessionMode::Plan {
            body["agent"] = json!("plan");
        }
        let turn = inner.write(|mapper, w| {
            if mapper.state() != SessionState::Idle {
                return Err(AdapterError::Busy);
            }
            mapper.begin_turn(w, &id, &prompt.text, &prompt.by)
        })?;
        if let Err(error) = inner.client.endpoint(Method::POST, &format!("/session/{}/prompt_async", inner.id()), Some(body)).await {
            inner.write(|mapper, w| mapper.fail_turn(w, turn, &error.to_string()))?;
            return Err(error);
        }
        Ok(())
    }

    async fn interrupt(&self) -> Result<()> {
        let inner = self.open()?;
        if self.state() == SessionState::Idle {
            return Ok(());
        }
        inner.client.endpoint(Method::POST, &format!("/session/{}/abort", inner.id()), None).await?;
        inner.write(|mapper, w| mapper.cancel_turn(w))
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        let inner = self.open()?;
        let reply = inner.mapper.locked().plan_reply(decision, &answer)?;
        if let Some(reply) = reply {
            inner.client.endpoint(Method::POST, &reply.path, reply.body).await?;
        }
        inner.write(|mapper, w| mapper.record_answer(w, decision, &answer))
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        if selection.role.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "model roles" });
        }
        if !selection.model.contains('/') {
            return Err(AdapterError::Format("model must be provider/model".into()));
        }
        self.idle()?;
        self.inner.write(|mapper, w| mapper.set_model(w, &selection.model))
    }

    /// The models of the server's connected providers.
    async fn models(&self) -> Result<Vec<HarnessModel>> {
        Ok(provider_models(&self.inner.client.json(Method::GET, "/config/providers", None).await?))
    }

    async fn set_mode(&self, mode: SessionMode) -> Result<()> {
        self.idle()?;
        *self.inner.mode.locked() = mode;
        self.inner.write(|mapper, w| Ok(w.set(mapper.root(), attr::MODE, mode)?))
    }

    async fn compact(&self, instructions: Option<String>) -> Result<()> {
        if instructions.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "custom compaction instructions" });
        }
        let (provider, model) = self.inner.model()?;
        let body = json!({ "providerID": provider, "modelID": model });
        self.inner.client.endpoint(Method::POST, &format!("/session/{}/summarize", self.inner.id()), Some(body)).await.map(drop)
    }

    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn state(&self) -> SessionState {
        if self.inner.closed() { SessionState::Closed } else { self.inner.mapper.locked().state() }
    }

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
        if let Some(worker) = self.worker.locked().take() {
            worker.abort();
        }
        self.inner.notify.notify_waiters();
        Ok(())
    }
}
