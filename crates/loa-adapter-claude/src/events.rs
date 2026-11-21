//! Claude Code's stream JSON messages as DOM patches.
//!
//! [`Mapper`] is a pure state machine: it takes one parsed stdout message at a
//! time and commits the drafts the DOM conventions call for. It never does
//! I/O. Whatever has to go back to the CLI (control responses, SDK MCP
//! requests) comes out as an [`Output`] for the driver to act on.

use crate::HARNESS;
use loa_contracts::harness::{AdapterError, Answer, Result, SessionConfig, SessionState};
use loa_core::{
    CallStatus, DecisionKind, DecisionStatus, Dom, Draft, ElementKind, JobKind, JobStatus, MessageKind,
    MessageStatus, NodeId, PatchSink, Role, SCHEMA_VERSION, SessionMode, TurnStatus, Value as V, answer, attr,
    now_ms,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// Something the driver must do after a message was mapped.
#[derive(Debug, PartialEq)]
pub enum Output {
    /// A line to write to the CLI's stdin.
    Write(Value),
    /// The CLI forwards a JSON-RPC message to an SDK MCP server the host
    /// serves. Answer with [`mcp_response`].
    Mcp { request_id: String, server: String, message: Value },
    /// The CLI answered a control request the host sent.
    ControlResponse { request_id: String, result: std::result::Result<Value, String> },
}

/// A successful `control_response` line.
pub fn control_success(request_id: &str, response: Value) -> Value {
    json!({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": response}})
}

/// A failed `control_response` line.
pub fn control_error(request_id: &str, error: &str) -> Value {
    json!({"type": "control_response", "response": {"subtype": "error", "request_id": request_id, "error": error}})
}

/// The `control_response` line answering an `mcp_message` request.
pub fn mcp_response(request_id: &str, message: Value) -> Value {
    control_success(request_id, json!({ "mcp_response": message }))
}

/// Tools whose call starts a subagent.
const SUBAGENT_TOOLS: [&str; 2] = ["Task", "Agent"];
const ASK_TOOL: &str = "AskUserQuestion";
const PLAN_TOOL: &str = "ExitPlanMode";

/// Attribute holding the uuid of the transcript line a `<call>` or `<result>`
/// came from. Messages keep theirs in `id`.
pub const UUID_ATTR: &str = "x-claude-uuid";

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
    ToolUse,
    Other,
}

/// One content block of the API message being streamed.
#[derive(Debug)]
struct Block {
    kind: BlockKind,
    /// `<message>` or `<call>`.
    node: Option<NodeId>,
    /// `<input>` of a call.
    input: Option<NodeId>,
    /// Text or arguments arrived as deltas.
    streamed: bool,
}

/// The API message whose `stream_event`s are arriving.
#[derive(Debug)]
struct Stream {
    message_id: String,
    model: Option<String>,
    blocks: Vec<Block>,
    /// Blocks already matched to a complete `assistant` message.
    finalized: usize,
}

#[derive(Debug)]
struct Turn {
    node: NodeId,
    stream: Option<Stream>,
    /// Messages still `streaming`.
    open_messages: Vec<NodeId>,
    /// Calls still `running`.
    running_calls: Vec<NodeId>,
}

#[derive(Debug)]
struct Call {
    node: NodeId,
    /// Subagent started by this call that ends with the call's result.
    job: Option<NodeId>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PendingKind {
    Approval,
    Question,
    Plan,
}

/// A `can_use_tool` request waiting for the user.
#[derive(Debug)]
struct Pending {
    request_id: String,
    kind: PendingKind,
    tool: String,
    input: Value,
    suggestions: Option<Value>,
    /// Decision nodes and their answers so far. A question request has one
    /// decision per question.
    decisions: Vec<(NodeId, Option<Answer>)>,
}

/// Maps one Claude Code session's stream onto its DOM.
pub struct Mapper {
    sink: Arc<dyn PatchSink>,
    session: NodeId,
    native_id: Option<String>,
    model: Option<String>,
    mode: Option<SessionMode>,
    turn: Option<Turn>,
    calls: HashMap<String, Call>,
    /// Jobs by tool use id and by task id.
    jobs: HashMap<String, NodeId>,
    pending: Vec<Pending>,
    /// Turns since the last compaction, for `<compaction covers>`.
    uncompacted: Vec<NodeId>,
    /// Compaction still waiting for its summary text.
    open_compaction: Option<NodeId>,
    /// Interrupted turns whose `result` hasn't arrived yet. Their late events
    /// are dropped.
    draining: u32,
}

impl Mapper {
    /// Inserts the `<session>` root for a new session.
    pub fn create(sink: Arc<dyn PatchSink>, config: &SessionConfig) -> Result<Self> {
        let mut attrs = vec![
            (attr::ID, V::from(config.session_id.as_str())),
            (attr::HARNESS, V::from(HARNESS)),
            (attr::NODE, V::from(config.node.as_str())),
            (attr::CREATED, V::Int(now_ms())),
            (attr::SCHEMA, V::Int(i64::from(SCHEMA_VERSION))),
            (attr::CWD, V::from(config.cwd.to_string_lossy().into_owned())),
        ];
        let optional = [
            (attr::PROJECT, &config.project),
            (attr::MODEL, &config.model),
            (attr::MODEL_ROLE, &config.model_role),
            (attr::EFFORT, &config.effort),
        ];
        attrs.extend(optional.into_iter().filter_map(|(name, value)| Some((name, V::from(value.clone()?)))));
        if let Some(mode) = config.mode {
            attrs.push((attr::MODE, mode.into()));
        }
        let session = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?.node;
        Ok(Self::empty(sink, session, None, config.model.clone(), config.mode))
    }

    /// Continues a session loaded from its journal. A turn left running or
    /// waiting by a harness process that is gone is cancelled.
    pub fn attach(sink: Arc<dyn PatchSink>, dom: &Dom) -> Result<Self> {
        let root = dom.root().ok_or_else(|| AdapterError::Format("the session DOM has no root".into()))?;
        let mode = root.str_attr(attr::MODE).and_then(SessionMode::parse);
        let mut mapper = Self::empty(
            sink,
            root.id,
            root.str_attr(attr::NATIVE_ID).map(str::to_owned),
            root.str_attr(attr::MODEL).map(str::to_owned),
            mode,
        );
        for child in dom.children(root.id) {
            match child.kind {
                ElementKind::Turn => mapper.uncompacted.push(child.id),
                ElementKind::Compaction => mapper.uncompacted.clear(),
                _ => {}
            }
        }
        let live = |status: Option<&str>, values: &[&str]| status.is_some_and(|s| values.contains(&s));
        let stale_turns: Vec<NodeId> = dom
            .children_of_kind(root.id, ElementKind::Turn)
            .filter(|t| live(t.str_attr(attr::STATUS), &[TurnStatus::Running.as_str(), TurnStatus::Waiting.as_str()]))
            .map(|t| t.id)
            .collect();
        for turn in stale_turns {
            let mut state = Turn { node: turn, stream: None, open_messages: Vec::new(), running_calls: Vec::new() };
            for node in dom.descendants(turn) {
                let status = node.str_attr(attr::STATUS);
                match node.kind {
                    ElementKind::Message if live(status, &[MessageStatus::Streaming.as_str()]) => {
                        state.open_messages.push(node.id);
                    }
                    ElementKind::Call
                        if live(status, &[CallStatus::Running.as_str(), CallStatus::Pending.as_str()]) =>
                    {
                        state.running_calls.push(node.id);
                    }
                    ElementKind::Decision if live(status, &[DecisionStatus::Waiting.as_str()]) => {
                        mapper.set(node.id, attr::STATUS, DecisionStatus::Cancelled)?;
                        mapper.set(node.id, attr::ENDED, V::Int(now_ms()))?;
                    }
                    _ => {}
                }
            }
            mapper.turn = Some(state);
            mapper.close_turn(TurnStatus::Cancelled)?;
        }
        Ok(mapper)
    }

    fn empty(
        sink: Arc<dyn PatchSink>,
        session: NodeId,
        native_id: Option<String>,
        model: Option<String>,
        mode: Option<SessionMode>,
    ) -> Self {
        Self {
            sink,
            session,
            native_id,
            model,
            mode,
            turn: None,
            calls: HashMap::new(),
            jobs: HashMap::new(),
            pending: Vec::new(),
            uncompacted: Vec::new(),
            open_compaction: None,
            draining: 0,
        }
    }

    pub fn session(&self) -> NodeId {
        self.session
    }

    /// Claude Code's own session id, from `system/init`.
    pub fn native_id(&self) -> Option<&str> {
        self.native_id.as_deref()
    }

    pub fn state(&self) -> SessionState {
        match (&self.turn, self.pending.is_empty()) {
            (None, _) => SessionState::Idle,
            (Some(_), true) => SessionState::Running,
            (Some(_), false) => SessionState::Waiting,
        }
    }

    pub fn turn_open(&self) -> bool {
        self.turn.is_some()
    }

    /// Records a prompt: inserts the `<turn>` and the user `<message>`.
    /// `uuid` is the id the prompt is sent to the CLI with.
    pub fn open_turn(&mut self, text: &str, by: &str, uuid: &str) -> Result<NodeId> {
        if self.turn.is_some() {
            return Err(AdapterError::Busy);
        }
        let turn = self.begin_turn(uuid)?;
        let attrs = [
            (attr::ROLE, V::from(Role::User)),
            (attr::STATUS, MessageStatus::Done.into()),
            (attr::ID, uuid.into()),
            (attr::BY, by.into()),
        ];
        self.sink.commit(Draft::insert(turn, ElementKind::Message, attrs, text))?;
        Ok(turn)
    }

    fn begin_turn(&mut self, id: &str) -> Result<NodeId> {
        let mut attrs = vec![(attr::STATUS, V::from(TurnStatus::Running)), (attr::STARTED, V::Int(now_ms()))];
        if !id.is_empty() {
            attrs.push((attr::ID, id.into()));
        }
        let node = self.insert(self.session, ElementKind::Turn, attrs, String::new())?;
        self.uncompacted.push(node);
        self.turn = Some(Turn { node, stream: None, open_messages: Vec::new(), running_calls: Vec::new() });
        Ok(node)
    }

    /// The turn new events belong to. Claude Code can start a turn on its own
    /// (a background task finished), which opens one without a prompt.
    /// Claude Code writes an interrupted turn's last events and its `result`
    /// before anything of the next turn, so until that `result` arrives every
    /// event is the interrupted turn's and has no turn.
    fn current_turn(&mut self) -> Result<Option<NodeId>> {
        if self.draining > 0 {
            return Ok(None);
        }
        if let Some(turn) = &self.turn {
            return Ok(Some(turn.node));
        }
        self.begin_turn("").map(Some)
    }

    /// Sets `model` on the session after a successful switch.
    pub fn record_model(&mut self, model: &str) -> Result<()> {
        if self.model.as_deref() != Some(model) {
            self.model = Some(model.to_owned());
            self.set(self.session, attr::MODEL, model)?;
        }
        Ok(())
    }

    /// Sets `mode` on the session after a successful switch.
    pub fn record_mode(&mut self, mode: SessionMode) -> Result<()> {
        if self.mode != Some(mode) {
            self.mode = Some(mode);
            self.set(self.session, attr::MODE, mode)?;
        }
        Ok(())
    }

    /// Maps one stdout message.
    pub fn handle(&mut self, mut msg: Value) -> Result<Option<Output>> {
        let kind = str_of(&msg, "type").unwrap_or_default().to_owned();
        match kind.as_str() {
            "system" => self.on_system(msg).map(|()| None),
            "stream_event" => self.on_stream_event(&mut msg).map(|()| None),
            "assistant" => self.on_assistant(&mut msg).map(|()| None),
            "user" => self.on_user(&mut msg).map(|()| None),
            "result" => self.on_result(&msg).map(|()| None),
            "control_request" => self.on_control_request(msg),
            "control_response" => Ok(Some(on_control_response(&mut msg))),
            "control_cancel_request" => self.on_cancel(&msg).map(|()| None),
            // Liveness and progress pings carry nothing worth keeping.
            "keep_alive" | "tool_progress" => Ok(None),
            _ => self.ext(&kind, &msg).map(|()| None),
        }
    }

    fn on_system(&mut self, msg: Value) -> Result<()> {
        match str_of(&msg, "subtype").unwrap_or_default() {
            "init" => {
                if let Some(id) = str_of(&msg, "session_id")
                    && self.native_id.as_deref() != Some(id)
                {
                    self.native_id = Some(id.to_owned());
                    self.set(self.session, attr::NATIVE_ID, id)?;
                }
                if let Some(model) = str_of(&msg, "model") {
                    self.record_model(model)?;
                }
                if let Some(mode) = str_of(&msg, "permissionMode") {
                    self.record_mode(mode_of(mode))?;
                }
                Ok(())
            }
            "status" => match str_of(&msg, "permissionMode") {
                Some(mode) => self.record_mode(mode_of(mode)),
                None => Ok(()),
            },
            "compact_boundary" => {
                let covers: Vec<V> = self
                    .uncompacted
                    .iter()
                    .filter(|t| self.turn.as_ref().is_none_or(|open| open.node != **t))
                    .map(|t| V::Int(node_int(*t)))
                    .collect();
                let mut attrs = vec![(attr::COVERS, V::List(covers)), (attr::CREATED, V::Int(now_ms()))];
                if let Some(tokens) = msg.pointer("/compact_metadata/pre_tokens").and_then(Value::as_i64) {
                    attrs.push((attr::TOKENS_BEFORE, V::Int(tokens)));
                }
                let node = self.insert(self.session, ElementKind::Compaction, attrs, String::new())?;
                self.open_compaction = Some(node);
                self.uncompacted.retain(|t| self.turn.as_ref().is_some_and(|open| open.node == *t));
                Ok(())
            }
            "task_started" => self.on_task_started(&msg),
            "task_notification" => {
                let status = match str_of(&msg, "status") {
                    Some("completed") => JobStatus::Done,
                    Some("failed") => JobStatus::Error,
                    _ => JobStatus::Cancelled,
                };
                self.end_task(&msg, status, str_of(&msg, "summary"))
            }
            "task_updated" => {
                let status = match msg.pointer("/patch/status").and_then(Value::as_str) {
                    Some("completed") => JobStatus::Done,
                    Some("failed") => JobStatus::Error,
                    Some("killed" | "stopped") => JobStatus::Cancelled,
                    _ => return Ok(()),
                };
                self.end_task(&msg, status, None)
            }
            "task_progress" => Ok(()),
            _ => self.ext("system", &msg),
        }
    }

    fn on_task_started(&mut self, msg: &Value) -> Result<()> {
        let Some(task) = str_of(msg, "task_id") else { return self.ext("system", msg) };
        let tool_use = str_of(msg, "tool_use_id");
        // A subagent call already made its job; the task id now names it too.
        if let Some(job) = tool_use.and_then(|id| self.jobs.get(id)).copied() {
            self.jobs.insert(task.to_owned(), job);
            return Ok(());
        }
        let kind = match str_of(msg, "task_type") {
            Some(t) if t.contains("bash") || t.contains("shell") => JobKind::Shell,
            _ => JobKind::Subagent,
        };
        let call = tool_use.and_then(|id| self.calls.get(id)).map(|c| c.node);
        let mut attrs = vec![
            (attr::KIND, V::from(kind)),
            (attr::STATUS, JobStatus::Running.into()),
            (attr::ID, task.into()),
            (attr::STARTED, V::Int(now_ms())),
        ];
        if let Some(description) = str_of(msg, "description") {
            attrs.push((attr::TITLE, description.into()));
        }
        let parent = match call {
            Some(call) => {
                attrs.push((attr::CALL, V::Int(node_int(call))));
                call
            }
            None => self.turn.as_ref().map_or(self.session, |t| t.node),
        };
        let job = self.insert(parent, ElementKind::Job, attrs, String::new())?;
        self.jobs.insert(task.to_owned(), job);
        if let Some(id) = tool_use {
            self.jobs.insert(id.to_owned(), job);
        }
        Ok(())
    }

    fn end_task(&mut self, msg: &Value, status: JobStatus, summary: Option<&str>) -> Result<()> {
        let Some(job) = str_of(msg, "task_id").and_then(|id| self.jobs.get(id)).copied() else {
            return Ok(());
        };
        if let Some(summary) = summary.filter(|s| !s.is_empty()) {
            self.sink.commit(Draft::append(job, summary))?;
        }
        self.finish_job(job, status)
    }

    fn finish_job(&mut self, job: NodeId, status: JobStatus) -> Result<()> {
        self.set(job, attr::STATUS, status)?;
        self.set(job, attr::ENDED, V::Int(now_ms()))
    }

    fn on_stream_event(&mut self, msg: &mut Value) -> Result<()> {
        // Subagent streams are summarized from their complete messages.
        if msg.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
            return Ok(());
        }
        let Some(event) = msg.get_mut("event") else { return Ok(()) };
        match str_of(event, "type").unwrap_or_default() {
            "message_start" => {
                if self.current_turn()?.is_none() {
                    return Ok(());
                }
                let message_id = event.pointer("/message/id").and_then(Value::as_str).unwrap_or_default().to_owned();
                let model = event.pointer("/message/model").and_then(Value::as_str).map(str::to_owned);
                if let Some(turn) = &mut self.turn {
                    turn.stream = Some(Stream { message_id, model, blocks: Vec::new(), finalized: 0 });
                }
                Ok(())
            }
            "content_block_start" => self.on_block_start(event),
            "content_block_delta" => self.on_block_delta(event),
            "content_block_stop" => {
                let Some(index) = index_of(event) else { return Ok(()) };
                let Some(block) = self.stream_block(index) else { return Ok(()) };
                if let (BlockKind::Text | BlockKind::Thinking, Some(node)) = (block.kind, block.node) {
                    self.finish_message(node)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn stream_block(&self, index: usize) -> Option<&Block> {
        self.turn.as_ref()?.stream.as_ref()?.blocks.get(index)
    }

    fn on_block_start(&mut self, event: &mut Value) -> Result<()> {
        let Some(index) = index_of(event) else { return Ok(()) };
        let Some(turn) = self.turn.as_ref().filter(|t| t.stream.is_some()).map(|t| t.node) else {
            return Ok(());
        };
        let model = self.turn.as_ref().and_then(|t| t.stream.as_ref()).and_then(|s| s.model.clone());
        let Some(content) = event.get_mut("content_block") else { return Ok(()) };
        let block = match str_of(content, "type").unwrap_or_default() {
            "text" => {
                let text = take_string(content, "text");
                let node = self.open_message(turn, MessageKind::Text, model, text)?;
                Block { kind: BlockKind::Text, node: Some(node), input: None, streamed: false }
            }
            "thinking" => {
                let text = take_string(content, "thinking");
                let node = self.open_message(turn, MessageKind::Thinking, model, text)?;
                Block { kind: BlockKind::Thinking, node: Some(node), input: None, streamed: false }
            }
            "tool_use" | "server_tool_use" | "mcp_tool_use" => {
                let id = str_of(content, "id").unwrap_or_default().to_owned();
                let tool = str_of(content, "name").unwrap_or_default().to_owned();
                let (call, input) = self.open_call(turn, &id, &tool, None)?;
                Block { kind: BlockKind::ToolUse, node: Some(call), input: Some(input), streamed: false }
            }
            _ => Block { kind: BlockKind::Other, node: None, input: None, streamed: false },
        };
        let Some(stream) = self.turn.as_mut().and_then(|t| t.stream.as_mut()) else { return Ok(()) };
        while stream.blocks.len() < index {
            stream.blocks.push(Block { kind: BlockKind::Other, node: None, input: None, streamed: false });
        }
        stream.blocks.push(block);
        Ok(())
    }

    fn on_block_delta(&mut self, event: &mut Value) -> Result<()> {
        let Some(index) = index_of(event) else { return Ok(()) };
        let Some(block) = self.turn.as_mut().and_then(|t| t.stream.as_mut()).and_then(|s| s.blocks.get_mut(index))
        else {
            return Ok(());
        };
        let Some(delta) = event.get_mut("delta") else { return Ok(()) };
        let (target, field) = match (str_of(delta, "type").unwrap_or_default(), block.kind) {
            ("text_delta", BlockKind::Text) => (block.node, "text"),
            ("thinking_delta", BlockKind::Thinking) => (block.node, "thinking"),
            ("input_json_delta", BlockKind::ToolUse) => (block.input, "partial_json"),
            _ => return Ok(()),
        };
        let text = take_string(delta, field);
        let Some(target) = target.filter(|_| !text.is_empty()) else { return Ok(()) };
        block.streamed = true;
        self.sink.commit(Draft::append(target, text))?;
        Ok(())
    }

    fn open_message(&mut self, turn: NodeId, kind: MessageKind, model: Option<String>, text: String) -> Result<NodeId> {
        let mut attrs = vec![(attr::ROLE, V::from(Role::Assistant)), (attr::STATUS, MessageStatus::Streaming.into())];
        if kind == MessageKind::Thinking {
            attrs.push((attr::KIND, kind.into()));
        }
        if let Some(model) = model {
            attrs.push((attr::MODEL, model.into()));
        }
        let node = self.insert(turn, ElementKind::Message, attrs, text)?;
        if let Some(turn) = &mut self.turn {
            turn.open_messages.push(node);
        }
        Ok(node)
    }

    fn finish_message(&mut self, node: NodeId) -> Result<()> {
        let Some(turn) = &mut self.turn else { return Ok(()) };
        let Some(at) = turn.open_messages.iter().position(|m| *m == node) else { return Ok(()) };
        turn.open_messages.swap_remove(at);
        self.set(node, attr::STATUS, MessageStatus::Done)
    }

    /// Inserts a running `<call>` with its `<input>`.
    fn open_call(&mut self, turn: NodeId, id: &str, tool: &str, input: Option<String>) -> Result<(NodeId, NodeId)> {
        let attrs = [
            (attr::TOOL, V::from(tool)),
            (attr::CALL_ID, id.into()),
            (attr::STATUS, CallStatus::Running.into()),
            (attr::STARTED, V::Int(now_ms())),
        ];
        let call = self.insert(turn, ElementKind::Call, attrs, String::new())?;
        let input_node = self.insert(call, ElementKind::Input, [(attr::FORMAT, V::from("json"))], input.unwrap_or_default())?;
        if let Some(turn) = &mut self.turn {
            turn.running_calls.push(call);
        }
        self.calls.insert(id.to_owned(), Call { node: call, job: None });
        Ok((call, input_node))
    }

    /// A tool call's arguments are complete: title it and start the subagent
    /// job a `Task` or `Agent` call runs.
    fn complete_call_input(&mut self, call: NodeId, id: &str, tool: &str, input: &Value) -> Result<()> {
        if let Some(title) = call_title(input) {
            self.set(call, attr::TITLE, title)?;
        }
        if !SUBAGENT_TOOLS.contains(&tool) || self.jobs.contains_key(id) {
            return Ok(());
        }
        let mut attrs = vec![
            (attr::KIND, V::from(JobKind::Subagent)),
            (attr::STATUS, JobStatus::Running.into()),
            (attr::CALL, V::Int(node_int(call))),
            (attr::STARTED, V::Int(now_ms())),
        ];
        if let Some(agent) = str_of(input, "subagent_type") {
            attrs.push((attr::AGENT, agent.into()));
        }
        if let Some(description) = str_of(input, "description") {
            attrs.push((attr::TITLE, description.into()));
        }
        let job = self.insert(call, ElementKind::Job, attrs, String::new())?;
        self.jobs.insert(id.to_owned(), job);
        let background = input.get("run_in_background").and_then(Value::as_bool).unwrap_or(false);
        if !background && let Some(entry) = self.calls.get_mut(id) {
            entry.job = Some(job);
        }
        Ok(())
    }

    fn on_assistant(&mut self, msg: &mut Value) -> Result<()> {
        if let Some(parent) = msg.get("parent_tool_use_id").and_then(Value::as_str) {
            return self.on_subagent_message(parent, msg);
        }
        let Some(turn) = self.current_turn()? else { return Ok(()) };
        let uuid = str_of(msg, "uuid").map(str::to_owned);
        let Some(message) = msg.get_mut("message") else { return Ok(()) };
        let message_id = str_of(message, "id").unwrap_or_default().to_owned();
        let model = str_of(message, "model").map(str::to_owned);
        let Some(Value::Array(content)) = message.get_mut("content") else { return Ok(()) };
        for mut block in std::mem::take(content) {
            let kind = match str_of(&block, "type").unwrap_or_default() {
                "text" => BlockKind::Text,
                "thinking" => BlockKind::Thinking,
                "tool_use" | "server_tool_use" | "mcp_tool_use" => BlockKind::ToolUse,
                _ => BlockKind::Other,
            };
            match self.next_streamed(&message_id, kind) {
                Some(streamed) => self.finalize_block(streamed, &mut block, uuid.as_deref())?,
                None => self.insert_block(turn, kind, block, model.clone(), uuid.as_deref())?,
            }
        }
        Ok(())
    }

    /// Takes the next streamed block of `message_id` when it has `kind`.
    fn next_streamed(&mut self, message_id: &str, kind: BlockKind) -> Option<(BlockKind, Option<NodeId>, Option<NodeId>, bool)> {
        let stream = self.turn.as_mut()?.stream.as_mut().filter(|s| s.message_id == message_id)?;
        // Blocks the stream had no element for (signatures, redactions) are
        // skipped so text and tool blocks line up.
        while stream.blocks.get(stream.finalized).is_some_and(|b| b.kind == BlockKind::Other && kind != BlockKind::Other) {
            stream.finalized += 1;
        }
        let block = stream.blocks.get(stream.finalized).filter(|b| b.kind == kind && b.kind != BlockKind::Other)?;
        stream.finalized += 1;
        Some((block.kind, block.node, block.input, block.streamed))
    }

    fn finalize_block(
        &mut self,
        (kind, node, input, streamed): (BlockKind, Option<NodeId>, Option<NodeId>, bool),
        block: &mut Value,
        uuid: Option<&str>,
    ) -> Result<()> {
        let Some(node) = node else { return Ok(()) };
        match kind {
            BlockKind::Text | BlockKind::Thinking => {
                let field = if kind == BlockKind::Text { "text" } else { "thinking" };
                if !streamed {
                    let text = take_string(block, field);
                    if !text.is_empty() {
                        self.sink.commit(Draft::set_text(node, text))?;
                    }
                }
                if let Some(uuid) = uuid {
                    self.set(node, attr::ID, uuid)?;
                }
                self.finish_message(node)
            }
            BlockKind::ToolUse => {
                let args = block.get("input").cloned().unwrap_or(Value::Null);
                if !streamed && let Some(input) = input {
                    self.sink.commit(Draft::set_text(input, args.to_string()))?;
                }
                let id = str_of(block, "id").unwrap_or_default().to_owned();
                let tool = str_of(block, "name").unwrap_or_default().to_owned();
                if let Some(uuid) = uuid {
                    self.set(node, UUID_ATTR, uuid)?;
                }
                self.complete_call_input(node, &id, &tool, &args)
            }
            BlockKind::Other => Ok(()),
        }
    }

    fn insert_block(&mut self, turn: NodeId, kind: BlockKind, mut block: Value, model: Option<String>, uuid: Option<&str>) -> Result<()> {
        match kind {
            BlockKind::Text | BlockKind::Thinking => {
                let (field, message_kind) =
                    if kind == BlockKind::Text { ("text", MessageKind::Text) } else { ("thinking", MessageKind::Thinking) };
                let text = take_string(&mut block, field);
                let node = self.open_message(turn, message_kind, model, text)?;
                if let Some(uuid) = uuid {
                    self.set(node, attr::ID, uuid)?;
                }
                self.finish_message(node)
            }
            BlockKind::ToolUse => {
                let id = str_of(&block, "id").unwrap_or_default().to_owned();
                let tool = str_of(&block, "name").unwrap_or_default().to_owned();
                let args = block.get("input").cloned().unwrap_or(Value::Null);
                let (call, _) = self.open_call(turn, &id, &tool, Some(args.to_string()))?;
                if let Some(uuid) = uuid {
                    self.set(call, UUID_ATTR, uuid)?;
                }
                self.complete_call_input(call, &id, &tool, &args)
            }
            BlockKind::Other => {
                let tag = format!("block:{}", str_of(&block, "type").unwrap_or_default());
                self.ext_under(turn, &tag, &block)
            }
        }
    }

    fn on_subagent_message(&mut self, parent: &str, msg: &Value) -> Result<()> {
        let Some(job) = self.jobs.get(parent).copied() else { return Ok(()) };
        let Some(content) = msg.pointer("/message/content").and_then(Value::as_array) else { return Ok(()) };
        for block in content {
            if str_of(block, "type") == Some("text")
                && let Some(text) = str_of(block, "text").filter(|t| !t.is_empty())
            {
                self.sink.commit(Draft::append(job, format!("{text}\n")))?;
            }
        }
        Ok(())
    }

    fn on_user(&mut self, msg: &mut Value) -> Result<()> {
        let subagent = msg.get("parent_tool_use_id").is_some_and(|p| !p.is_null());
        // Tool results of an interrupted turn arrive after it was cancelled.
        if subagent || self.draining > 0 {
            return Ok(());
        }
        if msg.get("isReplay").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        let summary = msg.get("isCompactSummary").and_then(Value::as_bool) == Some(true);
        let Some(content) = msg.get_mut("message").and_then(|m| m.get_mut("content")) else { return Ok(()) };
        if let Some(compaction) = self.open_compaction.filter(|_| summary) {
            self.open_compaction = None;
            let text = match content.take() {
                Value::String(text) => text,
                other => text_of_blocks(&other).unwrap_or_else(|| other.to_string()),
            };
            self.sink.commit(Draft::set_text(compaction, text))?;
            return Ok(());
        }
        let Value::Array(blocks) = content else { return self.ext("user", msg) };
        let blocks = std::mem::take(blocks);
        let uuid = str_of(msg, "uuid").map(str::to_owned);
        let mut rest = Vec::new();
        for mut block in blocks {
            if str_of(&block, "type") == Some("tool_result") {
                self.on_tool_result(&mut block, uuid.as_deref())?;
            } else {
                rest.push(block);
            }
        }
        if rest.is_empty() {
            return Ok(());
        }
        if let Some(Value::Array(blocks)) = msg.get_mut("message").and_then(|m| m.get_mut("content")) {
            *blocks = rest;
        }
        self.ext("user", msg)
    }

    fn on_tool_result(&mut self, block: &mut Value, uuid: Option<&str>) -> Result<()> {
        let id = str_of(block, "tool_use_id").unwrap_or_default().to_owned();
        let Some(call) = self.calls.get(&id) else {
            let turn = self.turn.as_ref().map_or(self.session, |t| t.node);
            return self.ext_under(turn, "tool_result", block);
        };
        let (node, job) = (call.node, call.job);
        let is_error = block.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        let (text, format) = match block.get_mut("content").map(Value::take) {
            Some(Value::String(text)) => (text, None),
            Some(Value::Null) | None => (String::new(), None),
            Some(other) => match text_of_blocks(&other) {
                Some(text) => (text, None),
                None => (other.to_string(), Some("json")),
            },
        };
        let mut attrs = vec![(attr::IS_ERROR, V::Bool(is_error))];
        if let Some(uuid) = uuid {
            attrs.push((UUID_ATTR, uuid.into()));
        }
        if let Some(format) = format {
            attrs.push((attr::FORMAT, format.into()));
        }
        self.insert(node, ElementKind::Result, attrs, text)?;
        let status = if is_error { CallStatus::Error } else { CallStatus::Done };
        self.set(node, attr::STATUS, status)?;
        self.set(node, attr::ENDED, V::Int(now_ms()))?;
        if let Some(turn) = &mut self.turn {
            turn.running_calls.retain(|c| *c != node);
        }
        if let Some(job) = job {
            self.finish_job(job, if is_error { JobStatus::Error } else { JobStatus::Done })?;
        }
        Ok(())
    }

    fn on_result(&mut self, msg: &Value) -> Result<()> {
        if self.draining > 0 {
            // The result of an interrupted turn, already closed as cancelled.
            self.draining -= 1;
            return Ok(());
        }
        let Some(turn) = self.turn.as_ref().map(|t| t.node) else { return Ok(()) };
        let mut attrs = Vec::new();
        let usage = msg.get("usage");
        let field = |name: &str| usage.and_then(|u| u.get(name)).and_then(Value::as_i64);
        for (native, ours) in [
            ("input_tokens", attr::INPUT_TOKENS),
            ("output_tokens", attr::OUTPUT_TOKENS),
            ("cache_read_input_tokens", attr::CACHE_READ_TOKENS),
            ("cache_creation_input_tokens", attr::CACHE_WRITE_TOKENS),
        ] {
            if let Some(value) = field(native) {
                attrs.push((ours, V::Int(value)));
            }
        }
        if let Some(cost) = msg.get("total_cost_usd").and_then(Value::as_f64) {
            attrs.push((attr::COST, V::Float(cost)));
        }
        if let Some(duration) = msg.get("duration_ms").and_then(Value::as_i64) {
            attrs.push((attr::DURATION_MS, V::Int(duration)));
        }
        if let Some(model) = &self.model {
            attrs.push((attr::MODEL, model.as_str().into()));
        }
        if !attrs.is_empty() {
            self.insert(turn, ElementKind::Usage, attrs, String::new())?;
        }
        let aborted = str_of(msg, "terminal_reason").is_some_and(|r| r.starts_with("aborted"));
        let failed = msg.get("is_error").and_then(Value::as_bool) == Some(true)
            || str_of(msg, "subtype").is_some_and(|s| s != "success");
        let status = match (aborted, failed) {
            (true, _) => TurnStatus::Cancelled,
            (false, true) => TurnStatus::Error,
            (false, false) => TurnStatus::Done,
        };
        self.close_turn(status)
    }

    /// Ends the open turn. Whatever is still streaming, running or waiting is
    /// cancelled, or marked failed when the turn failed.
    fn close_turn(&mut self, status: TurnStatus) -> Result<()> {
        let Some(turn) = self.turn.take() else { return Ok(()) };
        let now = V::Int(now_ms());
        let (message, call) = match status {
            TurnStatus::Error => (MessageStatus::Error, CallStatus::Error),
            TurnStatus::Done => (MessageStatus::Done, CallStatus::Cancelled),
            _ => (MessageStatus::Cancelled, CallStatus::Cancelled),
        };
        for node in turn.open_messages {
            self.set(node, attr::STATUS, message)?;
        }
        // A result that still arrives for these belongs to a dead turn.
        self.calls.retain(|_, call| !turn.running_calls.contains(&call.node));
        for node in turn.running_calls {
            self.set(node, attr::STATUS, call)?;
            self.set(node, attr::ENDED, now.clone())?;
        }
        for pending in std::mem::take(&mut self.pending) {
            for (decision, answered) in pending.decisions {
                if answered.is_none() {
                    self.set(decision, attr::STATUS, DecisionStatus::Cancelled)?;
                    self.set(decision, attr::ENDED, now.clone())?;
                }
            }
        }
        self.set(turn.node, attr::STATUS, status)?;
        self.set(turn.node, attr::ENDED, now)
    }

    /// Records an interrupt the CLI accepted: the open turn and everything in
    /// it become `cancelled`. Events of that turn still in flight, up to its
    /// `result`, are dropped.
    pub fn interrupt(&mut self) -> Result<()> {
        if self.turn.is_none() {
            return Ok(());
        }
        self.draining += 1;
        self.close_turn(TurnStatus::Cancelled)
    }

    /// Records that the CLI exited. An open turn fails.
    pub fn exited(&mut self) -> Result<()> {
        self.draining = 0;
        self.close_turn(TurnStatus::Error)
    }

    fn on_control_request(&mut self, mut msg: Value) -> Result<Option<Output>> {
        let request_id = str_of(&msg, "request_id").unwrap_or_default().to_owned();
        let Some(mut request) = msg.get_mut("request").map(Value::take) else {
            return Ok(Some(Output::Write(control_error(&request_id, "missing request"))));
        };
        match str_of(&request, "subtype").unwrap_or_default() {
            "can_use_tool" => self.on_can_use_tool(request_id, &mut request),
            "mcp_message" => {
                let server = str_of(&request, "server_name").unwrap_or_default().to_owned();
                let message = request.get_mut("message").map(Value::take).unwrap_or(Value::Null);
                Ok(Some(Output::Mcp { request_id, server, message }))
            }
            other => {
                let error = format!("unsupported control request `{other}`");
                Ok(Some(Output::Write(control_error(&request_id, &error))))
            }
        }
    }

    fn on_can_use_tool(&mut self, request_id: String, request: &mut Value) -> Result<Option<Output>> {
        let Some(turn) = self.current_turn()? else {
            // The turn it belongs to was interrupted.
            let deny = json!({"behavior": "deny", "message": "The turn was interrupted."});
            return Ok(Some(Output::Write(control_success(&request_id, deny))));
        };
        let tool = str_of(request, "tool_name").unwrap_or_default().to_owned();
        let input = request.get_mut("input").map(Value::take).unwrap_or(Value::Null);
        let call = str_of(request, "tool_use_id").and_then(|id| self.calls.get(id)).map(|c| c.node);
        let parent = call.unwrap_or(turn);
        let now = V::Int(now_ms());
        let base = |kind: DecisionKind| {
            let mut attrs = vec![
                (attr::KIND, V::from(kind)),
                (attr::STATUS, DecisionStatus::Waiting.into()),
                (attr::REQUEST_ID, request_id.as_str().into()),
                (attr::CREATED, now.clone()),
            ];
            if let Some(call) = call {
                attrs.push((attr::CALL, V::Int(node_int(call))));
            }
            attrs
        };
        let (kind, decisions) = match tool.as_str() {
            ASK_TOOL => {
                let questions = input.get("questions").and_then(Value::as_array).cloned().unwrap_or_default();
                let mut nodes = Vec::with_capacity(questions.len());
                for question in &questions {
                    let mut attrs = base(DecisionKind::Question);
                    if let Some(header) = str_of(question, "header") {
                        attrs.push((attr::TITLE, header.into()));
                    }
                    let options: Vec<V> = question
                        .get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|o| str_of(o, "label"))
                        .map(V::from)
                        .collect();
                    attrs.push((attr::OPTIONS, V::List(options)));
                    let multi = question.get("multiSelect").and_then(Value::as_bool).unwrap_or(false);
                    attrs.push((attr::MULTI, V::Bool(multi)));
                    let text = str_of(question, "question").unwrap_or_default().to_owned();
                    nodes.push((self.insert(parent, ElementKind::Decision, attrs, text)?, None));
                }
                (PendingKind::Question, nodes)
            }
            PLAN_TOOL => {
                let plan = str_of(&input, "plan").unwrap_or_default().to_owned();
                let mut attrs = base(DecisionKind::Plan);
                attrs.push((attr::TITLE, "Plan".into()));
                (PendingKind::Plan, vec![(self.insert(parent, ElementKind::Decision, attrs, plan)?, None)])
            }
            _ => {
                let mut attrs = base(DecisionKind::Approval);
                let title = str_of(request, "title").map_or_else(|| format!("Allow {tool}?"), str::to_owned);
                attrs.push((attr::TITLE, title.into()));
                for (native, ours) in [("decision_reason", "x-claude-decision-reason"), ("blocked_path", "x-claude-blocked-path")] {
                    if let Some(value) = str_of(request, native) {
                        attrs.push((ours, value.into()));
                    }
                }
                let text = match str_of(request, "description") {
                    Some(description) => description.to_owned(),
                    None => serde_json::to_string_pretty(&input).unwrap_or_default(),
                };
                (PendingKind::Approval, vec![(self.insert(parent, ElementKind::Decision, attrs, text)?, None)])
            }
        };
        let suggestions = request.get_mut("permission_suggestions").map(Value::take).filter(|s| !s.is_null());
        let pending = Pending { request_id, kind, tool, input, suggestions, decisions };
        if pending.decisions.is_empty() {
            // A question call with no questions has nothing to ask.
            return Ok(Some(Output::Write(control_success(&pending.request_id, permission_response(&pending)))));
        }
        self.pending.push(pending);
        self.set(turn, attr::STATUS, TurnStatus::Waiting).map(|()| None)
    }

    fn on_cancel(&mut self, msg: &Value) -> Result<()> {
        let Some(id) = str_of(msg, "request_id") else { return Ok(()) };
        let Some(at) = self.pending.iter().position(|p| p.request_id == id) else { return Ok(()) };
        let pending = self.pending.remove(at);
        let now = V::Int(now_ms());
        for (decision, answered) in pending.decisions {
            if answered.is_none() {
                self.set(decision, attr::STATUS, DecisionStatus::Cancelled)?;
                self.set(decision, attr::ENDED, now.clone())?;
            }
        }
        self.resume_turn()
    }

    fn resume_turn(&mut self) -> Result<()> {
        match (&self.turn, self.pending.is_empty()) {
            (Some(turn), true) => self.set(turn.node, attr::STATUS, TurnStatus::Running),
            _ => Ok(()),
        }
    }

    /// Records the answer to a waiting decision. Returns the control response
    /// to write once every decision of its request is answered.
    pub fn answer(&mut self, decision: NodeId, answer: Answer) -> Result<Option<Value>> {
        let (at, slot) = self
            .pending
            .iter()
            .enumerate()
            .find_map(|(at, p)| p.decisions.iter().position(|(d, a)| *d == decision && a.is_none()).map(|slot| (at, slot)))
            .ok_or(AdapterError::UnknownDecision(decision))?;
        let kind = self.pending[at].kind;
        validate(kind, &answer)?;
        self.set(decision, attr::ANSWER, V::from(answer.values.clone()))?;
        self.set(decision, attr::BY, answer.by.as_str())?;
        if let Some(reason) = &answer.reason {
            self.set(decision, attr::REASON, reason.as_str())?;
        }
        self.set(decision, attr::STATUS, DecisionStatus::Answered)?;
        self.set(decision, attr::ENDED, V::Int(now_ms()))?;
        self.pending[at].decisions[slot].1 = Some(answer);
        if self.pending[at].decisions.iter().any(|(_, a)| a.is_none()) {
            return Ok(None);
        }
        let pending = self.pending.remove(at);
        let response = permission_response(&pending);
        if pending.kind == PendingKind::Plan && matches!(response.get("behavior").and_then(Value::as_str), Some("allow")) {
            self.record_mode(SessionMode::Normal)?;
        }
        self.resume_turn()?;
        Ok(Some(control_success(&pending.request_id, response)))
    }

    fn ext(&mut self, kind: &str, msg: &Value) -> Result<()> {
        let parent = self.turn.as_ref().map_or(self.session, |t| t.node);
        self.ext_under(parent, kind, msg)
    }

    fn ext_under(&mut self, parent: NodeId, kind: &str, raw: &Value) -> Result<()> {
        let attrs = [(attr::HARNESS, V::from(HARNESS)), (attr::TYPE, kind.into())];
        self.insert(parent, ElementKind::Ext, attrs, raw.to_string()).map(|_| ())
    }

    fn insert<N: Into<String>>(
        &self,
        parent: NodeId,
        kind: ElementKind,
        attrs: impl IntoIterator<Item = (N, V)>,
        text: String,
    ) -> Result<NodeId> {
        Ok(self.sink.commit(Draft::insert(parent, kind, attrs, text))?.node)
    }

    fn set(&self, node: NodeId, name: &str, value: impl Into<V>) -> Result<()> {
        self.sink.commit(Draft::set(node, name, value))?;
        Ok(())
    }
}

fn validate(kind: PendingKind, answer: &Answer) -> Result<()> {
    let allowed: &[&str] = match kind {
        PendingKind::Approval => &[answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY],
        PendingKind::Plan => &[answer::APPROVE, answer::EDIT, answer::REJECT],
        PendingKind::Question => return Ok(()),
    };
    match answer.values.first() {
        Some(value) if allowed.contains(&value.as_str()) => Ok(()),
        _ => Err(AdapterError::Protocol(format!("the answer must be one of {allowed:?}, got {:?}", answer.values))),
    }
}

/// The `can_use_tool` response for a fully answered request, in the shape
/// Claude Code expects (`behavior`, `updatedInput`, `updatedPermissions`,
/// `message`).
fn permission_response(pending: &Pending) -> Value {
    let first = pending.decisions.first().and_then(|(_, a)| a.as_ref());
    let value = first.and_then(|a| a.values.first()).map_or("", String::as_str);
    let reason = first.and_then(|a| a.reason.as_deref());
    match pending.kind {
        PendingKind::Approval => match value {
            answer::ALLOW_SESSION => {
                let rules = pending.suggestions.clone().unwrap_or_else(|| {
                    json!([{"type": "addRules", "rules": [{"toolName": pending.tool}], "behavior": "allow", "destination": "session"}])
                });
                json!({"behavior": "allow", "updatedInput": pending.input, "updatedPermissions": rules})
            }
            answer::ALLOW_ONCE => json!({"behavior": "allow", "updatedInput": pending.input}),
            _ => json!({"behavior": "deny", "message": reason.unwrap_or("The user denied this tool call.")}),
        },
        PendingKind::Plan => match value {
            answer::APPROVE => json!({"behavior": "allow", "updatedInput": pending.input}),
            answer::EDIT => {
                let mut input = pending.input.clone();
                if let (Some(plan), Some(object)) = (reason, input.as_object_mut()) {
                    object.insert("plan".into(), Value::String(plan.to_owned()));
                }
                json!({"behavior": "allow", "updatedInput": input})
            }
            _ => json!({"behavior": "deny", "message": reason.unwrap_or("The user rejected the plan.")}),
        },
        PendingKind::Question => {
            let questions = pending.input.get("questions").and_then(Value::as_array);
            let mut answers = serde_json::Map::new();
            for (at, (_, answer)) in pending.decisions.iter().enumerate() {
                let question = questions.and_then(|q| q.get(at)).and_then(|q| str_of(q, "question"));
                if let (Some(question), Some(answer)) = (question, answer) {
                    answers.insert(question.to_owned(), Value::String(answer.values.join(", ")));
                }
            }
            let mut input = pending.input.clone();
            if let Some(object) = input.as_object_mut() {
                object.insert("answers".into(), Value::Object(answers));
            }
            json!({"behavior": "allow", "updatedInput": input})
        }
    }
}

fn on_control_response(msg: &mut Value) -> Output {
    let mut response = msg.get_mut("response").map(Value::take).unwrap_or(Value::Null);
    let request_id = str_of(&response, "request_id").unwrap_or_default().to_owned();
    let result = if str_of(&response, "subtype") == Some("error") {
        Err(str_of(&response, "error").unwrap_or("control request failed").to_owned())
    } else {
        Ok(response.get_mut("response").map(Value::take).unwrap_or(Value::Null))
    };
    Output::ControlResponse { request_id, result }
}

/// Claude Code permission mode to session mode.
fn mode_of(permission_mode: &str) -> SessionMode {
    if permission_mode == "plan" { SessionMode::Plan } else { SessionMode::Normal }
}

/// One line for the collapsed call row, from the argument that best names
/// what the call does.
fn call_title(input: &Value) -> Option<String> {
    const KEYS: [&str; 8] = ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"];
    const MAX: usize = 120;
    let text = KEYS.iter().find_map(|k| str_of(input, k))?;
    let line = text.lines().next().unwrap_or_default();
    Some(match line.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    })
}

/// The text of a block list made only of text blocks.
fn text_of_blocks(blocks: &Value) -> Option<String> {
    let blocks = blocks.as_array()?;
    let mut text = String::new();
    for block in blocks {
        if str_of(block, "type") != Some("text") {
            return None;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(str_of(block, "text").unwrap_or_default());
    }
    Some(text)
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

/// Moves a string field out of `value` without copying it.
fn take_string(value: &mut Value, key: &str) -> String {
    match value.get_mut(key).map(Value::take) {
        Some(Value::String(text)) => text,
        _ => String::new(),
    }
}

fn index_of(event: &Value) -> Option<usize> {
    usize::try_from(event.get("index")?.as_u64()?).ok()
}

/// Node ids as the `Int` attribute values `call` and `covers` hold.
fn node_int(node: NodeId) -> i64 {
    i64::try_from(node.0).unwrap_or(i64::MAX)
}
