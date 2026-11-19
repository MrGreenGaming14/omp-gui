//! Pure translation of omp's ordered RPC stream into session DOM drafts.
//! Inserts are acknowledged through `committed` before a child can reference
//! their writer-assigned node id. Callers must commit its returned drafts too.
//!
//! A `<turn>` is one prompt and everything omp did for it. A prompt is
//! registered with [`Mapping::queue`] before it is sent; its turn opens when
//! omp starts running it (a run starts, or a queued follow-up's user message
//! is delivered into a running one) and closes with the prompt's
//! `prompt_result`. omp's own `turn_end`/`agent_end` are model steps inside
//! it.

use loa_adapter::{AdapterError, Answer, Prompt, Result};
use loa_core::{answer, attr, CallStatus, DecisionKind, DecisionStatus, Draft, ElementKind as K, JobStatus, MessageKind, MessageStatus, NodeId, Op, Patch, TodoState, TurnStatus, Value};
use loa_core::schema::TEXT;
use loa_omp_rpc::{AskAnswer, AskQuestion, AssistantEvent, Event, PromptError, PromptStatus, SubagentStatus, TodoItem, TodoStatus, UiRequest, UiResponse};
use serde_json::Value as Json;
use std::collections::{HashMap, HashSet, VecDeque};

/// omp's event listing its slash commands. Recorded as an `<ext>` of this
/// type on the session root (the latest one is current), where the app reads
/// the commands it offers in the composer.
pub const COMMANDS: &str = "available_commands_update";
/// omp's output of a slash command it ran itself.
const COMMAND_OUTPUT: &str = "command_output";

#[derive(Debug)]
enum Pending {
    /// The current turn; its user message is the prompt omp is running.
    Turn(Option<Prompt>),
    /// A turn whose prompt omp finished without running it.
    Closed(Prompt),
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
    /// Prompts sent to omp that no turn runs yet, oldest first, by command id.
    queued: VecDeque<(String, Prompt)>,
    /// Command id of the prompt the current turn runs.
    turn_prompt: Option<String>,
    /// omp hasn't delivered the current turn's own user message yet.
    echo: bool,
    blocks: HashMap<u32, String>,
    text: HashMap<String, String>,
    inputs: HashMap<String, NodeId>,
    input_text: HashMap<String, String>,
    requests: HashMap<NodeId, UiRequest>,
    /// Messages, calls and jobs of the current turn that reached a final status.
    closed: HashSet<NodeId>,
    turns: Vec<NodeId>,
    current_message: Option<String>,
    /// Text typed on a question card whose `select` offered omp's "Other"
    /// row: omp asks for it with the `input` that follows.
    typed: Option<String>,
    /// The command list last recorded, so repeats add nothing.
    commands: Option<Json>,
}

impl Mapping {
    pub fn new(session: NodeId) -> Self {
        Self {
            session, turn: None, calls: HashMap::new(), messages: HashMap::new(),
            jobs: HashMap::new(), todos: HashMap::new(), decisions: HashMap::new(),
            pending: VecDeque::new(), deferred: VecDeque::new(), deferred_ui: VecDeque::new(),
            queued: VecDeque::new(), turn_prompt: None, echo: false,
            blocks: HashMap::new(), text: HashMap::new(), inputs: HashMap::new(),
            input_text: HashMap::new(), requests: HashMap::new(), closed: HashSet::new(), turns: Vec::new(),
            current_message: None, typed: None, commands: None,
        }
    }

    fn insert(&mut self, parent: NodeId, kind: K, attrs: Vec<(&str, Value)>, text: impl Into<String>, pending: Pending) -> Draft {
        self.pending.push_back(pending);
        Draft::insert(parent, kind, attrs, text.into())
    }

    /// Sets a final `status` on a message, call or job of the current turn.
    fn close(&mut self, node: NodeId, status: impl Into<Value>) -> Draft {
        self.closed.insert(node);
        set(node, attr::STATUS, status)
    }

    fn opening(&self) -> bool { self.pending.iter().any(|p| matches!(p, Pending::Turn(_))) }

    /// Opens the turn of the oldest queued prompt (or of omp's own work when
    /// none is queued), ending the current turn first.
    fn open_turn(&mut self) -> Vec<Draft> {
        let mut out = self.finish(TurnStatus::Done);
        let prompt = self.queued.pop_front().map(|(id, prompt)| {
            self.turn_prompt = Some(id);
            prompt
        });
        self.echo = prompt.is_some();
        out.push(self.insert(self.session, K::Turn, vec![(attr::STATUS, TurnStatus::Running.into())], "", Pending::Turn(prompt)));
        out
    }

    fn user_message(&mut self, turn: NodeId, prompt: &Prompt) -> Draft {
        self.insert(turn, K::Message, vec![(attr::ROLE, "user".into()), (attr::STATUS, MessageStatus::Done.into()), (attr::BY, prompt.by.clone().into())], prompt.text.clone(), Pending::User)
    }

    /// Registers `prompt`, about to be sent as command `id`. omp runs it now
    /// or, while busy, as a follow-up after the work before it.
    pub fn queue(&mut self, id: impl Into<String>, prompt: &Prompt) {
        self.queued.push_back((id.into(), prompt.clone()));
    }

    /// Forgets a queued prompt omp refused.
    pub fn withdraw(&mut self, id: &str) {
        self.queued.retain(|(queued, _)| queued != id);
    }

    /// No turn is open and no sent prompt waits for omp.
    pub fn is_idle(&self) -> bool {
        self.turn.is_none() && self.queued.is_empty() && !self.opening()
    }

    pub fn event(&mut self, event: &Event) -> Vec<Draft> {
        let command_output = matches!(event, Event::Unknown { kind, .. } if kind == COMMAND_OUTPUT) && !self.queued.is_empty();
        if self.turn.is_none() && (needs_turn(event) || command_output) {
            self.deferred.push_back(event.clone());
            return if self.opening() { Vec::new() } else { self.open_turn() };
        }
        match event {
            Event::AgentStart | Event::TurnStart => {
                if self.turn.is_none() && !self.opening() { self.open_turn() } else { Vec::new() }
            }
            Event::AgentEnd { .. } | Event::TurnEnd => Vec::new(),
            Event::SessionSettled => self.finish(TurnStatus::Done),
            Event::PromptResult { id, status, error, .. } => self.prompt_result(id.as_deref(), *status, error.as_ref()),
            Event::MessageStart { message, message_id } => {
                self.current_message = message_id.clone();
                if !is_user_prompt(message) {
                    Vec::new()
                } else if self.echo {
                    self.echo = false;
                    Vec::new()
                } else if self.queued.is_empty() {
                    // Steering omp took from elsewhere stays in its transcript only.
                    Vec::new()
                } else {
                    // omp delivered a queued follow-up: the work before it is done.
                    let out = self.open_turn();
                    self.echo = false;
                    out
                }
            }
            Event::MessageUpdate { role, event, message_id } if role == "assistant" => self.assistant(event, message_id.as_deref()),
            Event::MessageEnd { message, message_id } => {
                let key_prefix = format!("{}:", message_id.as_deref().or(self.current_message.as_deref()).unwrap_or(""));
                let status = match message.get("stopReason").and_then(Json::as_str) {
                    Some("aborted") => MessageStatus::Cancelled,
                    Some("error") => MessageStatus::Error,
                    _ => MessageStatus::Done,
                };
                let blocks: Vec<NodeId> = self.messages.iter().filter(|(key, _)| key.starts_with(&key_prefix)).map(|(_, id)| *id).collect();
                let open: Vec<NodeId> = blocks.iter().copied().filter(|id| !self.closed.contains(id)).collect();
                let mut out: Vec<Draft> = open.into_iter().map(|id| self.close(id, status)).collect();
                if message.get("role").and_then(Json::as_str) == Some("assistant") {
                    self.current_message = None;
                    // Usage and cost of this model call, on its last block.
                    let target = blocks.into_iter().max().or(self.turn);
                    if let (Some(target), Some(attrs)) = (target, usage(message)) {
                        out.push(self.insert(target, K::Usage, attrs, "", Pending::Other));
                    }
                }
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
                    // Each partial result holds the whole output so far.
                    out.push(set(*job, TEXT, tool_text(result)));
                }
                out
            }
            Event::ToolExecutionEnd { tool_call_id, result, is_error, .. } => {
                let mut out = Vec::new();
                if let Some(call) = self.calls.get(tool_call_id).copied() {
                    out.push(self.insert(call, K::Result, vec![(attr::IS_ERROR, (*is_error).into())], result.as_ref().map(tool_text).unwrap_or_default(), Pending::Result));
                    out.push(self.close(call, if *is_error { CallStatus::Error } else { CallStatus::Done }));
                }
                if let Some(job) = self.jobs.get(tool_call_id).copied() { out.push(self.close(job, if *is_error { JobStatus::Error } else { JobStatus::Done })); }
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
                if let Some(model) = model { out.push(set(self.session, attr::MODEL, model.selector())); }
                if let Some(level) = thinking_level { out.push(set(self.session, attr::EFFORT, level.clone())); }
                out
            }
            Event::ThinkingLevelChanged { thinking_level: Some(level) } => vec![set(self.session, attr::EFFORT, level.clone())],
            Event::SessionInfoUpdate { title, session_id } => {
                let mut out = vec![set(self.session, attr::NATIVE_ID, session_id.clone())];
                if let Some(title) = title { out.push(set(self.session, attr::TITLE, title.clone())); }
                out
            }
            Event::Unknown { kind, raw } if kind == COMMANDS => {
                let commands = raw.get("commands").cloned().unwrap_or(Json::Null);
                if self.commands.as_ref() == Some(&commands) { return Vec::new(); }
                self.commands = Some(commands);
                vec![self.insert(self.session, K::Ext, vec![(attr::HARNESS, "omp".into()), (attr::TYPE, COMMANDS.into())], raw.to_string(), Pending::Other)]
            }
            // A slash command omp ran itself answers in its turn.
            Event::Unknown { kind, raw } if kind == COMMAND_OUTPUT => {
                let text = raw.get("text").and_then(Json::as_str).unwrap_or_default();
                match self.turn {
                    Some(turn) => vec![self.insert(turn, K::Message, vec![(attr::ROLE, "assistant".into()), (attr::KIND, MessageKind::Text.into()), (attr::STATUS, MessageStatus::Done.into())], text, Pending::Other)],
                    None => vec![self.ext(COMMAND_OUTPUT, raw)],
                }
            }
            // omp's TUI bookkeeping (`advisor_cost_changed` carries no
            // payload; the cost arrives as usage on `message_end`) has no
            // element; newer events are dropped the same way.
            Event::Unknown { kind, .. } => {
                tracing::trace!(kind, "omp event without an element, ignored");
                Vec::new()
            }
            Event::Notice { level, message, source } => vec![self.ext("notice", &serde_json::json!({"level":level,"message":message,"source":source}))],
            Event::CompactionStart { .. } | Event::CompactionEnd { .. } | Event::ModelChanged | Event::ThinkingLevelChanged { .. } => Vec::new(),
            Event::MessageUpdate { role, .. } => {
                tracing::trace!(role, "omp message_update for a non-assistant message, ignored");
                Vec::new()
            }
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
                let Some(node) = self.messages.get(&key).copied() else { return Vec::new(); };
                let mut out = Vec::new();
                if old != *content { out.push(set(node, TEXT, content.clone())); }
                out.push(self.close(node, MessageStatus::Done));
                out
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
                let open: Vec<NodeId> = self.messages.values().copied().filter(|id| !self.closed.contains(id)).collect();
                open.into_iter().map(|id| self.close(id, status)).collect()
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
            let status = job_status(l.status);
            return vec![if status == JobStatus::Running { set(node, attr::STATUS, status) } else { self.close(node, status) }];
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
            return match request {
                UiRequest::Cancel { target_id, .. } => {
                    let Some(id) = self.decisions.get(target_id).copied() else { return Vec::new(); };
                    self.requests.remove(&id);
                    vec![set(id, attr::STATUS, DecisionStatus::Cancelled)]
                }
                UiRequest::Notify { message, notify_type, .. } => {
                    vec![self.ext("notice", &serde_json::json!({"level":notify_type,"message":message,"source":"extension"}))]
                }
                UiRequest::SetTitle { title, .. } => vec![set(self.session, attr::TITLE, title.clone())],
                UiRequest::OpenUrl { url, launch_url, instructions, .. } => {
                    vec![self.ext("open_url", &serde_json::json!({"url":url,"launchUrl":launch_url,"instructions":instructions}))]
                }
                // Status lines, widgets and editor text belong to omp's TUI.
                _ => {
                    tracing::trace!(request = ?request, "omp TUI request without an element, ignored");
                    Vec::new()
                }
            };
        }
        if self.turn.is_none() {
            self.deferred_ui.push_back(request.clone());
            return if self.opening() { Vec::new() } else { self.open_turn() };
        }
        let (kind, title, body, options, multi, timeout) = match request {
            UiRequest::Select { title, options, timeout, .. } => match Approval::of(options) {
                Some(approval) => (DecisionKind::Approval, title.lines().next().unwrap_or_default().to_owned(), title.clone(), approval.answers(), false, *timeout),
                None => (DecisionKind::Question, title.clone(), title.clone(), options.clone(), false, *timeout),
            },
            UiRequest::Confirm { title, message, timeout, .. } => (DecisionKind::Approval, title.clone(), message.clone(), vec!["yes".into(), "no".into()], false, *timeout),
            UiRequest::Input { title, timeout, .. } => (DecisionKind::Question, title.clone(), title.clone(), Vec::new(), false, *timeout),
            UiRequest::Editor { title, prefill, .. } => (DecisionKind::Plan, title.clone(), prefill.clone().unwrap_or_default(), Vec::new(), false, None),
            UiRequest::Ask { questions, timeout, .. } => match questions.as_slice() {
                [q] => (DecisionKind::Question, q.header.clone().unwrap_or_else(|| q.question.clone()), q.question.clone(), labels(q), q.multi, *timeout),
                _ => (DecisionKind::Form, "Questions".into(), questions.iter().map(|q| q.question.as_str()).collect::<Vec<_>>().join("\n"), Vec::new(), false, *timeout),
            },
            _ => unreachable!(),
        };
        let mut attrs = vec![(attr::KIND, kind.into()), (attr::STATUS, DecisionStatus::Waiting.into()), (attr::REQUEST_ID, request.id().to_owned().into()), (attr::TITLE, title.into()), (attr::OPTIONS, options.into()), (attr::MULTI, multi.into())];
        if let Some(timeout) = timeout { attrs.push((attr::TIMEOUT_MS, Value::Int(timeout as i64))); }
        if let UiRequest::Ask { questions, .. } = request
            && kind == DecisionKind::Form
        {
            attrs.push((attr::FIELDS, form_fields(questions).into()));
        }
        vec![self.insert(self.turn.unwrap(), K::Decision, attrs, body, Pending::Decision(request.id().to_owned(), request.clone())), set(self.turn.unwrap(), attr::STATUS, TurnStatus::Waiting)]
    }

    /// What answering the waiting `decision` with `answer` sends to omp.
    pub fn reply(&self, decision: NodeId, answer: &Answer) -> Result<Reply> {
        let request = self.requests.get(&decision).ok_or(AdapterError::UnknownDecision(decision))?;
        let id = request.id().to_owned();
        let first = answer.values.first().map(String::as_str);
        let unknown = || AdapterError::Protocol(format!("`{}` isn't an answer to this decision", first.unwrap_or_default()));
        let (response, approval) = match request {
            UiRequest::Select { options, .. } => match (Approval::of(options), first) {
                (_, None) => (UiResponse::Cancelled { id, timed_out: false }, false),
                (Some(approval), Some(value)) => (UiResponse::Value { id, value: approval.label(value).ok_or_else(unknown)?.to_owned() }, true),
                // Typed text goes through omp's "Other" row, then its `input`.
                (None, Some(value)) if !options.iter().any(|o| o == value) && options.iter().any(|o| o == OTHER) => (UiResponse::Value { id, value: OTHER.to_owned() }, false),
                (None, Some(value)) => (UiResponse::Value { id, value: value.to_owned() }, false),
            },
            UiRequest::Input { .. } | UiRequest::Editor { .. } => match first {
                Some(value) => (UiResponse::Value { id, value: value.to_owned() }, false),
                None => (UiResponse::Cancelled { id, timed_out: false }, false),
            },
            UiRequest::Confirm { .. } => (UiResponse::Confirmed { id, confirmed: matches!(first, Some(answer::ALLOW_ONCE | answer::ALLOW_SESSION | answer::APPROVE | "yes" | "true")) }, true),
            UiRequest::Ask { questions, .. } => (match ask_answers(questions, &answer.values) {
                Some(answers) => UiResponse::Answers { id, answers },
                None => UiResponse::Cancelled { id, timed_out: false },
            }, false),
            _ => return Err(AdapterError::UnknownDecision(decision)),
        };
        // omp's approval prompt carries no reason; the model reads it as steering.
        let steer = (approval && first == Some(answer::DENY))
            .then_some(answer.reason.as_deref().map(str::trim).filter(|reason| !reason.is_empty()))
            .flatten()
            .map(|reason| format!("I denied that tool call: {reason}"));
        Ok(Reply { response, steer })
    }

    pub fn answered(&mut self, id: NodeId, answer: &Answer) -> Vec<Draft> {
        let mut out = vec![set(id, attr::ANSWER, Value::List(answer.values.iter().cloned().map(Value::Str).collect())), set(id, attr::BY, answer.by.clone()), set(id, attr::STATUS, DecisionStatus::Answered)];
        if let Some(reason) = &answer.reason { out.push(set(id, attr::REASON, reason.clone())); }
        if let Some(turn) = self.turn { out.push(set(turn, attr::STATUS, TurnStatus::Running)); }
        if let Some(UiRequest::Select { options, .. }) = self.requests.remove(&id)
            && let Some(value) = answer.values.first()
            && Approval::of(&options).is_none()
            && !options.contains(value)
            && options.iter().any(|o| o == OTHER)
        {
            self.typed = Some(value.clone());
        }
        out
    }

    /// The answer to a dialog omp asks without a card: the `input` omp opens
    /// after its "Other" row was picked gets the text typed on the card.
    pub fn auto_reply(&mut self, request: &UiRequest) -> Option<UiResponse> {
        if !request.is_dialog() { return None; }
        match (request, self.typed.take()) {
            (UiRequest::Input { id, .. }, Some(value)) => Some(UiResponse::Value { id: id.clone(), value }),
            _ => None,
        }
    }

    fn prompt_result(&mut self, id: Option<&str>, status: PromptStatus, error: Option<&PromptError>) -> Vec<Draft> {
        let status = match status {
            PromptStatus::Completed => TurnStatus::Done,
            PromptStatus::Aborted => TurnStatus::Cancelled,
            PromptStatus::Error => TurnStatus::Error,
        };
        let mut out = if let Some(index) = id.and_then(|id| self.queued.iter().position(|(queued, _)| queued == id)) {
            // omp finished the prompt without running it: an abort dropped it
            // from omp's queue, or it was handled locally.
            let (_, prompt) = self.queued.remove(index).expect("position is in the queue");
            vec![self.insert(self.session, K::Turn, vec![(attr::STATUS, status.into())], "", Pending::Closed(prompt))]
        } else if id.is_none() || self.turn_prompt.is_none() || self.turn_prompt.as_deref() == id {
            self.finish(status)
        } else {
            // An earlier turn's prompt, settled at the same yield as the current one.
            Vec::new()
        };
        if let Some(error) = error { out.push(self.ext("prompt_error", &serde_json::json!({"message":error.message,"provider":error.provider,"model":error.model,"code":error.code,"retryable":error.retryable}))); }
        out
    }

    /// Ends the current turn. Nothing in it is left streaming, running or waiting.
    fn finish(&mut self, status: TurnStatus) -> Vec<Draft> {
        let Some(turn) = self.turn.take() else { return Vec::new(); };
        let open = |id: &&NodeId| !self.closed.contains(*id);
        let message = match status {
            TurnStatus::Cancelled => MessageStatus::Cancelled,
            TurnStatus::Error => MessageStatus::Error,
            _ => MessageStatus::Done,
        };
        let mut out: Vec<Draft> = self.messages.values().filter(open).map(|id| set(*id, attr::STATUS, message)).collect();
        if status == TurnStatus::Cancelled {
            out.extend(self.calls.values().filter(open).map(|id| set(*id, attr::STATUS, CallStatus::Cancelled)));
            out.extend(self.jobs.values().filter(open).map(|id| set(*id, attr::STATUS, JobStatus::Cancelled)));
        }
        out.extend(self.requests.keys().map(|id| set(*id, attr::STATUS, DecisionStatus::Cancelled)));
        out.push(set(turn, attr::STATUS, status));
        self.calls.clear(); self.messages.clear(); self.jobs.clear(); self.blocks.clear(); self.text.clear(); self.inputs.clear(); self.input_text.clear(); self.requests.clear(); self.decisions.clear(); self.closed.clear();
        self.turn_prompt = None;
        self.typed = None;
        self.echo = false;
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
                    Pending::Turn(prompt) => {
                        self.turn = Some(patch.node); self.turns.push(patch.node);
                        if let Some(prompt) = prompt { out.push(self.user_message(patch.node, &prompt)); }
                        while let Some(event) = self.deferred.pop_front() { out.extend(self.event(&event)); }
                        while let Some(request) = self.deferred_ui.pop_front() { out.extend(self.ui(&request)); }
                    }
                    Pending::Closed(prompt) => { self.turns.push(patch.node); out.push(self.user_message(patch.node, &prompt)); }
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

fn labels(question: &AskQuestion) -> Vec<String> { question.options.iter().map(|o| o.label.clone()).collect() }

/// JSON Schema of the form card for an `ask` with several questions: one
/// field per question, keyed by its id.
fn form_fields(questions: &[AskQuestion]) -> String {
    let properties: serde_json::Map<String, Json> = questions.iter().map(|q| {
        let labels = labels(q);
        let mut field = serde_json::json!({"title": q.question});
        if let Some(header) = &q.header { field["description"] = header.as_str().into(); }
        if q.multi {
            field["type"] = "array".into();
            field["items"] = serde_json::json!({"type": "string", "enum": labels});
        } else {
            field["type"] = "string".into();
            if let Some(default) = q.recommended.and_then(|index| labels.get(index as usize)) { field["default"] = default.as_str().into(); }
            if !labels.is_empty() { field["enum"] = labels.into(); }
        }
        (q.id.clone(), field)
    }).collect();
    let required: Vec<&str> = questions.iter().map(|q| q.id.as_str()).collect();
    serde_json::json!({"type": "object", "properties": properties, "required": required}).to_string()
}

/// omp's answers to an `ask`: a question card answers picked labels or typed
/// text, a form card one JSON object keyed by question id. `None` cancels.
fn ask_answers(questions: &[AskQuestion], values: &[String]) -> Option<Vec<AskAnswer>> {
    let answer = |q: &AskQuestion, selected_options: Vec<String>, custom_input: Option<String>| AskAnswer { id: q.id.clone(), selected_options, custom_input };
    if let [q] = questions {
        if values.is_empty() { return None; }
        let labels = labels(q);
        return Some(vec![if values.iter().all(|v| labels.contains(v)) { answer(q, values.to_vec(), None) } else { answer(q, Vec::new(), Some(values.join("\n"))) }]);
    }
    let [object] = values else { return None; };
    let Ok(Json::Object(fields)) = serde_json::from_str::<Json>(object) else { return None; };
    Some(questions.iter().map(|q| match fields.get(&q.id) {
        Some(Json::String(text)) if q.options.iter().any(|o| o.label == *text) => answer(q, vec![text.clone()], None),
        Some(Json::String(text)) => answer(q, Vec::new(), Some(text.clone())),
        Some(Json::Array(items)) => answer(q, items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect(), None),
        _ => answer(q, Vec::new(), None),
    }).collect())
}

/// What answering a decision sends to omp.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub response: UiResponse,
    /// Steering that tells the model why its tool call was denied.
    pub steer: Option<String>,
}

/// The row omp's `ask` tool adds to a question's `select` for typed answers.
const OTHER: &str = "Other (type your own)";

/// omp's tool approval prompt: a `select` whose last option is `Deny`, with
/// `Approve` (`Approve Once` when it also offers file and folder grants) and,
/// for prompts the approval mode raised, `Approve <tool> for this session`.
/// omp denies any other value.
struct Approval<'a> {
    once: &'a str,
    session: Option<&'a str>,
    deny: &'a str,
    options: &'a [String],
}

impl<'a> Approval<'a> {
    fn of(options: &'a [String]) -> Option<Self> {
        let (deny, rest) = options.split_last()?;
        if deny != "Deny" { return None; }
        let once = rest.iter().find(|o| *o == "Approve Once").or_else(|| rest.iter().find(|o| *o == "Approve"))?;
        let session = rest.iter().find(|o| o.starts_with("Approve ") && o.ends_with(" for this session"));
        Some(Self { once, session: session.map(String::as_str), deny, options })
    }

    fn answers(&self) -> Vec<String> {
        let session = self.session.map(|_| answer::ALLOW_SESSION);
        [Some(answer::ALLOW_ONCE), session, Some(answer::DENY)].into_iter().flatten().map(str::to_owned).collect()
    }

    /// omp's label for `answer`. A native label (a grant choice) passes through.
    fn label(&self, answer: &str) -> Option<&'a str> {
        match answer {
            answer::ALLOW_ONCE => Some(self.once),
            answer::ALLOW_SESSION => self.session,
            answer::DENY => Some(self.deny),
            other => self.options.iter().find(|o| *o == other).map(String::as_str),
        }
    }
}

fn set(node: NodeId, name: &str, value: impl Into<Value>) -> Draft { Draft::Set { node, name: name.into(), value: Some(value.into()) } }
fn json_text(value: &Json) -> String { value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string()) }
/// Text of a tool result: its `content` text blocks, as in imported sessions.
fn tool_text(value: &Json) -> String { value.get("content").map_or_else(|| json_text(value), |content| crate::native::content_text(Some(content))) }

/// `<usage>` attributes of an omp message's `usage` (token counts, total
/// cost) and its `provider/model`; `None` without usage.
fn usage(message: &Json) -> Option<Vec<(&'static str, Value)>> {
    let usage = message.get("usage")?.as_object()?;
    let mut attrs: Vec<(&str, Value)> = Vec::new();
    for (name, key) in [(attr::INPUT_TOKENS, "input"), (attr::OUTPUT_TOKENS, "output"), (attr::CACHE_READ_TOKENS, "cacheRead"), (attr::CACHE_WRITE_TOKENS, "cacheWrite")] {
        if let Some(count) = usage.get(key).and_then(Json::as_i64) { attrs.push((name, count.into())); }
    }
    if let Some(cost) = usage.get("cost").and_then(|cost| cost.get("total")).and_then(Json::as_f64) { attrs.push((attr::COST, cost.into())); }
    let field = |key| message.get(key).and_then(Json::as_str);
    let model = match (field("provider"), field("model")) {
        (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
        (provider, model) => model.or(provider).map(str::to_owned),
    };
    if let Some(model) = model { attrs.push((attr::MODEL, model.into())); }
    Some(attrs)
}

/// A user message from a host prompt, not one an agent or extension wrote.
fn is_user_prompt(message: &Json) -> bool {
    message.get("role").and_then(Json::as_str) == Some("user")
        && message.get("attribution").and_then(Json::as_str).is_none_or(|by| by == "user")
}
fn is_shell(tool: &str) -> bool { matches!(tool, "bash" | "shell" | "exec" | "remote_bash") }
fn todo_state(status: TodoStatus) -> TodoState { match status { TodoStatus::Pending => TodoState::Pending, TodoStatus::InProgress => TodoState::InProgress, TodoStatus::Completed => TodoState::Done, TodoStatus::Abandoned => TodoState::Cancelled } }
fn job_status(status: SubagentStatus) -> JobStatus { match status { SubagentStatus::Started => JobStatus::Running, SubagentStatus::Completed => JobStatus::Done, SubagentStatus::Failed => JobStatus::Error, SubagentStatus::Aborted => JobStatus::Cancelled } }
