//! The omp [`Adapter`]: an RPC process per session, its frames fed to the [`Mapper`].

use super::launch::{OmpAdapterConfig, Target, drop_unavailable_tools, spawn_options, split_selector};
use super::mapper::{Input, Mapper, Reply};
use super::native::{self, X_SESSION_FILE};
use super::rpc::{Frame, HostHandler, HostToolCall, HostToolDefinition, HostToolOutput, ImageContent, Incoming, MessageUpdates, ModelInfo, PromptStatus, RpcClient, RpcError, StreamingBehavior, SubagentLevel, ToolContent, UiResponder};
use super::rpc::Event;
use super::HARNESS;
use crate::kit::{EventMapper, insert_root};
use async_trait::async_trait;
use base64::Engine;
use loa_contracts::harness::{Adapter, AdapterError, Answer, Availability, Capabilities, ContextDelivery, Feature, HarnessModel, HarnessSession, HostTools, ModelSelection, ModelSwitch, OutputBlock, Prompt, Result, SessionConfig, SessionState};
use loa_core::{DecisionKind, Dom, DomWriter, NodeId, PatchSink, SCHEMA_VERSION, attr};
use loa_util::LockExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

fn rpc_error(error: RpcError) -> AdapterError {
    match error {
        RpcError::Version { offered, min, max } => AdapterError::Version { harness: HARNESS, detail: format!("RPC versions {offered:?}; supported {min}..={max}") },
        other => other.into(),
    }
}

fn base64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Serves the session's host tools to omp.
struct AppHost {
    session_id: String,
    handler: Arc<dyn HostTools>,
}

#[async_trait]
impl HostHandler for AppHost {
    async fn tool_call(&self, call: HostToolCall) -> HostToolOutput {
        match self.handler.call_content(&self.session_id, &call.tool_name, &call.arguments.to_string()).await {
            Ok(content) => HostToolOutput::blocks(
                content
                    .into_iter()
                    .map(|block| match block {
                        OutputBlock::Text(text) => ToolContent::text(text),
                        OutputBlock::Image { mime, data } => ToolContent::Image(ImageContent { data: base64(&data), mime_type: mime }),
                    })
                    .collect(),
            ),
            Err(error) => HostToolOutput::error(error),
        }
    }
}

/// The omp harness adapter.
#[derive(Clone, Debug)]
pub struct OmpAdapter {
    pub config: OmpAdapterConfig,
}

impl OmpAdapter {
    pub fn new(config: OmpAdapterConfig) -> Self {
        Self { config }
    }

    async fn launch(&self, config: SessionConfig, target: Target, sink: Arc<dyn PatchSink>, root: NodeId) -> Result<OmpSession> {
        let mut options = spawn_options(&self.config, &config, &target)?;
        if !config.host_tools.is_empty() {
            let handler = config.host_tool_handler.clone().ok_or_else(|| AdapterError::Protocol("host tools require a handler".into()))?;
            options.host = Some(Arc::new(AppHost { session_id: config.session_id.clone(), handler }));
        }
        // Each retry drops at least one tool, so this ends.
        let (client, mut events) = loop {
            match RpcClient::spawn(options.clone()).await {
                Err(RpcError::Exited { stderr, .. }) if drop_unavailable_tools(&mut options, &stderr) => {}
                spawned => break spawned.map_err(rpc_error)?,
            }
        };
        let setup = async {
            client.set_event_filter(None, Some(MessageUpdates::Delta)).await?;
            client.set_subagent_subscription(SubagentLevel::Events).await?;
            client.set_ask_dialog(true).await?;
            if !config.host_tools.is_empty() {
                let tools = config.host_tools.iter().map(|tool| {
                    let parameters = serde_json::from_str(&tool.input_schema)?;
                    Ok(HostToolDefinition { name: tool.name.clone(), label: None, description: tool.description.clone(), parameters, hidden: None, load_mode: Some("essential".into()) })
                });
                client.set_host_tools(tools.collect::<std::result::Result<_, RpcError>>()?).await?;
            }
            client.get_state().await
        };
        let state = match setup.await {
            Ok(state) => state,
            Err(error) => {
                let _ = client.shutdown(self.config.shutdown_grace).await;
                return Err(rpc_error(error));
            }
        };
        let w = DomWriter::new(&*sink);
        if !state.session_id.is_empty() {
            w.set(root, attr::NATIVE_ID, state.session_id.as_str())?;
        }
        if let Some(path) = state.session_file.as_deref() {
            w.set(root, X_SESSION_FILE, path)?;
        }
        if let Some(model) = &state.model {
            w.set(root, attr::MODEL, model.selector())?;
        }
        let initial = if state.is_streaming { SessionState::Running } else { SessionState::Idle };
        let shared = Arc::new(Shared { sink, mapper: Mutex::new(Mapper::new(root)), state: watch::Sender::new(initial), failure: Mutex::new(None) });
        let (reader, responder) = (shared.clone(), client.ui_responder());
        tokio::spawn(async move {
            while let Some(incoming) = events.recv().await {
                let frame = match incoming {
                    Incoming::Event(event) => Frame::Event(event),
                    Incoming::Ui(request) => Frame::Ui(request),
                    Incoming::Response(_) => continue,
                    Incoming::Exited { status, stderr } => return reader.fail(format!("omp exited ({status:?}): {stderr}")),
                };
                if let Err(error) = reader.feed(Input::Frame(frame), Some(&responder)).await {
                    return reader.fail(error.to_string());
                }
            }
        });
        Ok(OmpSession { client, shared, shutdown_grace: self.config.shutdown_grace })
    }
}

#[async_trait]
impl Adapter for OmpAdapter {
    fn harness(&self) -> &'static str {
        HARNESS
    }

    async fn availability(&self) -> Availability {
        Availability::probe(&self.config.executable, &["--version"]).await
    }

    fn capabilities(&self) -> Capabilities {
        let features = [Feature::Rewind, Feature::Compaction, Feature::Subagents, Feature::OmpTools, Feature::AdvisorRules];
        let decisions = [DecisionKind::Approval, DecisionKind::Question, DecisionKind::Plan, DecisionKind::Form];
        let builder = Capabilities::builder().schema(SCHEMA_VERSION).features(&features).model_switch(ModelSwitch::PerCall).decisions(&decisions);
        builder.context(&[ContextDelivery::SystemPrompt]).host_tools().queue_mid_turn().effort().import().export().build()
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let root = insert_root(&DomWriter::new(&*sink), HARNESS, &config)?;
        Ok(Box::new(self.launch(config, Target::New, sink, root).await?))
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let (root, file) = session_file(dom)?;
        Ok(Box::new(self.launch(config, Target::Resume(file), sink, root).await?))
    }

    async fn fork(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let (root, file) = session_file(dom)?;
        let session = self.launch(config, Target::Resume(file), sink, root).await?;
        if session.client.fork(None).await.map_err(rpc_error)?.cancelled {
            return Err(AdapterError::Protocol("omp cancelled fork".into()));
        }
        if let Some(path) = session.client.get_state().await.map_err(rpc_error)?.session_file {
            DomWriter::new(&*session.shared.sink).set(root, X_SESSION_FILE, path)?;
        }
        Ok(Box::new(session))
    }

    fn import(&self, native_path: &Path, sink: &dyn PatchSink) -> Result<()> {
        native::import(native_path, sink, &self.config.node)?;
        // The app resumes from this file; the JSONL header stays unchanged.
        // An import starts from an empty DOM, whose first node has id 1.
        Ok(DomWriter::new(sink).set(NodeId(1), X_SESSION_FILE, native_path.to_string_lossy().into_owned())?)
    }

    fn export(&self, dom: &Dom, out: &Path) -> Result<()> {
        Ok(std::fs::write(out, native::export_string(dom)?)?)
    }
}

/// Inserts the `<session>` root of a new session started with `config`.
fn session_file(dom: &Dom) -> Result<(NodeId, String)> {
    let root = dom.root().ok_or_else(|| AdapterError::Format("missing session root".into()))?;
    let file = root.str_attr(X_SESSION_FILE).ok_or_else(|| AdapterError::Format(format!("resuming omp needs the native session file in {X_SESSION_FILE}")))?;
    Ok((root.id, file.to_owned()))
}

/// A model as the picker lists it; efforts from omp's `thinking.efforts`
/// (e.g. low/medium/high/xhigh/max), none when the model doesn't reason.
fn harness_model(model: &ModelInfo) -> HarnessModel {
    let efforts = model.extra.get("thinking").and_then(|t| t.get("efforts")).and_then(serde_json::Value::as_array).filter(|_| model.reasoning);
    let efforts = efforts.into_iter().flatten().filter_map(|l| l.as_str().map(str::to_owned)).collect();
    let name = if model.name.is_empty() { model.id.clone() } else { model.name.clone() };
    HarnessModel { id: model.selector(), name, provider: model.provider.clone(), efforts }
}

struct Shared {
    sink: Arc<dyn PatchSink>,
    mapper: Mutex<Mapper>,
    state: watch::Sender<SessionState>,
    failure: Mutex<Option<String>>,
}

impl Shared {
    /// Applies `input` and answers the dialogs the mapper answers itself.
    async fn feed(&self, input: Input, responder: Option<&UiResponder>) -> Result<()> {
        let replies = {
            let mut mapper = self.mapper.locked();
            let replies = mapper.apply(input, &DomWriter::new(&*self.sink));
            if *self.state.borrow() != SessionState::Closed {
                self.state.send_replace(mapper.state());
            }
            replies?
        };
        for Reply { response, .. } in replies {
            if let Some(responder) = responder {
                responder.respond(&response).await.map_err(rpc_error)?;
            }
        }
        Ok(())
    }

    fn fail(&self, message: String) {
        *self.failure.locked() = Some(message);
        self.state.send_replace(SessionState::Closed);
    }

    fn closed(&self) -> AdapterError {
        self.failure.locked().clone().map_or(AdapterError::Closed, AdapterError::Protocol)
    }

    fn write(&self, set: impl FnOnce(&DomWriter, NodeId) -> std::result::Result<(), loa_core::CommitError>) -> Result<()> {
        let session = self.mapper.locked().session();
        Ok(set(&DomWriter::new(&*self.sink), session)?)
    }
}

/// A running omp session. Its client can be shared with model and provider UI.
pub struct OmpSession {
    client: RpcClient,
    shared: Arc<Shared>,
    shutdown_grace: Duration,
}

impl OmpSession {
    pub fn client(&self) -> &RpcClient {
        &self.client
    }

    fn open(&self) -> Result<()> {
        match self.state() {
            SessionState::Closed => Err(self.shared.closed()),
            _ => Ok(()),
        }
    }
}

#[async_trait]
impl HarnessSession for OmpSession {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        self.open()?;
        // omp's prompt carries images only. Other mentioned files stay the
        // `@path` reference the prompt text already holds.
        let images = prompt.attachments.iter().filter_map(|attachment| {
            let mime = attachment.mime.as_deref().filter(|mime| mime.starts_with("image/"))?;
            Some(std::fs::read(&attachment.path).map(|data| ImageContent { data: base64(&data), mime_type: mime.into() }))
        });
        let images = images.collect::<std::io::Result<Vec<_>>>()?;
        let id = self.client.reserve_id();
        self.shared.feed(Input::Sent { id: id.clone(), prompt: prompt.clone() }, None).await?;
        // As a follow-up, omp runs it at once when idle and after the running work otherwise.
        let ack = match self.client.prompt_as(id.clone(), prompt.text, images, Some(StreamingBehavior::FollowUp)).await {
            Ok(ack) => ack,
            Err(error) => {
                let mut mapper = self.shared.mapper.locked();
                mapper.withdraw(&id);
                if *self.shared.state.borrow() != SessionState::Closed {
                    self.shared.state.send_replace(mapper.state());
                }
                return Err(rpc_error(error));
            }
        };
        if ack.agent_invoked == Some(false) {
            let settled = Event::PromptResult { id: Some(id), agent_invoked: false, status: PromptStatus::Completed, error: None, session_settled: true };
            self.shared.feed(Input::Frame(Frame::Event(settled)), None).await?;
        }
        Ok(())
    }

    /// Asks omp to abort, withdrawing queued follow-ups (a plain `abort`
    /// leaves them queued and unsettled); each `prompt_result` then cancels its turn.
    async fn interrupt(&self) -> Result<()> {
        self.open()?;
        if self.state() != SessionState::Idle {
            self.client.abort_and_restore_queue().await.map_err(rpc_error)?;
        }
        Ok(())
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        self.open()?;
        let reply = {
            let mut mapper = self.shared.mapper.locked();
            let reply = mapper.answer(decision, &answer, &DomWriter::new(&*self.shared.sink));
            self.shared.state.send_replace(mapper.state());
            reply?
        };
        let Some(Reply { response, steer }) = reply else { return Ok(()) };
        self.client.respond_ui(&response).await.map_err(rpc_error)?;
        if let Some(steer) = steer {
            self.client.steer(steer).await.map_err(rpc_error)?;
        }
        Ok(())
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        self.open()?;
        if selection.role.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "model role updates over RPC" });
        }
        let (provider, model) = split_selector(&selection.model)?;
        let model = self.client.set_model(provider, model).await.map_err(rpc_error)?;
        self.shared.write(|w, session| w.set(session, attr::MODEL, model.selector()))
    }

    async fn models(&self) -> Result<Vec<HarnessModel>> {
        self.open()?;
        Ok(self.client.get_available_models().await.map_err(rpc_error)?.iter().map(harness_model).collect())
    }

    async fn set_effort(&self, effort: String) -> Result<()> {
        self.open()?;
        self.client.set_thinking_level(effort.as_str()).await.map_err(rpc_error)?;
        self.shared.write(|w, session| w.set(session, attr::EFFORT, effort))
    }

    async fn compact(&self, instructions: Option<String>) -> Result<()> {
        self.open()?;
        self.client.compact(instructions).await.map_err(rpc_error).map(drop)
    }

    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn state(&self) -> SessionState {
        *self.shared.state.borrow()
    }

    async fn wait_idle(&self) -> Result<()> {
        let mut state = self.shared.state.subscribe();
        match state.wait_for(|s| matches!(s, SessionState::Idle | SessionState::Closed)).await.map(|s| *s) {
            Ok(SessionState::Idle) => Ok(()),
            _ => Err(self.shared.closed()),
        }
    }

    async fn shutdown(&self) -> Result<()> {
        self.client.shutdown(self.shutdown_grace).await.map_err(rpc_error)?;
        self.shared.state.send_replace(SessionState::Closed);
        Ok(())
    }
}
