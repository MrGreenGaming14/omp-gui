//! A harness without a process for app tests: every prompt is answered at
//! once, and the hooks the app drives (host tools, models, effort, modes,
//! decisions, import/export) are switched on with builder flags.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use loa_contracts::harness::{
    Adapter, AdapterError, Answer, Availability, Capabilities, HarnessModel, HarnessSession, ModelSelection, Prompt, Result,
    SessionConfig, SessionState,
};
use loa_core::{DecisionKind, DecisionStatus, Dom, Draft, ElementKind, NodeId, PatchSink, Role, SessionMode, TurnStatus, Value, attr, now_ms};
use loa_util::LockExt;

/// The fields of the `ask form` decision.
pub const FORM: &str = r#"{"type":"object","properties":{"name":{"type":"string","description":"Release name"},"channel":{"enum":["beta","stable"]}},"required":["name","channel"]}"#;

/// What every session of a [`ScriptedAdapter`] was asked to do.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub configs: Vec<SessionConfig>,
    pub prompts: Vec<String>,
    pub models: Vec<String>,
    pub efforts: Vec<String>,
    pub modes: Vec<SessionMode>,
    pub compacts: Vec<Option<String>>,
    pub interrupts: usize,
    pub resumes: usize,
}

/// The scripted harness. A prompt `ask [question|multi|plan|form]` waits on
/// a decision of that kind (an approval otherwise) until it is answered;
/// `tool <name> <json>` calls the session's host tool and replies with its
/// output; any other prompt gets the reply `ok`.
#[derive(Clone)]
pub struct ScriptedAdapter {
    id: &'static str,
    capabilities: Capabilities,
    availability: Option<Availability>,
    resumes: bool,
    effort: bool,
    models: Vec<HarnessModel>,
    seen: Arc<Mutex<Seen>>,
}

impl ScriptedAdapter {
    pub fn new(id: &'static str) -> Self {
        let capabilities = Capabilities::builder().build();
        Self { id, capabilities, availability: None, resumes: true, effort: false, models: Vec::new(), seen: Arc::default() }
    }

    pub fn capabilities(self, capabilities: Capabilities) -> Self {
        Self { capabilities, ..self }
    }

    /// What `availability` reports; unknown by default.
    pub fn availability(self, availability: Availability) -> Self {
        Self { availability: Some(availability), ..self }
    }

    /// `resume` fails as unsupported.
    pub fn no_resume(self) -> Self {
        Self { resumes: false, ..self }
    }

    /// Sessions list these models and switch to any of them.
    pub fn models(self, models: &[(&str, &[&str])]) -> Self {
        let models = models
            .iter()
            .map(|(id, efforts)| HarnessModel {
                id: (*id).to_owned(),
                name: (*id).to_owned(),
                provider: id.split('/').next().unwrap_or_default().to_owned(),
                efforts: efforts.iter().map(|effort| (*effort).to_owned()).collect(),
            })
            .collect();
        Self { models, ..self }
    }

    /// Sessions change their effort mid-session.
    pub fn effort(self) -> Self {
        Self { effort: true, ..self }
    }

    pub fn seen(&self) -> Seen {
        self.seen.locked().clone()
    }

    fn session(&self, config: SessionConfig, sink: Arc<dyn PatchSink>, root: NodeId) -> Box<dyn HarnessSession> {
        // Without the handler: what it routes to may hold the session open.
        self.seen.locked().configs.push(SessionConfig { host_tool_handler: None, ..config.clone() });
        Box::new(ScriptedSession { adapter: self.clone(), config, sink, root, waiting: Mutex::new(None) })
    }
}

#[async_trait]
impl Adapter for ScriptedAdapter {
    fn harness(&self) -> &'static str {
        self.id
    }

    async fn availability(&self) -> Availability {
        self.availability.clone().unwrap_or_else(|| Availability::ready(None, Some("1.0".into())))
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let mut attrs: Vec<(&str, Value)> = vec![
            (attr::ID, config.session_id.as_str().into()),
            (attr::NODE, config.node.as_str().into()),
            (attr::HARNESS, self.id.into()),
            (attr::CREATED, now_ms().into()),
            (attr::CWD, config.cwd.display().to_string().into()),
            (attr::NATIVE_ID, format!("native-{}", config.session_id).into()),
        ];
        // As the real adapters do through `kit::ledger::insert_root`.
        attrs.extend(config.project.as_deref().map(|project| (attr::PROJECT, Value::from(project))));
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?.node;
        Ok(self.session(config, sink, root))
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        if !self.resumes {
            return Err(AdapterError::Unsupported { harness: self.id, what: "resume" });
        }
        self.seen.locked().resumes += 1;
        let root = dom.root().map(|root| root.id).ok_or_else(|| AdapterError::Format("empty DOM".into()))?;
        Ok(self.session(config, sink, root))
    }

    /// A native file is one user message per line.
    fn import(&self, native: &Path, sink: &dyn PatchSink) -> Result<()> {
        let text = std::fs::read_to_string(native).map_err(|error| AdapterError::Format(error.to_string()))?;
        let attrs: [(&str, Value); 3] = [(attr::HARNESS, self.id.into()), (attr::CREATED, now_ms().into()), (attr::TITLE, "Imported".into())];
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?.node;
        for line in text.lines() {
            let turn = sink.commit(Draft::insert(root, ElementKind::Turn, [(attr::STATUS, Value::from(TurnStatus::Done))], ""))?.node;
            sink.commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from(Role::User))], line))?;
        }
        Ok(())
    }

    fn export(&self, dom: &Dom, out: &Path) -> Result<()> {
        let lines: Vec<&str> = dom.iter().filter(|node| node.kind == ElementKind::Message).map(|node| node.text.as_str()).collect();
        std::fs::write(out, lines.join("\n")).map_err(|error| AdapterError::Format(error.to_string()))
    }
}

struct ScriptedSession {
    adapter: ScriptedAdapter,
    config: SessionConfig,
    sink: Arc<dyn PatchSink>,
    root: NodeId,
    /// The turn waiting on a decision.
    waiting: Mutex<Option<NodeId>>,
}

impl ScriptedSession {
    fn end(&self, turn: NodeId, reply: &str) -> Result<()> {
        let attrs: [(&str, Value); 1] = [(attr::ROLE, Role::Assistant.into())];
        self.sink.commit(Draft::insert(turn, ElementKind::Message, attrs, reply))?;
        self.sink.commit(Draft::set(turn, attr::ENDED, now_ms()))?;
        self.sink.commit(Draft::set(turn, attr::STATUS, TurnStatus::Done))?;
        Ok(())
    }

    fn ask(&self, turn: NodeId, kind: &str) -> Result<()> {
        let options = |labels: &[&str]| (attr::OPTIONS, Value::List(labels.iter().map(|label| Value::from(*label)).collect()));
        let (kind, title, body, mut attrs): (DecisionKind, &str, &str, Vec<(&str, Value)>) = match kind {
            "question" => (DecisionKind::Question, "Color", "Which color?", vec![options(&["red", "green"])]),
            "multi" => (DecisionKind::Question, "Languages", "Which languages?", vec![options(&["rust", "go", "zig"]), (attr::MULTI, true.into())]),
            "plan" => (DecisionKind::Plan, "Plan", "# Plan\n\n1. Read the code\n2. Fix the bug", Vec::new()),
            "form" => (DecisionKind::Form, "Release", "Release settings", vec![(attr::FIELDS, FORM.into())]),
            _ => (DecisionKind::Approval, "Run ls", "ls", Vec::new()),
        };
        attrs.extend([(attr::KIND, kind.into()), (attr::STATUS, DecisionStatus::Waiting.into()), (attr::TITLE, title.into())]);
        self.sink.commit(Draft::insert(turn, ElementKind::Decision, attrs, body))?;
        self.sink.commit(Draft::set(turn, attr::STATUS, TurnStatus::Waiting))?;
        *self.waiting.locked() = Some(turn);
        Ok(())
    }
}

#[async_trait]
impl HarnessSession for ScriptedSession {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        self.adapter.seen.locked().prompts.push(prompt.text.clone());
        let started: [(&str, Value); 2] = [(attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, now_ms().into())];
        let turn = self.sink.commit(Draft::insert(self.root, ElementKind::Turn, started, ""))?.node;
        let by: [(&str, Value); 2] = [(attr::ROLE, Role::User.into()), (attr::BY, prompt.by.as_str().into())];
        self.sink.commit(Draft::insert(turn, ElementKind::Message, by, prompt.text.as_str()))?;
        if let Some(kind) = prompt.text.strip_prefix("ask") {
            return self.ask(turn, kind.trim());
        }
        if let Some(call) = prompt.text.strip_prefix("tool ") {
            let (tool, args) = call.split_once(' ').unwrap_or((call, "{}"));
            let reply = match &self.config.host_tool_handler {
                Some(tools) => tools.call(&self.config.session_id, tool, args).await.unwrap_or_else(|error| format!("failed: {error}")),
                None => "failed: no host tools".to_owned(),
            };
            return self.end(turn, &reply);
        }
        self.end(turn, "ok")
    }

    async fn interrupt(&self) -> Result<()> {
        self.adapter.seen.locked().interrupts += 1;
        Ok(())
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        let turn = self.waiting.locked().take().ok_or(AdapterError::UnknownDecision(decision))?;
        let values = Value::List(answer.values.into_iter().map(Value::from).collect());
        self.sink.commit(Draft::set(decision, attr::ANSWER, values))?;
        self.sink.commit(Draft::set(decision, attr::BY, answer.by))?;
        if let Some(reason) = answer.reason {
            self.sink.commit(Draft::set(decision, attr::REASON, reason))?;
        }
        self.sink.commit(Draft::set(decision, attr::STATUS, DecisionStatus::Answered))?;
        self.sink.commit(Draft::set(turn, attr::STATUS, TurnStatus::Running))?;
        self.end(turn, "ok")
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        if !self.adapter.models.iter().any(|model| model.id == selection.model) {
            return Err(AdapterError::Protocol(format!("unknown model {}", selection.model)));
        }
        self.sink.commit(Draft::set(self.root, attr::MODEL, selection.model.as_str()))?;
        self.adapter.seen.locked().models.push(selection.model);
        Ok(())
    }

    async fn set_mode(&self, mode: SessionMode) -> Result<()> {
        self.sink.commit(Draft::set(self.root, attr::MODE, mode.as_str()))?;
        self.adapter.seen.locked().modes.push(mode);
        Ok(())
    }

    async fn models(&self) -> Result<Vec<HarnessModel>> {
        if self.adapter.models.is_empty() {
            return Err(AdapterError::Unsupported { harness: self.adapter.id, what: "listing models" });
        }
        Ok(self.adapter.models.clone())
    }

    async fn set_effort(&self, effort: String) -> Result<()> {
        if !self.adapter.effort {
            return Err(AdapterError::Unsupported { harness: self.adapter.id, what: "set_effort" });
        }
        self.sink.commit(Draft::set(self.root, attr::EFFORT, effort.as_str()))?;
        self.adapter.seen.locked().efforts.push(effort);
        Ok(())
    }

    async fn compact(&self, instructions: Option<String>) -> Result<()> {
        self.adapter.seen.locked().compacts.push(instructions);
        Ok(())
    }

    fn harness(&self) -> &'static str {
        self.adapter.id
    }

    fn state(&self) -> SessionState {
        SessionState::Idle
    }

    async fn wait_idle(&self) -> Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
