//! Connects the harness contract, the RPC process and the DOM event mapper.

use crate::launch::{OmpAdapterConfig, Target, spawn_options, split_selector};
use crate::mapping::Mapping;
use crate::native;
use async_trait::async_trait;
use base64::Engine;
use loa_adapter::{Adapter, AdapterError, Answer, Capabilities, ContextDelivery, Feature, HarnessSession, HostTools, ModelSelection, Prompt, Result, SessionConfig, SessionState};
use loa_core::{DecisionKind, Draft, ElementKind, NodeId, PatchSink, SCHEMA_VERSION, SessionMode, Value, attr, now_ms};
use loa_omp_rpc::{Event, HostHandler, HostToolCall, HostToolDefinition, HostToolOutput, HostUriRequest, HostUriOutput, ImageContent, Incoming, MessageUpdates, RpcClient, RpcError, SubagentLevel, UiRequest, UiResponse};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

fn rpc_error(error: RpcError) -> AdapterError {
    match error {
        RpcError::NotFound(path) => AdapterError::NotInstalled(path),
        RpcError::Version { offered, min, max } => AdapterError::Version {
            harness: "omp", detail: format!("RPC versions {offered:?}; supported {min}..={max}"),
        },
        RpcError::Io(error) => AdapterError::Io(error),
        other => AdapterError::Protocol(other.to_string()),
    }
}

fn commit(sink: &dyn PatchSink, mapper: &mut Mapping, drafts: Vec<Draft>) -> Result<()> {
    let mut pending = drafts;
    while !pending.is_empty() {
        let mut next = Vec::new();
        for draft in pending {
            let patch = sink.commit(draft)?;
            next.extend(mapper.committed(&[patch]));
        }
        pending = next;
    }
    Ok(())
}

struct AppHost {
    session_id: String,
    handler: Arc<dyn HostTools>,
}

#[async_trait]
impl HostHandler for AppHost {
    async fn tool_call(&self, call: HostToolCall) -> HostToolOutput {
        match self.handler.call(&self.session_id, &call.tool_name, &call.arguments.to_string()).await {
            Ok(text) => HostToolOutput::ok(text),
            Err(error) => HostToolOutput::error(error),
        }
    }

    async fn uri_request(&self, request: HostUriRequest) -> HostUriOutput {
        HostUriOutput::Error(format!("unregistered host URI: {}", request.url))
    }
}

/// omp harness adapter. Construct once with writable app state and a node key.
#[derive(Clone, Debug)]
pub struct OmpAdapter {
    pub config: OmpAdapterConfig,
}

impl OmpAdapter {
    pub fn new(config: OmpAdapterConfig) -> Self { Self { config } }

    async fn launch(&self, config: SessionConfig, target: Target, sink: Arc<dyn PatchSink>, root: NodeId) -> Result<OmpSession> {
        let mut options = spawn_options(&self.config, &config, &target)?;
        if !config.host_tools.is_empty() {
            let handler = config.host_tool_handler.as_ref().ok_or_else(|| AdapterError::Protocol("host tools require a handler".into()))?;
            options.host = Some(Arc::new(AppHost { session_id: config.session_id.clone(), handler: handler.clone() }));
        }
        let (client, mut events) = RpcClient::spawn(options).await.map_err(rpc_error)?;
        let setup = async {
            client.set_event_filter(None, Some(MessageUpdates::Delta)).await.map_err(rpc_error)?;
            client.set_subagent_subscription(SubagentLevel::Events).await.map_err(rpc_error)?;
            client.set_ask_dialog(true).await.map_err(rpc_error)?;
            if !config.host_tools.is_empty() {
                let tools = config.host_tools.iter().map(|tool| {
                    let parameters = serde_json::from_str(&tool.input_schema).map_err(|e| AdapterError::Format(format!("host tool {} schema: {e}", tool.name)))?;
                    Ok(HostToolDefinition { name: tool.name.clone(), label: None, description: tool.description.clone(), parameters, hidden: None, load_mode: Some("essential".into()) })
                }).collect::<Result<Vec<_>>>()?;
                client.set_host_tools(tools).await.map_err(rpc_error)?;
            }
            let state = client.get_state().await.map_err(rpc_error)?;
            Ok::<_, AdapterError>(state)
        }.await;
        let state = match setup {
            Ok(state) => state,
            Err(error) => { let _ = client.shutdown(self.config.shutdown_grace).await; return Err(error); }
        };
        if !state.session_id.is_empty() {
            sink.commit(Draft::set(root, attr::NATIVE_ID, state.session_id.as_str()))?;
        }
        if let Some(path) = state.session_file.as_deref() {
            sink.commit(Draft::set(root, "x-omp-session-file", path))?;
        }
        let shared = Arc::new(Shared {
            sink,
            mapper: Mutex::new(Mapping::new(root)),
            status: Mutex::new(if state.is_streaming { SessionState::Running } else { SessionState::Idle }),
            idle: Notify::new(),
            failure: Mutex::new(None),
        });
        let background = shared.clone();
        tokio::spawn(async move {
            while let Some(incoming) = events.recv().await {
                let mapped = match incoming {
                    Incoming::Event(event) => {
                        let mut mapper = background.mapper.lock().await;
                        let drafts = mapper.event(&event);
                        let result = commit(&*background.sink, &mut mapper, drafts);
                        if matches!(event, Event::SessionSettled | Event::PromptResult { session_settled: true, .. }) {
                            *background.status.lock().await = SessionState::Idle;
                            background.idle.notify_waiters();
                        }
                        result
                    }
                    Incoming::Ui(request) => {
                        let mut mapper = background.mapper.lock().await;
                        let drafts = mapper.ui(&request);
                        let result = commit(&*background.sink, &mut mapper, drafts);
                        if matches!(request, UiRequest::Select { .. } | UiRequest::Confirm { .. } | UiRequest::Input { .. } | UiRequest::Editor { .. } | UiRequest::Ask { .. }) {
                            *background.status.lock().await = SessionState::Waiting;
                        }
                        result
                    }
                    Incoming::Response(_) => Ok(()),
                    Incoming::Exited { status, stderr } => {
                        let message = format!("omp exited ({status:?}): {stderr}");
                        *background.failure.lock().await = Some(message);
                        *background.status.lock().await = SessionState::Closed;
                        background.idle.notify_waiters();
                        break;
                    }
                };
                if let Err(error) = mapped {
                    *background.failure.lock().await = Some(error.to_string());
                    *background.status.lock().await = SessionState::Closed;
                    background.idle.notify_waiters();
                    break;
                }
            }
        });
        Ok(OmpSession { client, shared, shutdown_grace: self.config.shutdown_grace })
    }
}

 

#[async_trait]
impl Adapter for OmpAdapter {
    fn harness(&self) -> &'static str { "omp" }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: SCHEMA_VERSION,
            features: vec![Feature::Rewind, Feature::Compaction, Feature::Subagents, Feature::OmpTools, Feature::AdvisorRules],
            model_switch: loa_adapter::ModelSwitch::PerCall,
            decisions: vec![DecisionKind::Approval, DecisionKind::Question, DecisionKind::Plan, DecisionKind::Form],
            context_delivery: vec![ContextDelivery::SystemPrompt],
            host_tools: true,
            mcp_servers: false,
            queue_mid_turn: true,
            effort: true,
            import: true,
            export: true,
        }
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let mut attrs = vec![(attr::ID, Value::from(config.session_id.as_str())), (attr::HARNESS, Value::from("omp")), (attr::NODE, Value::from(config.node.as_str())), (attr::CREATED, Value::Int(now_ms())), (attr::SCHEMA, Value::Int(i64::from(SCHEMA_VERSION))), (attr::CWD, Value::from(config.cwd.to_string_lossy().as_ref()))];
        if let Some(project) = &config.project { attrs.push((attr::PROJECT, project.as_str().into())); }
        if let Some(model) = &config.model { attrs.push((attr::MODEL, model.as_str().into())); }
        if let Some(role) = &config.model_role { attrs.push((attr::MODEL_ROLE, role.as_str().into())); }
        if let Some(effort) = &config.effort { attrs.push((attr::EFFORT, effort.as_str().into())); }
        if let Some(mode) = config.mode { attrs.push((attr::MODE, mode.into())); }
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?.node;
        let session = self.launch(config, Target::New, sink, root).await?;
        Ok(Box::new(session))
    }

    async fn resume(&self, config: SessionConfig, dom: &loa_core::Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let root = dom.root().ok_or_else(|| AdapterError::Format("missing session root".into()))?;
        let target = root.attrs.get("x-omp-session-file").and_then(Value::as_str).ok_or_else(|| AdapterError::Format("resuming omp needs native session file in x-omp-session-file".into()))?;
        let session = self.launch(config, Target::Resume(target.to_owned()), sink, root.id).await?;
        Ok(Box::new(session))
    }

    async fn fork(&self, config: SessionConfig, dom: &loa_core::Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let root = dom.root().ok_or_else(|| AdapterError::Format("missing forked session root".into()))?;
        let source = root.attrs.get("x-omp-session-file").and_then(Value::as_str)
            .ok_or_else(|| AdapterError::Format("fork needs native session file in x-omp-session-file".into()))?;
        let session = self.launch(config, Target::Resume(source.to_owned()), sink, root.id).await?;
        let result = session.client.fork(None).await.map_err(rpc_error)?;
        if result.cancelled { return Err(AdapterError::Protocol("omp cancelled fork".into())); }
        let state = session.client.get_state().await.map_err(rpc_error)?;
        if let Some(path) = state.session_file {
            session.shared.sink.commit(Draft::set(root.id, "x-omp-session-file", path))?;
        }
        Ok(Box::new(session))
    }

    fn import(&self, native_path: &Path, sink: &dyn PatchSink) -> Result<()> {
        native::import(native_path, sink, &self.config.node)?;
        // Native path is an app-owned resumption hint; the original JSONL header remains unchanged.
        let root = sink_root_after_import(native_path, sink)?;
        sink.commit(Draft::set(root, "x-omp-session-file", native_path.to_string_lossy().into_owned()))?;
        Ok(())
    }

    fn export(&self, dom: &loa_core::Dom, out: &Path) -> Result<()> { native::export(dom, out) }
}

fn sink_root_after_import(_path: &Path, _sink: &dyn PatchSink) -> Result<NodeId> {
    // `Adapter::import` requires an empty DOM, whose first inserted node has id 1.
    Ok(NodeId(1))
}

struct Shared {
    sink: Arc<dyn PatchSink>,
    mapper: Mutex<Mapping>,
    status: Mutex<SessionState>,
    idle: Notify,
    failure: Mutex<Option<String>>,
}

/// A running omp session. Its client can be shared with model and provider UI.
pub struct OmpSession {
    client: RpcClient,
    shared: Arc<Shared>,
    shutdown_grace: Duration,
}

impl OmpSession {
    pub fn client(&self) -> &RpcClient { &self.client }
    pub fn protocol_version(&self) -> u32 { self.client.protocol_version() }

    async fn error_if_closed(&self) -> Result<()> {
        if self.client.has_exited() {
            return Err(AdapterError::Protocol(self.shared.failure.lock().await.clone().unwrap_or_else(|| "omp exited".into())));
        }
        if *self.shared.status.lock().await == SessionState::Closed { return Err(AdapterError::Closed); }
        Ok(())
    }
}

#[async_trait]
impl HarnessSession for OmpSession {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        self.error_if_closed().await?;
        let images = prompt.attachments.iter().map(|attachment| {
            let data = std::fs::read(&attachment.path)?;
            let mime = attachment.mime.as_deref().ok_or_else(|| AdapterError::Format(format!("image {} needs MIME type", attachment.path.display())))?;
            Ok(ImageContent { data: base64::engine::general_purpose::STANDARD.encode(data), mime_type: mime.into() })
        }).collect::<Result<Vec<_>>>()?;
        let ack = self.client.prompt(prompt.text.clone(), images, None).await.map_err(rpc_error)?;
        let mut mapper = self.shared.mapper.lock().await;
        let drafts = mapper.sent(&prompt);
        commit(&*self.shared.sink, &mut mapper, drafts)?;
        if ack.agent_invoked == Some(false) {
            let drafts = mapper.event(&Event::PromptResult { id: None, agent_invoked: false, status: loa_omp_rpc::PromptStatus::Completed, error: None, session_settled: true });
            commit(&*self.shared.sink, &mut mapper, drafts)?;
            *self.shared.status.lock().await = SessionState::Idle;
            self.shared.idle.notify_waiters();
        } else {
            *self.shared.status.lock().await = SessionState::Running;
        }
        Ok(())
    }

    async fn interrupt(&self) -> Result<()> {
        self.error_if_closed().await?;
        if self.state() != SessionState::Idle {
            self.client.abort().await.map_err(rpc_error)?;
            let mut mapper = self.shared.mapper.lock().await;
            let drafts = mapper.event(&Event::PromptResult { id: None, agent_invoked: true, status: loa_omp_rpc::PromptStatus::Aborted, error: None, session_settled: true });
            commit(&*self.shared.sink, &mut mapper, drafts)?;
            *self.shared.status.lock().await = SessionState::Idle;
            self.shared.idle.notify_waiters();
        }
        Ok(())
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        self.error_if_closed().await?;
        let mut mapper = self.shared.mapper.lock().await;
        let request = mapper.decision(decision).ok_or(AdapterError::UnknownDecision(decision))?;
        let id = request.id().to_owned();
        let response = match request {
            UiRequest::Select { .. } | UiRequest::Input { .. } | UiRequest::Editor { .. } => match answer.values.first() {
                Some(value) => UiResponse::Value { id, value: value.clone() },
                None => UiResponse::Cancelled { id, timed_out: false },
            },
            UiRequest::Confirm { .. } => UiResponse::Confirmed { id, confirmed: answer.values.first().is_some_and(|v| matches!(v.as_str(), "approve" | "allow_once" | "allow_session" | "yes" | "true")) },
            UiRequest::Ask { questions, .. } if answer.values.len() == questions.len() => UiResponse::Answers {
                id,
                answers: questions.iter().zip(&answer.values).map(|(question, value)| loa_omp_rpc::AskAnswer {
                    id: question.id.clone(),
                    selected_options: vec![value.clone()],
                    custom_input: None,
                }).collect(),
            },
            UiRequest::Ask { .. } => UiResponse::Cancelled { id, timed_out: false },
            _ => return Err(AdapterError::UnknownDecision(decision)),
        };
        self.client.respond_ui(&response).await.map_err(rpc_error)?;
        let drafts = mapper.answered(decision, &answer);
        commit(&*self.shared.sink, &mut mapper, drafts)?;
        *self.shared.status.lock().await = SessionState::Running;
        Ok(())
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        self.error_if_closed().await?;
        if selection.role.is_some() { return Err(AdapterError::Unsupported { harness: "omp", what: "model role updates over RPC" }); }
        let (provider, model) = split_selector(&selection.model)?;
        self.client.set_model(provider, model).await.map_err(rpc_error)?;
        Ok(())
    }

    async fn set_mode(&self, _mode: SessionMode) -> Result<()> { Err(AdapterError::Unsupported { harness: "omp", what: "mode changes over RPC" }) }

    async fn compact(&self, instructions: Option<String>) -> Result<()> {
        self.error_if_closed().await?;
        self.client.compact(instructions).await.map_err(rpc_error)?;
        Ok(())
    }

    fn harness(&self) -> &'static str { "omp" }

    fn state(&self) -> SessionState {
        // `try_lock` never blocks an event reader; the last observable stable state is idle.
        self.shared.status.try_lock().map_or(SessionState::Running, |status| *status)
    }

    async fn wait_idle(&self) -> Result<()> {
        loop {
            let notified = self.shared.idle.notified();
            let status = *self.shared.status.lock().await;
            match status {
                SessionState::Idle => return Ok(()),
                SessionState::Closed => return Err(AdapterError::Protocol(self.shared.failure.lock().await.clone().unwrap_or_else(|| "omp closed".into()))),
                _ => notified.await,
            }
        }
    }

    async fn shutdown(&self) -> Result<()> {
        self.client.shutdown(self.shutdown_grace).await.map_err(rpc_error)?;
        *self.shared.status.lock().await = SessionState::Closed;
        self.shared.idle.notify_waiters();
        Ok(())
    }
}
