//! omp's ordered RPC stream as session DOM writes.
//!
//! A `<turn>` is one prompt and everything omp did for it. A prompt is
//! registered with [`Input::Sent`] before it is sent; its turn opens when
//! omp starts running it (a run starts, or a queued follow-up's user message
//! is delivered into a running one) and closes with the prompt's
//! `prompt_result`. omp's own `turn_end`/`agent_end` are model steps inside it.

use super::rpc::{AskAnswer, AskQuestion, AssistantEvent, Event, Frame, PromptError, PromptStatus, SubagentLifecycle, SubagentProgress, SubagentStatus, TodoItem, TodoStatus, UiRequest, UiResponse};
use super::HARNESS;
use crate::kit::{EventMapper, TurnLedger};
use loa_contracts::harness::{AdapterError, Answer, Prompt, Result, SessionState};
use loa_core::schema::TEXT;
use loa_core::{CallStatus, CommitError, DecisionKind, DecisionStatus, DomWriter, ElementKind as K, JobStatus, MessageKind, MessageStatus, NodeId, TodoState, TurnStatus, Value, answer, attr};
use serde::{Deserialize, Deserializer};
use serde_json::{Value as Json, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::mem::take;

/// omp's event listing its slash commands, kept as an `<ext>` of this type on
/// the session root (the latest is current); the composer offers them.
pub const COMMANDS: &str = "available_commands_update";
/// omp's output of a slash command it ran itself.
const COMMAND_OUTPUT: &str = "command_output";

type Out<T = ()> = std::result::Result<T, CommitError>;

/// What the session feeds the [`Mapper`]: omp's frames and the prompts it sends.
#[derive(Clone, Debug)]
pub enum Input {
    /// A prompt about to be sent as command `id`; omp runs it now or, while
    /// busy, as a follow-up after the work before it.
    Sent { id: String, prompt: Prompt },
    /// The answer to the dialog omp opened as `request`.
    Answer { request: String, answer: Answer },
    Frame(Frame),
}

/// Fixture lines: `{"$":"send","id","text"}`, `{"$":"answer","request","values","reason"?}` or a frame.
impl<'de> Deserialize<'de> for Input {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = Json::deserialize(d)?;
        let text = |key: &str| raw[key].as_str().map(str::to_owned);
        Ok(match raw["$"].as_str() {
            Some("send") => Input::Sent { id: text("id").unwrap_or_default(), prompt: Prompt { text: text("text").unwrap_or_default(), by: "owner".into(), ..Prompt::default() } },
            Some("answer") => {
                let values = raw["values"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect();
                Input::Answer { request: text("request").unwrap_or_default(), answer: Answer { values, reason: text("reason"), by: text("by").unwrap_or_else(|| "owner".into()) } }
            }
            _ => Input::Frame(Frame::from_json(raw)),
        })
    }
}

/// What answering a dialog sends to omp.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub response: UiResponse,
    /// Steering that tells the model why its tool call was denied.
    pub steer: Option<String>,
}

/// What lives only as long as the open turn.
#[derive(Debug, Default)]
struct Open {
    /// Command id of the prompt the turn runs.
    prompt: Option<String>,
    /// omp hasn't delivered the turn's own user message yet.
    echo: bool,
    calls: HashMap<String, NodeId>,
    messages: HashMap<String, NodeId>,
    jobs: HashMap<String, NodeId>,
    /// Decisions by omp's request id, and the dialogs they answer.
    decisions: HashMap<String, NodeId>,
    requests: HashMap<NodeId, UiRequest>,
    blocks: HashMap<u32, String>,
    text: HashMap<String, String>,
    inputs: HashMap<String, NodeId>,
    input_text: HashMap<String, String>,
    /// Messages, calls and jobs that reached a final status.
    closed: HashSet<NodeId>,
    /// Text typed on a question card whose `select` offered omp's "Other"
    /// row: omp asks for it with the `input` that follows.
    typed: Option<String>,
}

#[derive(Debug)]
pub struct Mapper {
    ledger: TurnLedger<String>,
    open: Open,
    /// Prompts sent to omp that no turn runs yet, oldest first, by command id.
    queued: VecDeque<(String, Prompt)>,
    turns: Vec<NodeId>,
    todos: HashMap<String, NodeId>,
    current_message: Option<String>,
    /// The command list last recorded, so repeats add nothing.
    commands: Option<Json>,
}

impl EventMapper for Mapper {
    type Event = Input;
    type Reply = Reply;

    fn apply(&mut self, input: Input, w: &DomWriter) -> Result<Vec<Reply>> {
        let reply = match input {
            Input::Sent { id, prompt } => {
                self.queued.push_back((id, prompt));
                None
            }
            Input::Answer { request, answer } => {
                let decision = *self.open.decisions.get(&request).ok_or_else(|| AdapterError::Protocol(format!("no dialog `{request}`")))?;
                self.answer(decision, &answer, w)?
            }
            Input::Frame(Frame::Event(event)) => self.event(w, &event).map(|()| None)?,
            Input::Frame(Frame::Ui(request)) => match self.auto_reply(&request) {
                Some(response) => Some(Reply { response, steer: None }),
                None => self.ui(w, &request).map(|()| None)?,
            },
        };
        Ok(reply.into_iter().collect())
    }

    fn answer(&mut self, decision: NodeId, answer: &Answer, w: &DomWriter) -> Result<Option<Reply>> {
        let reply = self.reply(decision, answer)?;
        self.answered(w, decision, answer)?;
        Ok(Some(reply))
    }

    fn ledger(&self) -> &TurnLedger<String> {
        &self.ledger
    }

    fn ledger_mut(&mut self) -> &mut TurnLedger<String> {
        &mut self.ledger
    }
}

impl Mapper {
    pub fn new(session: NodeId) -> Self {
        let ledger = TurnLedger::new(session);
        Self { ledger, open: Open::default(), queued: VecDeque::new(), turns: Vec::new(), todos: HashMap::new(), current_message: None, commands: None }
    }

    pub fn session(&self) -> NodeId {
        self.ledger.session
    }

    fn turn(&self) -> Option<NodeId> {
        self.ledger.turn().map(|t| t.node)
    }

    /// Running while a turn is open or a sent prompt waits for omp.
    pub fn state(&self) -> SessionState {
        match self.ledger.state() {
            SessionState::Idle if !self.queued.is_empty() => SessionState::Running,
            state => state,
        }
    }

    /// Forgets a sent prompt omp refused.
    pub fn withdraw(&mut self, id: &str) {
        self.queued.retain(|(queued, _)| queued != id);
    }

    /// Sets a final `status` on a message, call or job of the current turn.
    fn close(&mut self, w: &DomWriter, node: NodeId, status: impl Into<Value>) -> Out {
        self.open.closed.insert(node);
        w.set(node, attr::STATUS, status)
    }

    /// Opens the turn of the oldest queued prompt (or of omp's own work when
    /// none is queued), ending the current turn first.
    fn open_turn(&mut self, w: &DomWriter) -> Out<NodeId> {
        self.finish(w, TurnStatus::Done)?;
        let prompt = self.queued.pop_front();
        let turn = self.ledger.begin_with(w, prompt.as_ref().map(|(_, p)| p))?;
        w.set(turn, attr::STARTED, w.now())?;
        self.turns.push(turn);
        self.open.echo = prompt.is_some();
        self.open.prompt = prompt.map(|(id, _)| id);
        Ok(turn)
    }

    fn ensure_turn(&mut self, w: &DomWriter) -> Out<NodeId> {
        match self.turn() {
            Some(turn) => Ok(turn),
            None => self.open_turn(w),
        }
    }

    fn event(&mut self, w: &DomWriter, event: &Event) -> Out {
        let command_output = matches!(event, Event::Unknown(raw) if raw["type"] == COMMAND_OUTPUT) && !self.queued.is_empty();
        if needs_turn(event) || command_output {
            self.ensure_turn(w)?;
        }
        let session = self.session();
        match event {
            Event::AgentStart | Event::TurnStart => self.ensure_turn(w).map(drop),
            Event::SessionSettled => self.finish(w, TurnStatus::Done),
            Event::PromptResult { id, status, error, .. } => self.prompt_result(w, id.as_deref(), *status, error.as_ref()),
            Event::MessageStart { message, message_id } => self.message_start(w, message, message_id),
            Event::MessageUpdate { role, event, message_id } if role == "assistant" => self.assistant(w, event, message_id.as_deref()),
            Event::MessageEnd { message, message_id } => self.message_end(w, message, message_id.as_deref()),
            Event::ToolExecutionStart { tool_call_id, tool_name, args, intent } => self.tool_start(w, tool_call_id, tool_name, args.as_ref(), intent.as_deref()),
            Event::ToolExecutionUpdate { tool_call_id, args, partial_result, .. } => self.tool_update(w, tool_call_id, args.as_ref(), partial_result.as_ref()),
            Event::ToolExecutionEnd { tool_call_id, result, is_error, .. } => self.tool_end(w, tool_call_id, result.as_ref(), *is_error),
            Event::TodoReminder { todos } => self.update_todos(w, todos),
            Event::TodoAutoClear => self.todos.values().try_for_each(|id| w.set(*id, attr::STATE, TodoState::Cancelled)),
            Event::SubagentLifecycle(l) => self.subagent(w, l),
            Event::SubagentProgress(p) => self.subagent_progress(w, p),
            Event::CompactionEnd { result: Some(result), aborted: false, skipped: false, .. } => {
                let covers: Vec<Value> = self.turns.iter().map(|id| Value::Int(id.0 as i64)).collect();
                let mut attrs = vec![(attr::COVERS, Value::List(covers))];
                attrs.extend(result.tokens_before.map(|t| (attr::TOKENS_BEFORE, t.into())));
                w.insert(session, K::Compaction, attrs, result.summary.clone()).map(drop)
            }
            Event::ConfigUpdate { model, thinking_level } => {
                model.iter().try_for_each(|m| w.set(session, attr::MODEL, m.selector()))?;
                thinking_level.iter().try_for_each(|l| w.set(session, attr::EFFORT, l.as_str()))
            }
            Event::ThinkingLevelChanged { thinking_level: Some(level) } => w.set(session, attr::EFFORT, level.as_str()),
            Event::SessionInfoUpdate { title, session_id } => {
                w.set(session, attr::NATIVE_ID, session_id.as_str())?;
                title.iter().try_for_each(|t| w.set(session, attr::TITLE, t.as_str()))
            }
            Event::Notice { level, message, source } => self.ext(w, "notice", &json!({"level":level,"message":message,"source":source})),
            Event::Unknown(raw) => self.unknown(w, raw),
            // Model steps inside a turn, and omp's TUI bookkeeping, have no element.
            _ => Ok(()),
        }
    }

    fn message_start(&mut self, w: &DomWriter, message: &Json, message_id: &Option<String>) -> Out {
        self.current_message.clone_from(message_id);
        if !is_user_prompt(message) {
            return Ok(());
        }
        if self.open.echo {
            self.open.echo = false;
        } else if !self.queued.is_empty() {
            // omp delivered a queued follow-up: the work before it is done.
            self.open_turn(w)?;
            self.open.echo = false;
        }
        // Otherwise steering omp took from elsewhere stays in its transcript only.
        Ok(())
    }

    fn message_end(&mut self, w: &DomWriter, message: &Json, message_id: Option<&str>) -> Out {
        let prefix = format!("{}:", message_id.or(self.current_message.as_deref()).unwrap_or(""));
        let status = match message.get("stopReason").and_then(Json::as_str) {
            Some("aborted") => MessageStatus::Cancelled,
            Some("error") => MessageStatus::Error,
            _ => MessageStatus::Done,
        };
        let blocks: Vec<NodeId> = self.open.messages.iter().filter(|(key, _)| key.starts_with(&prefix)).map(|(_, id)| *id).collect();
        for id in blocks.iter().copied().filter(|id| !self.open.closed.contains(id)).collect::<Vec<_>>() {
            self.close(w, id, status)?;
        }
        if message.get("role").and_then(Json::as_str) == Some("assistant") {
            self.current_message = None;
            // Usage and cost of this model call, on its last block.
            if let (Some(target), Some(attrs)) = (blocks.into_iter().max().or(self.turn()), usage(message)) {
                w.insert(target, K::Usage, attrs, "")?;
            }
        }
        Ok(())
    }

    fn tool_start(&mut self, w: &DomWriter, id: &str, tool: &str, args: Option<&Json>, intent: Option<&str>) -> Out {
        let turn = self.ensure_turn(w)?;
        let mut attrs = vec![(attr::TOOL, tool.into()), (attr::CALL_ID, id.into()), (attr::STATUS, CallStatus::Running.into())];
        attrs.extend(intent.map(|i| (attr::TITLE, Value::from(i))));
        let call = w.insert(turn, K::Call, attrs, "")?;
        self.open.calls.insert(id.to_owned(), call);
        if matches!(tool, "bash" | "shell" | "exec" | "remote_bash") {
            let mut attrs = vec![(attr::KIND, Value::from("shell")), (attr::STATUS, JobStatus::Running.into()), (attr::ID, id.into())];
            let command = args.and_then(|v| v.get("command").or_else(|| v.get("cmd"))).and_then(Json::as_str);
            attrs.extend(command.map(|c| (attr::COMMAND, Value::from(c))));
            self.open.jobs.insert(id.to_owned(), w.insert(turn, K::Job, attrs, "")?);
        }
        if let Some(args) = args {
            let text = json_text(args);
            self.open.input_text.insert(id.to_owned(), text.clone());
            self.open.inputs.insert(id.to_owned(), w.insert(call, K::Input, [(attr::FORMAT, "json")], text)?);
        }
        Ok(())
    }

    fn tool_update(&mut self, w: &DomWriter, id: &str, args: Option<&Json>, partial: Option<&Json>) -> Out {
        if let (Some(input), Some(args)) = (self.open.inputs.get(id), args) {
            let value = args.to_string();
            let old = self.open.input_text.entry(id.to_owned()).or_default();
            match value.strip_prefix(old.as_str()) {
                Some("") => {}
                Some(delta) => w.append(*input, delta)?,
                None => w.set(*input, TEXT, value.as_str())?,
            }
            *old = value;
        }
        if let (Some(result), Some(job)) = (partial, self.open.jobs.get(id)) {
            // Each partial result holds the whole output so far.
            w.set(*job, TEXT, tool_text(result))?;
        }
        Ok(())
    }

    fn tool_end(&mut self, w: &DomWriter, id: &str, result: Option<&Json>, is_error: bool) -> Out {
        if let Some(call) = self.open.calls.get(id).copied() {
            w.insert(call, K::Result, [(attr::IS_ERROR, is_error)], result.map(tool_text).unwrap_or_default())?;
            self.close(w, call, if is_error { CallStatus::Error } else { CallStatus::Done })?;
        }
        if let Some(job) = self.open.jobs.get(id).copied() {
            self.close(w, job, if is_error { JobStatus::Error } else { JobStatus::Done })?;
        }
        Ok(())
    }

    fn unknown(&mut self, w: &DomWriter, raw: &Json) -> Out {
        if raw["type"] == COMMANDS {
            let commands = raw.get("commands").cloned().unwrap_or(Json::Null);
            if self.commands.as_ref() != Some(&commands) {
                self.commands = Some(commands);
                let mut raw = raw.clone();
                raw.sort_all_objects();
                w.ext(self.session(), HARNESS, COMMANDS, raw)?;
            }
        } else if raw["type"] == COMMAND_OUTPUT {
            // A slash command omp ran itself answers in its turn. The prompt's
            // response may close that turn before this frame is read; it then
            // joins the turn just closed.
            let text = raw.get("text").and_then(Json::as_str).unwrap_or_default();
            match self.turn().or_else(|| self.turns.last().copied()) {
                Some(turn) => {
                    w.insert(turn, K::Message, [(attr::ROLE, "assistant"), (attr::KIND, MessageKind::Text.as_str()), (attr::STATUS, MessageStatus::Done.as_str())], text)?;
                }
                None => self.ext(w, COMMAND_OUTPUT, raw)?,
            }
        } else {
            tracing::trace!(kind = %raw["type"], "omp event without an element, ignored");
        }
        Ok(())
    }

    fn assistant(&mut self, w: &DomWriter, event: &AssistantEvent, message_id: Option<&str>) -> Out {
        let prefix = message_id.or(self.current_message.as_deref()).unwrap_or("assistant").to_owned();
        match event {
            AssistantEvent::TextStart { content_index } | AssistantEvent::ThinkingStart { content_index } => {
                if self.open.blocks.contains_key(content_index) {
                    return Ok(());
                }
                let key = format!("{prefix}:{content_index}");
                self.open.blocks.insert(*content_index, key.clone());
                let kind = if matches!(event, AssistantEvent::ThinkingStart { .. }) { MessageKind::Thinking } else { MessageKind::Text };
                let turn = self.ensure_turn(w)?;
                let attrs = [(attr::ROLE, "assistant"), (attr::KIND, kind.as_str()), (attr::STATUS, MessageStatus::Streaming.as_str()), (attr::ID, prefix.as_str())];
                self.open.messages.insert(key, w.insert(turn, K::Message, attrs, "")?);
            }
            AssistantEvent::TextDelta { content_index, delta } | AssistantEvent::ThinkingDelta { content_index, delta } => {
                let Some(key) = self.open.blocks.get(content_index) else { return Ok(()) };
                self.open.text.entry(key.clone()).or_default().push_str(delta);
                if let Some(node) = self.open.messages.get(key) {
                    w.append(*node, delta.as_str())?;
                }
            }
            AssistantEvent::TextEnd { content_index, content } | AssistantEvent::ThinkingEnd { content_index, content } => {
                let Some(key) = self.open.blocks.remove(content_index) else { return Ok(()) };
                let old = self.open.text.remove(&key).unwrap_or_default();
                let Some(node) = self.open.messages.get(&key).copied() else { return Ok(()) };
                if old != *content {
                    w.set(node, TEXT, content.as_str())?;
                }
                self.close(w, node, MessageStatus::Done)?;
            }
            AssistantEvent::ToolcallStart { content_index, block, partial } => {
                let value = block.as_ref().or_else(|| partial.as_ref().and_then(|p| p.get("content")?.as_array()?.get(*content_index as usize)));
                if let Some(id) = value.and_then(|v| v.get("id")).and_then(Json::as_str) {
                    self.open.blocks.insert(*content_index, id.to_owned());
                }
            }
            AssistantEvent::ToolcallDelta { content_index, delta } => {
                if let Some(node) = self.open.blocks.get(content_index).and_then(|key| self.open.inputs.get(key)) {
                    w.append(*node, delta.as_str())?;
                }
            }
            AssistantEvent::ToolcallEnd { content_index, tool_call } => {
                self.open.blocks.remove(content_index);
                if let Some(input) = tool_call.get("id").and_then(Json::as_str).and_then(|id| self.open.inputs.get(id)) {
                    w.set(*input, TEXT, tool_call.get("arguments").map(json_text).unwrap_or_default())?;
                }
            }
            AssistantEvent::Done { .. } | AssistantEvent::Error { .. } => {
                let status = if matches!(event, AssistantEvent::Error { .. }) { MessageStatus::Error } else { MessageStatus::Done };
                self.open.blocks.clear();
                for id in self.open.messages.values().copied().filter(|id| !self.open.closed.contains(id)).collect::<Vec<_>>() {
                    self.close(w, id, status)?;
                }
            }
            AssistantEvent::Other => {}
        }
        Ok(())
    }

    fn subagent(&mut self, w: &DomWriter, l: &SubagentLifecycle) -> Out {
        let status = job_status(l.status);
        if let Some(node) = self.open.jobs.get(&l.id).copied() {
            return if status == JobStatus::Running { w.set(node, attr::STATUS, status) } else { self.close(w, node, status) };
        }
        let mut attrs = vec![(attr::KIND, Value::from("subagent")), (attr::STATUS, status.into()), (attr::ID, l.id.as_str().into()), (attr::AGENT, l.agent.as_str().into())];
        attrs.extend(l.description.as_deref().map(|t| (attr::TITLE, t.into())));
        attrs.extend(l.session_file.as_deref().map(|s| (attr::SESSION, s.into())));
        let parent = self.turn().unwrap_or(self.session());
        self.open.jobs.insert(l.id.clone(), w.insert(parent, K::Job, attrs, "")?);
        Ok(())
    }

    fn subagent_progress(&mut self, w: &DomWriter, p: &SubagentProgress) -> Out {
        let job = p.parent_tool_call_id.as_ref().and_then(|s| self.open.jobs.get(&format!("agent:{s}:{}", p.index)));
        match job {
            Some(node) => w.append(*node, format!("{}\n", json_text(&p.progress))),
            None => self.ext(w, "subagent_progress", &json!({"agent":p.agent,"task":p.task,"progress":p.progress,"sessionFile":p.session_file})),
        }
    }

    fn update_todos(&mut self, w: &DomWriter, todos: &[TodoItem]) -> Out {
        for (index, todo) in todos.iter().enumerate() {
            let key = index.to_string();
            let state = todo_state(todo.status);
            match self.todos.get(&key) {
                Some(id) => {
                    w.set(*id, attr::STATE, state)?;
                    w.set(*id, TEXT, todo.content.as_str())?;
                }
                None => {
                    let node = w.insert(self.session(), K::Todo, [(attr::ID, key.as_str()), (attr::STATE, state.as_str())], todo.content.as_str())?;
                    self.todos.insert(key, node);
                }
            }
        }
        let gone = self.todos.iter().filter(|(key, _)| key.parse::<usize>().is_ok_and(|i| i >= todos.len()));
        gone.map(|(_, node)| *node).collect::<Vec<_>>().into_iter().try_for_each(|node| w.set(node, attr::STATE, TodoState::Cancelled))
    }

    fn ui(&mut self, w: &DomWriter, request: &UiRequest) -> Out {
        if !request.is_dialog() {
            return match request {
                UiRequest::Cancel { target_id, .. } => match self.open.decisions.get(target_id).copied() {
                    Some(id) => {
                        self.open.requests.remove(&id);
                        self.ledger.answered(id);
                        w.set(id, attr::STATUS, DecisionStatus::Cancelled)
                    }
                    None => Ok(()),
                },
                UiRequest::Notify { message, notify_type, .. } => self.ext(w, "notice", &json!({"level":notify_type,"message":message,"source":"extension"})),
                UiRequest::SetTitle { title, .. } => w.set(self.session(), attr::TITLE, title.as_str()),
                UiRequest::OpenUrl { url, launch_url, instructions, .. } => self.ext(w, "open_url", &json!({"url":url,"launchUrl":launch_url,"instructions":instructions})),
                // Status lines, widgets and editor text belong to omp's TUI.
                _ => Ok(()),
            };
        }
        let turn = self.ensure_turn(w)?;
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
            _ => return Ok(()),
        };
        let mut attrs = vec![(attr::KIND, kind.into()), (attr::STATUS, DecisionStatus::Waiting.into()), (attr::REQUEST_ID, request.id().into()), (attr::TITLE, title.into()), (attr::OPTIONS, options.into()), (attr::MULTI, multi.into())];
        attrs.extend(timeout.map(|t| (attr::TIMEOUT_MS, Value::Int(t as i64))));
        if let (UiRequest::Ask { questions, .. }, DecisionKind::Form) = (request, kind) {
            attrs.push((attr::FIELDS, form_fields(questions).into()));
        }
        let node = w.insert(turn, K::Decision, attrs, body)?;
        self.open.decisions.insert(request.id().to_owned(), node);
        self.open.requests.insert(node, request.clone());
        self.ledger.wait(node);
        w.set(turn, attr::STATUS, TurnStatus::Waiting)
    }

    /// What answering the waiting `decision` with `answer` sends to omp.
    fn reply(&self, decision: NodeId, answer: &Answer) -> Result<Reply> {
        let request = self.open.requests.get(&decision).ok_or(AdapterError::UnknownDecision(decision))?;
        let id = request.id().to_owned();
        let first = answer.values.first().map(String::as_str);
        let unknown = || AdapterError::Protocol(format!("`{}` isn't an answer to this decision", first.unwrap_or_default()));
        let cancelled = |id| UiResponse::Cancelled { id, timed_out: false };
        let (response, approval) = match request {
            UiRequest::Select { options, .. } => match (Approval::of(options), first) {
                (_, None) => (cancelled(id), false),
                (Some(approval), Some(value)) => (UiResponse::Value { id, value: approval.label(value).ok_or_else(unknown)?.to_owned() }, true),
                // Typed text goes through omp's "Other" row, then its `input`.
                (None, Some(value)) if !options.iter().any(|o| o == value) && options.iter().any(|o| o == OTHER) => (UiResponse::Value { id, value: OTHER.to_owned() }, false),
                (None, Some(value)) => (UiResponse::Value { id, value: value.to_owned() }, false),
            },
            UiRequest::Input { .. } | UiRequest::Editor { .. } => (first.map_or_else(|| cancelled(id.clone()), |value| UiResponse::Value { id: id.clone(), value: value.to_owned() }), false),
            UiRequest::Confirm { .. } => (UiResponse::Confirmed { id, confirmed: matches!(first, Some(answer::ALLOW_ONCE | answer::ALLOW_SESSION | answer::APPROVE | "yes" | "true")) }, true),
            UiRequest::Ask { questions, .. } => (ask_answers(questions, &answer.values).map_or_else(|| cancelled(id.clone()), |answers| UiResponse::Answers { id: id.clone(), answers }), false),
            _ => return Err(AdapterError::UnknownDecision(decision)),
        };
        // omp's approval prompt carries no reason; the model reads it as steering.
        let reason = answer.reason.as_deref().map(str::trim).filter(|reason| !reason.is_empty());
        let steer = reason.filter(|_| approval && first == Some(answer::DENY)).map(|reason| format!("I denied that tool call: {reason}"));
        Ok(Reply { response, steer })
    }

    fn answered(&mut self, w: &DomWriter, id: NodeId, answer: &Answer) -> Out {
        w.set(id, attr::ANSWER, Value::List(answer.values.iter().cloned().map(Value::Str).collect()))?;
        w.set(id, attr::BY, answer.by.as_str())?;
        w.set(id, attr::STATUS, DecisionStatus::Answered)?;
        answer.reason.iter().try_for_each(|reason| w.set(id, attr::REASON, reason.as_str()))?;
        self.ledger.answered(id);
        self.turn().iter().try_for_each(|turn| w.set(*turn, attr::STATUS, TurnStatus::Running))?;
        if let Some(UiRequest::Select { options, .. }) = self.open.requests.remove(&id)
            && let Some(value) = answer.values.first()
            && Approval::of(&options).is_none()
            && !options.contains(value)
            && options.iter().any(|o| o == OTHER)
        {
            self.open.typed = Some(value.clone());
        }
        Ok(())
    }

    /// The answer to a dialog omp asks without a card: the `input` omp opens
    /// after its "Other" row was picked gets the text typed on the card.
    fn auto_reply(&mut self, request: &UiRequest) -> Option<UiResponse> {
        match (request, request.is_dialog().then(|| self.open.typed.take()).flatten()) {
            (UiRequest::Input { id, .. }, Some(value)) => Some(UiResponse::Value { id: id.clone(), value }),
            _ => None,
        }
    }

    fn prompt_result(&mut self, w: &DomWriter, id: Option<&str>, status: PromptStatus, error: Option<&PromptError>) -> Out {
        let status = match status {
            PromptStatus::Completed => TurnStatus::Done,
            PromptStatus::Aborted => TurnStatus::Cancelled,
            PromptStatus::Error => TurnStatus::Error,
        };
        if let Some(index) = id.and_then(|id| self.queued.iter().position(|(queued, _)| queued == id)) {
            // omp finished the prompt without running it: an abort dropped it
            // from omp's queue, or it was handled locally.
            let (_, prompt) = self.queued.remove(index).expect("position is in the queue");
            let turn = w.insert(self.session(), K::Turn, [(attr::STATUS, status.as_str())], "")?;
            w.set(turn, attr::ENDED, w.now())?;
            self.turns.push(turn);
            w.insert(turn, K::Message, [(attr::ROLE, "user"), (attr::STATUS, MessageStatus::Done.as_str()), (attr::BY, prompt.by.as_str())], prompt.text.as_str())?;
        } else if id.is_none() || self.open.prompt.is_none() || self.open.prompt.as_deref() == id {
            self.finish(w, status)?;
        }
        // Otherwise an earlier turn's prompt, settled at the same yield as the current one.
        match error {
            Some(e) => self.ext(w, "prompt_error", &json!({"message":e.message,"provider":e.provider,"model":e.model,"code":e.code,"retryable":e.retryable})),
            None => Ok(()),
        }
    }

    /// Ends the current turn. Nothing in it is left streaming, running or waiting.
    fn finish(&mut self, w: &DomWriter, status: TurnStatus) -> Out {
        let Some(turn) = self.turn() else { return Ok(()) };
        let message = match status {
            TurnStatus::Cancelled => MessageStatus::Cancelled,
            TurnStatus::Error => MessageStatus::Error,
            _ => MessageStatus::Done,
        };
        let open = take(&mut self.open);
        let still = |map: &HashMap<String, NodeId>| map.values().copied().filter(|id| !open.closed.contains(id)).collect::<Vec<_>>();
        still(&open.messages).into_iter().try_for_each(|id| w.set(id, attr::STATUS, message))?;
        if status == TurnStatus::Cancelled {
            still(&open.calls).into_iter().try_for_each(|id| w.set(id, attr::STATUS, CallStatus::Cancelled))?;
            still(&open.jobs).into_iter().try_for_each(|id| w.set(id, attr::STATUS, JobStatus::Cancelled))?;
        }
        open.requests.keys().try_for_each(|id| w.set(*id, attr::STATUS, DecisionStatus::Cancelled))?;
        w.close(turn, status)?;
        // The turn closed here, so the ledger starts over instead of ending it.
        self.ledger = TurnLedger::new(self.session());
        Ok(())
    }

    fn ext(&self, w: &DomWriter, kind: &str, raw: &Json) -> Out {
        let mut raw = raw.clone();
        raw.sort_all_objects();
        w.ext(self.turn().unwrap_or(self.session()), HARNESS, kind, raw).map(drop)
    }
}

fn needs_turn(event: &Event) -> bool {
    matches!(event, Event::MessageStart { .. } | Event::MessageUpdate { .. } | Event::MessageEnd { .. } | Event::ToolExecutionStart { .. } | Event::ToolExecutionUpdate { .. } | Event::ToolExecutionEnd { .. } | Event::SubagentLifecycle(_) | Event::SubagentProgress(_))
}

fn labels(question: &AskQuestion) -> Vec<String> {
    question.options.iter().map(|o| o.label.clone()).collect()
}

/// JSON Schema of the form card for an `ask` with several questions: one
/// field per question, keyed by its id.
fn form_fields(questions: &[AskQuestion]) -> String {
    let properties: serde_json::Map<String, Json> = questions
        .iter()
        .map(|q| {
            let labels = labels(q);
            let mut field = json!({"title": q.question});
            if let Some(header) = &q.header {
                field["description"] = header.as_str().into();
            }
            if q.multi {
                field["type"] = "array".into();
                field["items"] = json!({"type": "string", "enum": labels});
            } else {
                field["type"] = "string".into();
                if let Some(default) = q.recommended.and_then(|index| labels.get(index as usize)) {
                    field["default"] = default.as_str().into();
                }
                if !labels.is_empty() {
                    field["enum"] = labels.into();
                }
            }
            (q.id.clone(), field)
        })
        .collect();
    let required: Vec<&str> = questions.iter().map(|q| q.id.as_str()).collect();
    let mut schema = json!({"type": "object", "properties": properties, "required": required});
    schema.sort_all_objects();
    schema.to_string()
}

/// omp's answers to an `ask`: a question card answers picked labels or typed
/// text, a form card one JSON object keyed by question id. `None` cancels.
fn ask_answers(questions: &[AskQuestion], values: &[String]) -> Option<Vec<AskAnswer>> {
    let answer = |q: &AskQuestion, selected_options: Vec<String>, custom_input: Option<String>| AskAnswer { id: q.id.clone(), selected_options, custom_input };
    if let [q] = questions {
        if values.is_empty() {
            return None;
        }
        let labels = labels(q);
        let picked = values.iter().all(|v| labels.contains(v));
        return Some(vec![if picked { answer(q, values.to_vec(), None) } else { answer(q, Vec::new(), Some(values.join("\n"))) }]);
    }
    let [object] = values else { return None };
    let Ok(Json::Object(fields)) = serde_json::from_str::<Json>(object) else { return None };
    let pick = |q: &AskQuestion| match fields.get(&q.id) {
        Some(Json::String(text)) if q.options.iter().any(|o| o.label == *text) => answer(q, vec![text.clone()], None),
        Some(Json::String(text)) => answer(q, Vec::new(), Some(text.clone())),
        Some(Json::Array(items)) => answer(q, items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect(), None),
        _ => answer(q, Vec::new(), None),
    };
    Some(questions.iter().map(pick).collect())
}

/// The row omp's `ask` tool adds to a question's `select` for typed answers.
const OTHER: &str = "Other (type your own)";

/// omp's tool approval prompt: a `select` whose last option is `Deny`, with
/// `Approve` (`Approve Once` when it also offers file and folder grants) and,
/// for prompts the approval mode raised, `Approve <tool> for this session`.
struct Approval<'a> {
    once: &'a str,
    session: Option<&'a str>,
    deny: &'a str,
    options: &'a [String],
}

impl<'a> Approval<'a> {
    fn of(options: &'a [String]) -> Option<Self> {
        let (deny, rest) = options.split_last()?;
        if deny != "Deny" {
            return None;
        }
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

fn json_text(value: &Json) -> String {
    value.as_str().map_or_else(|| value.to_string(), str::to_owned)
}

/// Text of a tool result: its `content` text blocks, as in imported sessions.
fn tool_text(value: &Json) -> String {
    value.get("content").map_or_else(|| json_text(value), |content| super::native::content_text(Some(content)))
}

/// `<usage>` attributes of an omp message's `usage` (token counts, total
/// cost) and its `provider/model`; `None` without usage.
fn usage(message: &Json) -> Option<Vec<(&'static str, Value)>> {
    let usage = message.get("usage")?.as_object()?;
    let counts = [(attr::INPUT_TOKENS, "input"), (attr::OUTPUT_TOKENS, "output"), (attr::CACHE_READ_TOKENS, "cacheRead"), (attr::CACHE_WRITE_TOKENS, "cacheWrite")];
    let mut attrs: Vec<(&str, Value)> = counts.into_iter().filter_map(|(name, key)| Some((name, usage.get(key)?.as_i64()?.into()))).collect();
    attrs.extend(usage.get("cost").and_then(|cost| cost.get("total")).and_then(Json::as_f64).map(|cost| (attr::COST, cost.into())));
    let field = |key| message.get(key).and_then(Json::as_str);
    let model = match (field("provider"), field("model")) {
        (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
        (provider, model) => model.or(provider).map(str::to_owned),
    };
    attrs.extend(model.map(|model| (attr::MODEL, model.into())));
    Some(attrs)
}

/// A user message from a host prompt, not one an agent or extension wrote.
fn is_user_prompt(message: &Json) -> bool {
    message.get("role").and_then(Json::as_str) == Some("user") && message.get("attribution").and_then(Json::as_str).is_none_or(|by| by == "user")
}

fn todo_state(status: TodoStatus) -> TodoState {
    match status {
        TodoStatus::Pending => TodoState::Pending,
        TodoStatus::InProgress => TodoState::InProgress,
        TodoStatus::Completed => TodoState::Done,
        TodoStatus::Abandoned => TodoState::Cancelled,
    }
}

fn job_status(status: SubagentStatus) -> JobStatus {
    match status {
        SubagentStatus::Started => JobStatus::Running,
        SubagentStatus::Completed => JobStatus::Done,
        SubagentStatus::Failed => JobStatus::Error,
        SubagentStatus::Aborted => JobStatus::Cancelled,
    }
}
