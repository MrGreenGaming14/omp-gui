//! Pure translation of omp's ordered RPC stream into session DOM drafts.
//! Inserts are acknowledged through `committed` before a child can reference
//! their writer-assigned node id. Callers must commit its returned drafts too.

use loa_adapter::{Answer, Prompt};
use loa_core::{attr, CallStatus, DecisionKind, DecisionStatus, Draft, ElementKind as K, JobStatus, MessageKind, MessageStatus, NodeId, Op, Patch, TodoState, TurnStatus, Value};
use loa_core::schema::TEXT;
use loa_omp_rpc::{AssistantEvent, Event, PromptStatus, SubagentStatus, TodoItem, TodoStatus, UiRequest};
use serde_json::Value as Json;
use std::collections::{HashMap, VecDeque};

#[derive(Debug)]
enum Pending {
    Turn,
    User,
    Message(String),
    Call(String, Option<Json>),
    Input(String),
    Result,
    Job(String),
    Decision(String, UiRequest),
    Todo(String),
    Other,
}

/// IDs are writer-assigned: never infer one from the session id or patch sequence.
#[derive(Debug)]
pub struct Mapping {
    pub session: NodeId,
    pub turn: Option<NodeId>,
    pub calls: HashMap<String, NodeId>,
    pub messages: HashMap<String, NodeId>,
    pub jobs: HashMap<String, NodeId>,
    pub todos: HashMap<String, NodeId>,
    pub decisions: HashMap<String, NodeId>,
    pending: VecDeque<Pending>,
    deferred: VecDeque<Event>,
    deferred_ui: VecDeque<UiRequest>,
    deferred_prompts: VecDeque<Prompt>,
    blocks: HashMap<u32, String>,
    text: HashMap<String, String>,
    inputs: HashMap<String, NodeId>,
    input_text: HashMap<String, String>,
    requests: HashMap<NodeId, UiRequest>,
    turns: Vec<NodeId>,
    current_message: Option<String>,
}

impl Mapping {
    pub fn new(session: NodeId) -> Self {
        Self {
            session, turn: None, calls: HashMap::new(), messages: HashMap::new(),
            jobs: HashMap::new(), todos: HashMap::new(), decisions: HashMap::new(),
            pending: VecDeque::new(), deferred: VecDeque::new(), deferred_ui: VecDeque::new(), deferred_prompts: VecDeque::new(),
            blocks: HashMap::new(), text: HashMap::new(), inputs: HashMap::new(),
            input_text: HashMap::new(), requests: HashMap::new(), turns: Vec::new(),
            current_message: None,
        }
    }

    fn insert(&mut self, parent: NodeId, kind: K, attrs: Vec<(&str, Value)>, text: impl Into<String>, pending: Pending) -> Draft {
        self.pending.push_back(pending);
        Draft::insert(parent, kind, attrs, text.into())
    }

    fn turn_start(&mut self) -> Vec<Draft> {
        if let Some(turn) = self.turn.take() {
            let mut drafts = self.cancelled_for(turn);
            drafts.push(set(turn, attr::STATUS, TurnStatus::Done));
            drafts.push(self.insert(self.session, K::Turn, vec![(attr::STATUS, TurnStatus::Running.into())], "", Pending::Turn));
            drafts
        } else {
            vec![self.insert(self.session, K::Turn, vec![(attr::STATUS, TurnStatus::Running.into())], "", Pending::Turn)]
        }
    }

    pub fn sent(&mut self, prompt: &Prompt) -> Vec<Draft> {
        if let Some(turn) = self.turn {
            vec![self.insert(turn, K::Message, vec![(attr::ROLE, "user".into()), (attr::STATUS, MessageStatus::Done.into()), (attr::BY, prompt.by.clone().into())], prompt.text.clone(), Pending::User)]
        } else {
            // The writer must assign the turn id before inserting its child.
            self.deferred_prompts.push_back(prompt.clone());
            self.turn_start()
        }
    }

    pub fn event(&mut self, event: &Event) -> Vec<Draft> {
        if self.turn.is_none() && needs_turn(event) {
            self.deferred.push_back(event.clone());
            return if self.pending.iter().any(|p| matches!(p, Pending::Turn)) { Vec::new() } else { self.turn_start() };
        }
        match event {
            Event::AgentStart | Event::TurnStart => {
                if self.turn.is_none() && !self.pending.iter().any(|p| matches!(p, Pending::Turn)) { self.turn_start() } else { Vec::new() }
            }
            Event::AgentEnd { .. } | Event::TurnEnd | Event::SessionSettled => self.finish(TurnStatus::Done),
            Event::PromptResult { status, error, .. } => {
                let mut out = match status {
                    PromptStatus::Completed => self.finish(TurnStatus::Done),
                    PromptStatus::Aborted => self.finish(TurnStatus::Cancelled),
                    PromptStatus::Error => self.finish(TurnStatus::Error),
                };
                if let Some(error) = error { out.push(self.ext("prompt_error", &serde_json::json!({"message":error.message,"provider":error.provider,"model":error.model,"code":error.code,"retryable":error.retryable}))); }
                out
            }
            Event::MessageStart { message_id, .. } => { self.current_message = message_id.clone(); Vec::new() }
            Event::MessageUpdate { role, event, message_id } if role == "assistant" => self.assistant(event, message_id.as_deref()),
            Event::MessageEnd { message, message_id } => {
                let key_prefix = message_id.as_deref().or(self.current_message.as_deref()).unwrap_or("").to_owned();
                let mut out = Vec::new();
                for (key, id) in &self.messages {
                    if key.starts_with(&format!("{key_prefix}:")) { out.push(set(*id, attr::STATUS, MessageStatus::Done)); }
                }
                if message.get("role").and_then(Json::as_str) == Some("assistant") { self.current_message = None; }
                out
            }
            Event::ToolExecutionStart { tool_call_id, tool_name, args, intent } => {
                let mut attrs = vec![(attr::TOOL, tool_name.clone().into()), (attr::CALL_ID, tool_call_id.clone().into()), (attr::STATUS, CallStatus::Running.into())];
                if let Some(intent) = intent { attrs.push((attr::TITLE, intent.clone().into())); }
                let mut out = vec![self.insert(self.turn.unwrap(), K::Call, attrs, "", Pending::Call(tool_call_id.clone(), args.clone()))];
                if is_shell(tool_name) { out.extend(self.shell_job(tool_call_id, args.as_ref())); }
                out
            }
            Event::ToolExecutionUpdate { tool_call_id, args, partial_result, .. } => {
                let mut out = Vec::new();
                if let (Some(input), Some(args)) = (self.inputs.get(tool_call_id), args) {
                    let value = args.to_string();
                    let old = self.input_text.entry(tool_call_id.clone()).or_default();
                    if let Some(delta) = value.strip_prefix(old.as_str()) { if !delta.is_empty() { out.push(Draft::Append { node: *input, text: delta.to_owned() }); } }
                    else { out.push(set(*input, TEXT, value.clone())); }
                    *old = value;
                }
                if let Some(result) = partial_result
                    && let Some(job) = self.jobs.get(tool_call_id)
                {
                    out.push(Draft::Append { node: *job, text: json_text(result) });
                }
                out
            }
            Event::ToolExecutionEnd { tool_call_id, result, is_error, .. } => {
                let mut out = Vec::new();
                if let Some(call) = self.calls.get(tool_call_id).copied() {
                    out.push(self.insert(call, K::Result, vec![(attr::IS_ERROR, (*is_error).into())], result.as_ref().map(json_text).unwrap_or_default(), Pending::Result));
                    out.push(set(call, attr::STATUS, if *is_error { CallStatus::Error } else { CallStatus::Done }));
                }
                if let Some(job) = self.jobs.get(tool_call_id).copied() { out.push(set(job, attr::STATUS, if *is_error { JobStatus::Error } else { JobStatus::Done })); }
                out
            }
            Event::TodoReminder { todos } => self.update_todos(todos),
            Event::TodoAutoClear => self.clear_todos(),
            Event::SubagentLifecycle(l) => self.subagent(l),
            Event::SubagentProgress(p) => {
                let key = p.parent_tool_call_id.as_ref().map(|s| format!("agent:{s}:{}", p.index));
                key.and_then(|key| self.jobs.get(&key).copied()).map(|node| vec![Draft::Append { node, text: format!("{}\n", json_text(&p.progress)) }]).unwrap_or_else(|| vec![self.ext("subagent_progress", &serde_json::json!({"agent":p.agent,"task":p.task,"progress":p.progress,"sessionFile":p.session_file}))])
            }
            Event::CompactionEnd { result: Some(result), aborted: false, skipped: false, .. } => {
                let covers = self.turns.iter().map(|id| Value::Int(id.0 as i64)).collect();
                let mut attrs = vec![(attr::COVERS, Value::List(covers))];
                if let Some(tokens) = result.tokens_before { attrs.push((attr::TOKENS_BEFORE, tokens.into())); }
                vec![self.insert(self.session, K::Compaction, attrs, result.summary.clone(), Pending::Other)]
            }
            Event::ConfigUpdate { model, thinking_level } => {
                let mut out = Vec::new();
                if let Some(model) = model { out.push(set(self.session, attr::MODEL, model.id.clone())); }
                if let Some(level) = thinking_level { out.push(set(self.session, attr::EFFORT, level.clone())); }
                out
            }
            Event::ThinkingLevelChanged { thinking_level: Some(level) } => vec![set(self.session, attr::EFFORT, level.clone())],
            Event::SessionInfoUpdate { title, session_id } => {
                let mut out = vec![set(self.session, attr::NATIVE_ID, session_id.clone())];
                if let Some(title) = title { out.push(set(self.session, attr::TITLE, title.clone())); }
                out
            }
            Event::Unknown { kind, raw } => vec![self.ext(kind, raw)],
            Event::Notice { level, message, source } => vec![self.ext("notice", &serde_json::json!({"level":level,"message":message,"source":source}))],
            Event::CompactionStart { .. } | Event::CompactionEnd { .. } | Event::ModelChanged | Event::ThinkingLevelChanged { .. } => Vec::new(),
            Event::MessageUpdate { role, event, .. } => vec![self.ext("message_update", &serde_json::json!({"role":role,"event":format!("{event:?}")}))],
        }
    }

    fn assistant(&mut self, event: &AssistantEvent, message_id: Option<&str>) -> Vec<Draft> {
        let prefix = message_id.or(self.current_message.as_deref()).unwrap_or("assistant").to_owned();
        match event {
            AssistantEvent::TextStart { content_index } | AssistantEvent::ThinkingStart { content_index } => {
                let key = format!("{prefix}:{content_index}");
                if self.blocks.contains_key(content_index) { return Vec::new(); }
                self.blocks.insert(*content_index, key.clone());
                let kind = if matches!(event, AssistantEvent::ThinkingStart { .. }) { MessageKind::Thinking } else { MessageKind::Text };
                vec![self.insert(self.turn.unwrap(), K::Message, vec![(attr::ROLE, "assistant".into()), (attr::KIND, kind.into()), (attr::STATUS, MessageStatus::Streaming.into()), (attr::ID, prefix.into())], "", Pending::Message(key))]
            }
            AssistantEvent::TextDelta { content_index, delta } | AssistantEvent::ThinkingDelta { content_index, delta } => {
                let Some(key) = self.blocks.get(content_index) else { return Vec::new(); };
                self.text.entry(key.clone()).or_default().push_str(delta);
                self.messages.get(key).map(|node| vec![Draft::Append { node: *node, text: delta.clone() }]).unwrap_or_default()
            }
            AssistantEvent::TextEnd { content_index, content } | AssistantEvent::ThinkingEnd { content_index, content } => {
                let Some(key) = self.blocks.remove(content_index) else { return Vec::new(); };
                let old = self.text.remove(&key).unwrap_or_default();
                self.messages.get(&key).map(|node| {
                    let mut out = Vec::new();
                    if old != *content { out.push(set(*node, TEXT, content.clone())); }
                    out.push(set(*node, attr::STATUS, MessageStatus::Done));
                    out
                }).unwrap_or_default()
            }
            AssistantEvent::ToolcallStart { content_index, block, partial } => {
                let value = block.as_ref().or_else(|| partial.as_ref().and_then(|p| p.get("content").and_then(Json::as_array).and_then(|c| c.get(*content_index as usize))));
                if let Some(id) = value.and_then(|v| v.get("id")).and_then(Json::as_str) { self.blocks.insert(*content_index, id.to_owned()); }
                Vec::new()
            }
            AssistantEvent::ToolcallDelta { content_index, delta } => {
                self.blocks.get(content_index).and_then(|key| self.inputs.get(key)).map(|node| vec![Draft::Append { node: *node, text: delta.clone() }]).unwrap_or_default()
            }
            AssistantEvent::ToolcallEnd { content_index, tool_call } => {
                self.blocks.remove(content_index);
                if let Some(id) = tool_call.get("id").and_then(Json::as_str)
                    && let Some(input) = self.inputs.get(id)
                {
                    return vec![set(*input, TEXT, tool_call.get("arguments").map(json_text).unwrap_or_default())];
                }
                Vec::new()
            }
            AssistantEvent::Done { .. } | AssistantEvent::Error { .. } => {
                let status = if matches!(event, AssistantEvent::Error { .. }) { MessageStatus::Error } else { MessageStatus::Done };
                self.blocks.clear();
                self.messages.values().map(|node| set(*node, attr::STATUS, status)).collect()
            }
            AssistantEvent::Start | AssistantEvent::ImageEnd { .. } | AssistantEvent::Other => Vec::new(),
        }
    }

    fn shell_job(&mut self, key: &str, args: Option<&Json>) -> Vec<Draft> {
        let mut attrs = vec![(attr::KIND, "shell".into()), (attr::STATUS, JobStatus::Running.into()), (attr::ID, key.to_owned().into())];
        if let Some(command) = args.and_then(|v| v.get("command").or_else(|| v.get("cmd"))).and_then(Json::as_str) { attrs.push((attr::COMMAND, command.to_owned().into())); }
        vec![self.insert(self.turn.unwrap(), K::Job, attrs, "", Pending::Job(key.to_owned()))]
    }

    fn subagent(&mut self, l: &loa_omp_rpc::SubagentLifecycle) -> Vec<Draft> {
        let key = l.id.clone();
        if let Some(node) = self.jobs.get(&key).copied() {
            return vec![set(node, attr::STATUS, job_status(l.status))];
        }
        let mut attrs = vec![(attr::KIND, "subagent".into()), (attr::STATUS, job_status(l.status).into()), (attr::ID, key.clone().into()), (attr::AGENT, l.agent.clone().into())];
        if let Some(title) = &l.description { attrs.push((attr::TITLE, title.clone().into())); }
        if let Some(session) = &l.session_file { attrs.push((attr::SESSION, session.clone().into())); }
        vec![self.insert(self.turn.unwrap_or(self.session), K::Job, attrs, "", Pending::Job(key))]
    }

    fn update_todos(&mut self, todos: &[TodoItem]) -> Vec<Draft> {
        let mut out = Vec::new();
        for (index, todo) in todos.iter().enumerate() {
            let key = index.to_string();
            let state = todo_state(todo.status);
            if let Some(id) = self.todos.get(&key) {
                out.push(set(*id, attr::STATE, state));
                out.push(set(*id, TEXT, todo.content.clone()));
            } else {
                out.push(self.insert(self.session, K::Todo, vec![(attr::ID, key.clone().into()), (attr::STATE, state.into())], todo.content.clone(), Pending::Todo(key)));
            }
        }
        for (key, node) in &self.todos { if key.parse::<usize>().is_ok_and(|i| i >= todos.len()) { out.push(set(*node, attr::STATE, TodoState::Cancelled)); } }
        out
    }

    fn clear_todos(&mut self) -> Vec<Draft> { self.todos.values().map(|id| set(*id, attr::STATE, TodoState::Cancelled)).collect() }

    pub fn ui(&mut self, request: &UiRequest) -> Vec<Draft> {
        if !request.is_dialog() {
            if let UiRequest::Cancel { target_id, .. } = request {
                return self.decisions.get(target_id).map(|id| vec![set(*id, attr::STATUS, DecisionStatus::Cancelled)]).unwrap_or_default();
            }
            return vec![self.ext("extension_ui_request", &serde_json::json!({"request":format!("{request:?}")}))];
        }
        if self.turn.is_none() {
            self.deferred_ui.push_back(request.clone());
            return if self.pending.iter().any(|p| matches!(p, Pending::Turn)) { Vec::new() } else { self.turn_start() };
        }
        let (kind, title, body, options, multi, timeout) = match request {
            UiRequest::Select { title, options, timeout, .. } => (DecisionKind::Question, title.clone(), title.clone(), options.clone(), false, *timeout),
            UiRequest::Confirm { title, message, timeout, .. } => (DecisionKind::Approval, title.clone(), message.clone(), vec!["yes".into(), "no".into()], false, *timeout),
            UiRequest::Input { title, timeout, .. } => (DecisionKind::Question, title.clone(), title.clone(), Vec::new(), false, *timeout),
            UiRequest::Editor { title, prefill, .. } => (DecisionKind::Plan, title.clone(), prefill.clone().unwrap_or_default(), Vec::new(), false, None),
            UiRequest::Ask { questions, timeout, .. } => (DecisionKind::Form, "Questions".into(), questions.iter().map(|q| q.question.as_str()).collect::<Vec<_>>().join("\n"), questions.iter().flat_map(|q| q.options.iter().map(|o| o.label.clone())).collect(), questions.iter().any(|q| q.multi), *timeout),
            _ => unreachable!(),
        };
        let mut attrs = vec![(attr::KIND, kind.into()), (attr::STATUS, DecisionStatus::Waiting.into()), (attr::REQUEST_ID, request.id().to_owned().into()), (attr::TITLE, title.into()), (attr::OPTIONS, options.into()), (attr::MULTI, multi.into())];
        if let Some(timeout) = timeout { attrs.push((attr::TIMEOUT_MS, Value::Int(timeout as i64))); }
        vec![self.insert(self.turn.unwrap(), K::Decision, attrs, body, Pending::Decision(request.id().to_owned(), request.clone())), set(self.turn.unwrap(), attr::STATUS, TurnStatus::Waiting)]
    }

    pub fn decision(&self, id: NodeId) -> Option<UiRequest> { self.requests.get(&id).cloned() }

    pub fn answered(&mut self, id: NodeId, answer: &Answer) -> Vec<Draft> {
        let mut out = vec![set(id, attr::ANSWER, Value::List(answer.values.iter().cloned().map(Value::Str).collect())), set(id, attr::BY, answer.by.clone()), set(id, attr::STATUS, DecisionStatus::Answered)];
        if let Some(reason) = &answer.reason { out.push(set(id, attr::REASON, reason.clone())); }
        if let Some(turn) = self.turn { out.push(set(turn, attr::STATUS, TurnStatus::Running)); }
        self.requests.remove(&id);
        out
    }

    fn cancelled_for(&self, turn: NodeId) -> Vec<Draft> {
        let mut out: Vec<_> = self.messages.values().map(|id| set(*id, attr::STATUS, MessageStatus::Cancelled)).collect();
        out.extend(self.calls.values().map(|id| set(*id, attr::STATUS, CallStatus::Cancelled)));
        out.extend(self.jobs.values().map(|id| set(*id, attr::STATUS, JobStatus::Cancelled)));
        out.extend(self.requests.keys().map(|id| set(*id, attr::STATUS, DecisionStatus::Cancelled)));
        out.push(set(turn, attr::STATUS, TurnStatus::Cancelled));
        out
    }

    pub fn cancelled(&mut self) -> Vec<Draft> { self.finish(TurnStatus::Cancelled) }

    fn finish(&mut self, status: TurnStatus) -> Vec<Draft> {
        let Some(turn) = self.turn.take() else { return Vec::new(); };
        let mut out = if status == TurnStatus::Cancelled { self.cancelled_for(turn) } else { Vec::new() };
        if status != TurnStatus::Cancelled { out.push(set(turn, attr::STATUS, status)); }
        self.calls.clear(); self.messages.clear(); self.jobs.clear(); self.blocks.clear(); self.text.clear(); self.inputs.clear(); self.input_text.clear(); self.requests.clear(); self.decisions.clear();
        out
    }

    fn ext(&mut self, kind: &str, raw: &Json) -> Draft {
        self.insert(self.turn.unwrap_or(self.session), K::Ext, vec![(attr::HARNESS, "omp".into()), (attr::TYPE, kind.to_owned().into())], raw.to_string(), Pending::Other)
    }

    /// Feed each successfully committed patch back in order. Any returned
    /// drafts must be committed and fed back before reading another RPC frame.
    pub fn committed(&mut self, patches: &[Patch]) -> Vec<Draft> {
        let mut out = Vec::new();
        for patch in patches {
            if let Op::Insert { .. } = &patch.op {
                let Some(pending) = self.pending.pop_front() else { continue; };
                match pending {
                    Pending::Turn => {
                        self.turn = Some(patch.node); self.turns.push(patch.node);
                        while let Some(prompt) = self.deferred_prompts.pop_front() { out.extend(self.sent(&prompt)); }
                        while let Some(event) = self.deferred.pop_front() { out.extend(self.event(&event)); }
                        while let Some(request) = self.deferred_ui.pop_front() { out.extend(self.ui(&request)); }
                    }
                    Pending::User => {}
                    Pending::Message(key) => { self.messages.insert(key, patch.node); }
                    Pending::Call(key, args) => {
                        self.calls.insert(key.clone(), patch.node);
                        if let Some(args) = args { let text = json_text(&args); self.input_text.insert(key.clone(), text.clone()); out.push(self.insert(patch.node, K::Input, vec![(attr::FORMAT, "json".into())], text, Pending::Input(key))); }
                    }
                    Pending::Input(key) => { self.inputs.insert(key, patch.node); }
                    Pending::Result | Pending::Other => {}
                    Pending::Job(key) => { self.jobs.insert(key, patch.node); }
                    Pending::Decision(key, request) => { self.decisions.insert(key, patch.node); self.requests.insert(patch.node, request); }
                    Pending::Todo(key) => { self.todos.insert(key, patch.node); }
                }
            }
        }
        out
    }
}

fn needs_turn(event: &Event) -> bool {
    matches!(event, Event::MessageStart { .. } | Event::MessageUpdate { .. } | Event::MessageEnd { .. } | Event::ToolExecutionStart { .. } | Event::ToolExecutionUpdate { .. } | Event::ToolExecutionEnd { .. } | Event::SubagentLifecycle(_) | Event::SubagentProgress(_))
}
fn set(node: NodeId, name: &str, value: impl Into<Value>) -> Draft { Draft::Set { node, name: name.into(), value: Some(value.into()) } }
fn json_text(value: &Json) -> String { value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string()) }
fn is_shell(tool: &str) -> bool { matches!(tool, "bash" | "shell" | "exec" | "remote_bash") }
fn todo_state(status: TodoStatus) -> TodoState { match status { TodoStatus::Pending => TodoState::Pending, TodoStatus::InProgress => TodoState::InProgress, TodoStatus::Completed => TodoState::Done, TodoStatus::Abandoned => TodoState::Cancelled } }
fn job_status(status: SubagentStatus) -> JobStatus { match status { SubagentStatus::Started => JobStatus::Running, SubagentStatus::Completed => JobStatus::Done, SubagentStatus::Failed => JobStatus::Error, SubagentStatus::Aborted => JobStatus::Cancelled } }
