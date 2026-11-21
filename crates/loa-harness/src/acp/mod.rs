//! Generic Agent Client Protocol adapter.
//!
//! One [`AcpAdapter`] drives any agent that speaks ACP (JSON-RPC 2.0 over
//! stdio, protocol version 1). It is built with the agent's command line;
//! each session runs its own agent process. The harness id is `acp`.
//!
//! Mapping onto the session DOM:
//! - `initialize` negotiates the protocol version and records what the agent
//!   offers; [`AcpAdapter::capabilities`] reflects the last negotiation
//! - `session/new` starts a session with the config's MCP servers,
//!   `session/load` resumes one (its replayed history is skipped, the DOM has it)
//! - `session/prompt` runs a turn, `session/cancel` interrupts it
//! - `session/update` chunks, tool calls, plans and mode changes become
//!   messages, `<call>` elements, `<todo>` elements and session attributes;
//!   anything else is kept as `<ext harness="acp">`
//! - `session/request_permission` becomes an approval `<decision>`; the
//!   answer is sent back as the picked option id
//! - `fs/*` and `terminal/*` requests from the agent are served inside the
//!   session cwd; paths outside it are refused

pub mod host;
pub mod mapper;
mod session;
mod wire;

pub const HARNESS: &str = "acp";
pub use mapper::{X_AGENT, X_MODE, X_OPTION, X_OPTION_IDS, X_OPTION_KINDS, X_PRIORITY, X_STOP_REASON};
pub use session::AcpSession;

use loa_util::LockExt;
use async_trait::async_trait;
use loa_contracts::harness::{
    Adapter, AdapterError, Availability, AvailabilityStatus, Capabilities, ContextDelivery, Feature, HarnessModel,
    HarnessSession, McpTransport, ModelSwitch, Result, SessionConfig, SessionState,
};
use loa_core::{DecisionKind, Dom, DomWriter, Draft, ElementKind, MemorySink, NodeId, PatchSink, SCHEMA_VERSION, SessionMode, Value, attr, now_ms};
use crate::kit::{JsonRpc2, StdioRpc, insert_root};
use host::Host;
use mapper::{AcpMapper, Input};
use serde_json::{Value as Json, json};
use session::{Modes, Shared};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::process::Command;
use tokio::sync::watch;
use wire::{AgentCapabilities, InitializeResponse, LoadSessionResponse, NewSessionResponse, SessionModeState, SessionModelState};


/// What the last `initialize` and `session/new` taught about the agent.
#[derive(Clone, Copy, Debug, Default)]
struct Learned {
    plan_mode: bool,
    models: bool,
}

/// The ACP adapter. One instance serves one agent command line.
pub struct AcpAdapter {
    program: PathBuf,
    args: Vec<String>,
    learned: Mutex<Option<Learned>>,
    /// What availability says when the program is missing.
    missing: Option<String>,
}

impl AcpAdapter {
    /// An adapter that runs `program args…` for every session.
    pub fn new(program: impl Into<PathBuf>, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self { program: program.into(), args: args.into_iter().map(Into::into).collect(), learned: Mutex::new(None), missing: None }
    }

    /// Says `hint` instead of "not found" when the program is missing.
    pub fn with_missing_hint(mut self, hint: impl Into<String>) -> Self {
        self.missing = Some(hint.into());
        self
    }

    /// The capabilities every ACP agent is guaranteed to have.
    /// Every ACP agent takes stdio MCP servers, so the app can serve project
    /// context as an MCP resource.
    pub fn base_capabilities() -> Capabilities {
        let builder = Capabilities::builder().schema(SCHEMA_VERSION).decisions(&[DecisionKind::Approval]);
        builder.context(&[ContextDelivery::McpResource]).mcp_servers().build()
    }

    fn learn(&self, update: impl FnOnce(&mut Learned)) {
        update(self.learned.locked().get_or_insert_with(Learned::default));
    }

    /// Spawns the agent and starts the reader. `root` is the `<session>`
    /// node the projector writes under.
    async fn connect(
        &self,
        config: &SessionConfig,
        sink: Arc<dyn PatchSink>,
        root: NodeId,
        model: Option<String>,
    ) -> Result<(Arc<Shared>, AcpSession)> {
        if !config.host_tools.is_empty() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "host tools" });
        }
        let host = Arc::new(Host::new(&config.cwd)?);
        let mapper = AcpMapper::new(root, model);
        let (state, _) = watch::channel(SessionState::Idle);
        let shared = Arc::new(Shared {
            mapper: Mutex::new(mapper),
            sink,
            host,
            session_id: OnceLock::new(),
            writer: OnceLock::new(),
            writer_ready: tokio::sync::Notify::new(),
            state,
            shutting_down: AtomicBool::new(false),
            modes: Mutex::new(Modes::default()),
            models: Mutex::new(None),
        });
        let program = config.executable.as_ref().unwrap_or(&self.program);
        let mut command = Command::new(program);
        command.args(&self.args).current_dir(&config.cwd).envs(&config.env);
        let rpc = Arc::new(StdioRpc::spawn(command, JsonRpc2 { header: true }, shared.clone())?);
        let _ = shared.writer.set(rpc.writer());
        shared.writer_ready.notify_one();
        let session = AcpSession { shared: shared.clone(), rpc };
        Ok((shared, session))
    }

    /// Runs `initialize` and checks the protocol version.
    async fn initialize(&self, shared: &Shared, rpc: &StdioRpc<JsonRpc2>) -> Result<AgentCapabilities> {
        let params = json!({
            "protocolVersion": wire::PROTOCOL_VERSION,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true },
                "terminal": true,
            },
            "clientInfo": { "name": "loa", "version": env!("CARGO_PKG_VERSION") },
        });
        let init: InitializeResponse = rpc.request("initialize", params).await?;
        if init.protocol_version != wire::PROTOCOL_VERSION {
            return Err(AdapterError::Version {
                harness: HARNESS,
                detail: format!(
                    "the agent speaks ACP version {}, this client speaks {}",
                    init.protocol_version,
                    wire::PROTOCOL_VERSION
                ),
            });
        }
        if let Some(agent) = &init.agent_info {
            let name = match &agent.version {
                Some(version) => format!("{} {version}", agent.name),
                None => agent.name.clone(),
            };
            DomWriter::new(&*shared.sink).set(shared.mapper.locked().root(), X_AGENT, name)?;
        }
        Ok(init.agent_capabilities)
    }

    /// `initialize` then `session/new`.
    async fn create(&self, config: &SessionConfig, shared: &Shared, rpc: &StdioRpc<JsonRpc2>) -> Result<()> {
        let agent = self.initialize(shared, rpc).await?;
        let params = json!({
            "cwd": shared.host.root().to_string_lossy(),
            "mcpServers": mcp_servers(config, &agent)?,
        });
        let created: NewSessionResponse = rpc.request("session/new", params).await?;
        shared.mapper.locked().set_native(&DomWriter::new(&*shared.sink), &created.session_id)?;
        let _ = shared.session_id.set(created.session_id);
        self.adopt_models(config, shared, rpc, created.models).await?;
        self.adopt_modes(shared, rpc, created.modes, config.mode).await
    }

    /// `initialize` then `session/load` of `native_id`, skipping the history it replays.
    async fn load(&self, config: &SessionConfig, shared: &Shared, rpc: &StdioRpc<JsonRpc2>, native_id: String) -> Result<()> {
        let agent = self.initialize(shared, rpc).await?;
        if !agent.load_session {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "resume" });
        }
        let params = json!({
            "sessionId": native_id,
            "cwd": shared.host.root().to_string_lossy(),
            "mcpServers": mcp_servers(config, &agent)?,
        });
        shared.mapper.locked().adopt_native(&native_id);
        let _ = shared.session_id.set(native_id);
        let id = rpc.next_id();
        // The ordered response hook clears replay before later notifications.
        shared.feed(Input::Load { id: id.to_string() })?;
        let loaded = rpc.request_as(json!(id), "session/load", params).await?;
        let loaded: LoadSessionResponse = if loaded.is_null() { LoadSessionResponse::default() } else { decode(loaded)? };
        self.adopt_models(config, shared, rpc, loaded.models).await?;
        self.adopt_modes(shared, rpc, loaded.modes, config.mode).await
    }

    /// Records the models the agent offers and switches to the configured one
    /// when it is among them.
    async fn adopt_models(&self, config: &SessionConfig, shared: &Shared, rpc: &StdioRpc<JsonRpc2>, models: Option<SessionModelState>) -> Result<()> {
        self.learn(|learned| learned.models = models.is_some());
        let Some(models) = models else { return Ok(()) };
        let offered: Vec<HarnessModel> = models
            .available_models
            .into_iter()
            .map(|model| HarnessModel {
                name: if model.name.is_empty() { model.model_id.clone() } else { model.name },
                id: model.model_id,
                provider: HARNESS.to_owned(),
                efforts: Vec::new(),
            })
            .collect();
        let wanted = config.model.as_deref().filter(|wanted| *wanted != models.current_model_id);
        let switch = wanted.filter(|wanted| offered.iter().any(|m| m.id == *wanted)).map(str::to_owned);
        *shared.models.locked() = Some(offered);
        match switch {
            Some(model) => AcpSession::switch_model(shared, rpc, &model).await,
            None if wanted.is_none() => {
                shared.mapper.locked().model_changed(&DomWriter::new(&*shared.sink), &models.current_model_id)?;
                Ok(())
            }
            None => Err(AdapterError::Protocol(format!("the agent doesn't offer model {}", config.model.as_deref().unwrap_or_default()))),
        }
    }

    /// Records the modes the agent offers and switches to the configured one.
    async fn adopt_modes(&self, shared: &Shared, rpc: &StdioRpc<JsonRpc2>, modes: Option<SessionModeState>, wanted: Option<SessionMode>) -> Result<()> {
        let Some(modes) = modes else {
            self.learn(|learned| learned.plan_mode = false);
            return Ok(());
        };
        let available: Vec<String> = modes.available_modes.into_iter().map(|m| m.id).collect();
        self.learn(|learned| learned.plan_mode = available.iter().any(|m| m == "plan"));
        {
            let mut known = shared.modes.locked();
            known.available = available;
            known.initial = Some(modes.current_mode_id.clone());
        }
        shared.mapper.locked().mode_changed(&DomWriter::new(&*shared.sink), &modes.current_mode_id)?;
        if let Some(mode) = wanted {
            let target = AcpSession::mode_id(shared, mode)
                .ok_or(AdapterError::Unsupported { harness: HARNESS, what: "the configured mode" })?;
            if target != modes.current_mode_id {
                AcpSession::switch_mode(shared, rpc, &target).await?;
            }
        }
        Ok(())
    }
}

/// Hands out `session` once `setup` succeeded, and shuts the agent down otherwise.
async fn finish(session: AcpSession, setup: Result<()>) -> Result<Box<dyn HarnessSession>> {
    match setup {
        Ok(()) => Ok(Box::new(session)),
        Err(e) => {
            let _ = session.shutdown().await;
            Err(e)
        }
    }
}

#[async_trait]
impl Adapter for AcpAdapter {
    fn harness(&self) -> &'static str {
        HARNESS
    }

    /// Starts the agent and runs `initialize`: ready when it speaks this ACP
    /// version, with the agent's name and version as the detail.
    async fn availability(&self) -> Availability {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let config = SessionConfig { cwd, ..SessionConfig::default() };
        let sink = Arc::new(MemorySink::new());
        let probe = async {
            let attrs = [(attr::ID, Value::from("availability-probe")), (attr::HARNESS, HARNESS.into()), (attr::NODE, "".into()), (attr::CREATED, Value::Int(now_ms()))];
            let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?.node;
            let (shared, session) = self.connect(&config, sink.clone(), root, None).await?;
            let initialized = self.initialize(&shared, &session.rpc).await;
            let _ = session.shutdown().await;
            initialized.map(drop)
        };
        match tokio::time::timeout(Availability::PROBE_TIMEOUT, probe).await {
            Ok(Ok(())) => {
                let agent = sink.with_dom(|dom| dom.root().and_then(|root| root.str_attr(X_AGENT)).map(str::to_owned));
                Availability { detail: agent.or_else(|| Some("ACP agent".to_owned())), ..Availability::ready(None, None) }
            }
            Ok(Err(AdapterError::NotInstalled(program))) => {
                Availability::unavailable(AvailabilityStatus::NotInstalled, self.missing.clone().unwrap_or_else(|| format!("`{program}` not found")))
            }
            Ok(Err(AdapterError::Version { detail, .. })) => Availability::unavailable(AvailabilityStatus::Incompatible, detail),
            Ok(Err(error)) => Availability::unavailable(AvailabilityStatus::Failed, error.to_string()),
            Err(_) => Availability::unavailable(AvailabilityStatus::Failed, "the agent didn't answer `initialize`"),
        }
    }

    fn capabilities(&self) -> Capabilities {
        let mut capabilities = Self::base_capabilities();
        let learned = *self.learned.locked();
        if learned.is_some_and(|l| l.plan_mode) {
            capabilities.features.push(Feature::PlanMode);
        }
        if learned.is_some_and(|l| l.models) {
            capabilities.model_switch = ModelSwitch::PerTurn;
        }
        capabilities
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        // The root goes in before the agent starts so updates that arrive
        // right after `session/new` already have a home.
        let root = insert_root(&DomWriter::new(&*sink), HARNESS, &config)?;
        let (shared, session) = self.connect(&config, sink, root, config.model.clone()).await?;
        let setup = self.create(&config, &shared, &session.rpc).await;
        finish(session, setup).await
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let root = dom.root().ok_or_else(|| AdapterError::Format("the session DOM is empty".to_owned()))?;
        let native_id = root
            .str_attr(attr::NATIVE_ID)
            .ok_or_else(|| AdapterError::Format("the session has no native_id".to_owned()))?
            .to_owned();
        let model = config.model.clone().or_else(|| root.str_attr(attr::MODEL).map(str::to_owned));
        let (shared, session) = self.connect(&config, sink, root.id, model).await?;
        shared.mapper.locked().adopt(dom);
        let setup = self.load(&config, &shared, &session.rpc, native_id).await;
        finish(session, setup).await
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: Json) -> Result<T> {
    serde_json::from_value(value).map_err(|e| AdapterError::Protocol(format!("malformed response: {e}")))
}

/// The config's MCP servers in ACP's `McpServer` shape. HTTP servers need
/// the agent's `mcpCapabilities.http`; stdio is mandatory for every agent.
fn mcp_servers(config: &SessionConfig, agent: &AgentCapabilities) -> Result<Vec<Json>> {
    let pairs = |map: &std::collections::BTreeMap<String, String>| {
        map.iter().map(|(name, value)| json!({ "name": name, "value": value })).collect::<Vec<_>>()
    };
    config
        .mcp_servers
        .iter()
        .map(|server| match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                Ok(json!({ "name": server.name, "command": command, "args": args, "env": pairs(env) }))
            }
            McpTransport::Http { url, headers } if agent.mcp_capabilities.http => {
                Ok(json!({ "type": "http", "name": server.name, "url": url, "headers": pairs(headers) }))
            }
            McpTransport::Http { .. } => Err(AdapterError::Unsupported { harness: HARNESS, what: "HTTP MCP servers" }),
        })
        .collect()
}
