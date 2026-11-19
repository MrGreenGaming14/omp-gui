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

mod fs;
mod project;
mod rpc;
mod session;
mod terminal;
mod wire;

pub use project::{HARNESS, X_AGENT, X_MODE, X_OPTION, X_OPTION_IDS, X_OPTION_KINDS, X_PRIORITY, X_STOP_REASON};
pub use session::AcpSession;

use async_trait::async_trait;
use loa_adapter::{
    Adapter, AdapterError, Capabilities, ContextDelivery, Feature, HarnessSession, McpTransport, ModelSwitch, Result,
    SessionConfig,
};
use loa_core::{
    DecisionKind, Dom, Draft, ElementKind, NodeId, PatchSink, SCHEMA_VERSION, SessionMode, TodoState, Value, attr,
    now_ms,
};
use project::Projector;
use rpc::Rpc;
use serde_json::{Value as Json, json};
use session::{Modes, Shared};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use terminal::Terminals;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use wire::{AgentCapabilities, InitializeResponse, LoadSessionResponse, NewSessionResponse, SessionModeState};

/// A byte stream to one agent: what it writes, what it reads, and its
/// process when there is one.
pub struct Transport {
    pub from_agent: Box<dyn AsyncRead + Send + Unpin>,
    pub to_agent: Box<dyn AsyncWrite + Send + Unpin>,
    /// Killed on shutdown and when the session is dropped.
    pub child: Option<Child>,
}

/// Opens a connection to a fresh agent for one session.
#[async_trait]
pub trait Connector: Send + Sync {
    async fn connect(&self, config: &SessionConfig) -> Result<Transport>;
}

/// Runs the agent as a child process and talks to it over stdin and stdout.
/// Its stderr goes to `tracing` at debug level.
#[derive(Clone, Debug)]
pub struct CommandConnector {
    program: PathBuf,
    args: Vec<String>,
}

impl CommandConnector {
    pub fn new(program: impl Into<PathBuf>, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self { program: program.into(), args: args.into_iter().map(Into::into).collect() }
    }
}

#[async_trait]
impl Connector for CommandConnector {
    async fn connect(&self, config: &SessionConfig) -> Result<Transport> {
        let program = config.executable.as_ref().unwrap_or(&self.program);
        let mut child = Command::new(program)
            .args(&self.args)
            .current_dir(&config.cwd)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => AdapterError::NotInstalled(program.display().to_string()),
                _ => AdapterError::Io(e),
            })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(AdapterError::Protocol("the agent's stdio wasn't captured".to_owned()));
        };
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "loa_adapter_acp::agent", "{line}");
                }
            });
        }
        Ok(Transport { from_agent: Box::new(stdout), to_agent: Box::new(stdin), child: Some(child) })
    }
}

/// What the last `initialize` and `session/new` taught about the agent.
#[derive(Clone, Copy, Debug, Default)]
struct Learned {
    plan_mode: bool,
}

/// The ACP adapter. One instance serves one agent command line.
pub struct AcpAdapter {
    connector: Arc<dyn Connector>,
    learned: Mutex<Option<Learned>>,
}

impl AcpAdapter {
    /// An adapter that runs `program args…` for every session.
    pub fn new(program: impl Into<PathBuf>, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::with_connector(Arc::new(CommandConnector::new(program, args)))
    }

    /// An adapter that reaches agents through `connector` (another process
    /// launcher, a remote host, an in-process agent).
    pub fn with_connector(connector: Arc<dyn Connector>) -> Self {
        Self { connector, learned: Mutex::new(None) }
    }

    /// The capabilities every ACP agent is guaranteed to have.
    pub fn base_capabilities() -> Capabilities {
        Capabilities {
            schema_version: SCHEMA_VERSION,
            features: Vec::new(),
            model_switch: ModelSwitch::Never,
            decisions: vec![DecisionKind::Approval],
            // Every ACP agent takes stdio MCP servers, so the app can serve
            // project context as an MCP resource.
            context_delivery: vec![ContextDelivery::McpResource],
            host_tools: false,
            mcp_servers: true,
            queue_mid_turn: false,
            effort: false,
            import: false,
            export: false,
        }
    }

    fn learn(&self, learned: Learned) {
        *self.learned.lock().unwrap_or_else(PoisonError::into_inner) = Some(learned);
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
        let cwd = config.cwd.canonicalize()?;
        let transport = self.connector.connect(config).await?;
        let rpc = Rpc::new(transport.to_agent);
        let projector = Projector::new(sink, root, model);
        let (state, _) = watch::channel(projector.state());
        let shared = Arc::new(Shared {
            rpc: rpc.clone(),
            projector: Mutex::new(projector),
            session_id: OnceLock::new(),
            terminals: Terminals::new(cwd.clone()),
            root: cwd,
            state,
            shutting_down: AtomicBool::new(false),
            modes: Mutex::new(Modes::default()),
        });
        let reader = tokio::spawn(rpc::read_loop(transport.from_agent, rpc, shared.clone()));
        let session = AcpSession::new(shared.clone(), transport.child, reader);
        Ok((shared, session))
    }

    /// Runs `initialize` and checks the protocol version.
    async fn initialize(&self, shared: &Shared) -> Result<AgentCapabilities> {
        let params = json!({
            "protocolVersion": wire::PROTOCOL_VERSION,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true },
                "terminal": true,
            },
            "clientInfo": { "name": "loa", "version": env!("CARGO_PKG_VERSION") },
        });
        let init: InitializeResponse = decode(shared.rpc.request("initialize", params).await?)?;
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
            let projector = shared.projector();
            projector.set(projector.root(), X_AGENT, name.into());
        }
        Ok(init.agent_capabilities)
    }

    /// `initialize` then `session/new`.
    async fn create(&self, config: &SessionConfig, shared: &Shared) -> Result<()> {
        let agent = self.initialize(shared).await?;
        let params = json!({
            "cwd": shared.root.to_string_lossy(),
            "mcpServers": mcp_servers(config, &agent)?,
        });
        let created: NewSessionResponse = decode(shared.rpc.request("session/new", params).await?)?;
        {
            let projector = shared.projector();
            projector.set(projector.root(), attr::NATIVE_ID, created.session_id.as_str().into());
        }
        let _ = shared.session_id.set(created.session_id);
        self.adopt_modes(shared, created.modes, config.mode).await
    }

    /// `initialize` then `session/load` of `native_id`, skipping the history it replays.
    async fn load(&self, config: &SessionConfig, shared: &Shared, native_id: String) -> Result<()> {
        let agent = self.initialize(shared).await?;
        if !agent.load_session {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "resume" });
        }
        let params = json!({
            "sessionId": native_id,
            "cwd": shared.root.to_string_lossy(),
            "mcpServers": mcp_servers(config, &agent)?,
        });
        let _ = shared.session_id.set(native_id);
        let id = shared.rpc.next_id();
        // The response clears replay mode in the reader, in order with the
        // notifications before it.
        shared.projector().set_replay(Some(id));
        let rx = shared.rpc.start_request(id, "session/load", params)?;
        let loaded = rx.await.unwrap_or(Err(rpc::RpcError::Closed))?;
        // Agents answer an empty `LoadSessionResponse` as `null` or `{}`.
        let loaded: LoadSessionResponse = if loaded.is_null() { LoadSessionResponse::default() } else { decode(loaded)? };
        self.adopt_modes(shared, loaded.modes, config.mode).await
    }

    /// Records the modes the agent offers and switches to the configured one.
    async fn adopt_modes(&self, shared: &Shared, modes: Option<SessionModeState>, wanted: Option<SessionMode>) -> Result<()> {
        let Some(modes) = modes else {
            self.learn(Learned { plan_mode: false });
            return Ok(());
        };
        let available: Vec<String> = modes.available_modes.into_iter().map(|m| m.id).collect();
        self.learn(Learned { plan_mode: available.iter().any(|m| m == "plan") });
        {
            let mut known = shared.modes.lock().unwrap_or_else(PoisonError::into_inner);
            known.available = available;
            known.initial = Some(modes.current_mode_id.clone());
        }
        shared.projector().mode_changed(&modes.current_mode_id);
        if let Some(mode) = wanted {
            let target = AcpSession::mode_id(shared, mode)
                .ok_or(AdapterError::Unsupported { harness: HARNESS, what: "the configured mode" })?;
            if target != modes.current_mode_id {
                AcpSession::switch_mode(shared, &target).await?;
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

    fn capabilities(&self) -> Capabilities {
        let mut capabilities = Self::base_capabilities();
        let learned = *self.learned.lock().unwrap_or_else(PoisonError::into_inner);
        if learned.is_some_and(|l| l.plan_mode) {
            capabilities.features.push(Feature::PlanMode);
        }
        capabilities
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let mut attrs = vec![
            (attr::ID, Value::from(config.session_id.as_str())),
            (attr::HARNESS, HARNESS.into()),
            (attr::NODE, config.node.as_str().into()),
            (attr::CREATED, Value::Int(now_ms())),
            (attr::SCHEMA, Value::Int(i64::from(SCHEMA_VERSION))),
            (attr::CWD, config.cwd.to_string_lossy().into_owned().into()),
        ];
        let optional = [
            (attr::PROJECT, &config.project),
            (attr::MODEL, &config.model),
            (attr::MODEL_ROLE, &config.model_role),
            (attr::EFFORT, &config.effort),
        ];
        for (name, value) in optional {
            if let Some(value) = value {
                attrs.push((name, value.as_str().into()));
            }
        }
        // The root goes in before the agent starts so updates that arrive
        // right after `session/new` already have a home.
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?.node;
        let (shared, session) = self.connect(&config, sink, root, config.model.clone()).await?;
        let setup = self.create(&config, &shared).await;
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
        {
            let mut projector = shared.projector();
            for todo in dom.children_of_kind(root.id, ElementKind::Todo) {
                let state = todo.str_attr(attr::STATE).and_then(TodoState::parse).unwrap_or(TodoState::Pending);
                let priority = todo.str_attr(X_PRIORITY).map(str::to_owned);
                projector.adopt_todo(todo.id, todo.text.clone(), state, priority);
            }
            if let Some(mode) = root.str_attr(X_MODE) {
                projector.adopt_mode(mode.to_owned());
            }
        }
        let setup = self.load(&config, &shared, native_id).await;
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
