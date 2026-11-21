//! Claude Code's stream JSON lines as DOM writes; turn bookkeeping is a
//! [`TurnLedger`]. Whatever goes back to the CLI comes out as an [`Output`].
//! Lines are read field by field: a field of unexpected type counts as absent.

use crate::claude::{HARNESS, USAGE};
use crate::kit::{EndEvent, TurnLedger, insert_root};
use loa_contracts::harness::{AdapterError, Answer, Result, SessionConfig, SessionState};
use loa_core::{
    CallStatus, DecisionKind, DecisionStatus, Dom, DomWriter, ElementKind as E, JobKind, JobStatus, MessageKind,
    MessageStatus, NodeId, PatchSink, Role, SessionMode, TurnStatus, Value as V, answer, attr, now_ms,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// What the driver must do after a line was mapped.
#[derive(Debug, PartialEq)]
pub enum Output {
    /// A line for the CLI's stdin.
    Write(Value),
    /// A JSON-RPC message for an SDK MCP server the host serves.
    Mcp { request_id: String, server: String, message: Value },
}

/// The uuid of the transcript line a `<call>` or `<result>` came from.
pub const UUID_ATTR: &str = "x-claude-uuid";

const SUBAGENT_TOOLS: [&str; 2] = ["Task", "Agent"];

/// Ledger keys: streaming messages by node, running calls by tool use id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    Message(NodeId),
    Call(String),
}

/// A streamed `text`, `thinking` or `tool_use` block: its `<message>` or
/// `<call>`, and the node its deltas append to (a call's `<input>`).
#[derive(Clone, Copy)]
struct StreamBlock {
    kind: &'static str,
    node: NodeId,
    target: NodeId,
    streamed: bool,
}

/// The API message whose stream events are arriving.
struct Streaming {
    message_id: String,
    model: Option<String>,
    /// `None` for blocks the stream made no element for (signatures, redactions).
    blocks: Vec<Option<StreamBlock>>,
    /// Blocks already matched to a complete `assistant` message.
    finalized: usize,
}

struct Call {
    node: NodeId,
    /// The subagent that ends with this call's result.
    job: Option<NodeId>,
}

/// A `can_use_tool` request waiting for the user; a question request has one decision per question.
struct Pending {
    request_id: String,
    kind: DecisionKind,
    tool: String,
    input: Value,
    suggestions: Option<Value>,
    decisions: Vec<(NodeId, Option<Answer>)>,
}

/// Maps one Claude Code session's stream onto its DOM.
pub struct Mapper {
    sink: Arc<dyn PatchSink>,
    ledger: TurnLedger<Key>,
    native_id: Option<String>,
    model: Option<String>,
    mode: Option<SessionMode>,
    stream: Option<Streaming>,
    calls: HashMap<String, Call>,
    /// Jobs by tool use id and by task id.
    jobs: HashMap<String, NodeId>,
    pending: Vec<Pending>,
    /// Turns since the last compaction.
    uncompacted: Vec<NodeId>,
    open_compaction: Option<NodeId>,
    /// Interrupted turns whose `result` hasn't arrived; their late events are dropped.
    draining: u32,
}

/// A content block's kind: `text`, `thinking`, `tool_use` (server and MCP tool uses too), or "" for any other.
fn block_kind(block: &Value) -> &'static str {
    match block["type"].as_str() {
        Some("text") => "text",
        Some("thinking") => "thinking",
        Some("tool_use" | "server_tool_use" | "mcp_tool_use") => "tool_use",
        _ => "",
    }
}

/// The text of a string, or of blocks that are all text, joined by lines.
fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let texts = blocks.iter().map(|b| (b["type"] == "text").then(|| b["text"].as_str().unwrap_or_default()));
            texts.collect::<Option<Vec<_>>>().map(|texts| texts.join("\n"))
        }
        _ => None,
    }
}

/// A `control_response` line answering `request_id`.
pub(crate) fn control_response(request_id: &str, outcome: std::result::Result<Value, String>) -> Value {
    let mut response = match outcome {
        Ok(response) => json!({"subtype": "success", "response": response}),
        Err(error) => json!({"subtype": "error", "error": error}),
    };
    response["request_id"] = request_id.into();
    let mut line = json!({"type": "control_response", "response": response});
    line.sort_all_objects();
    line
}

impl Mapper {
    /// Inserts the `<session>` root for a new session.
    pub fn create(sink: Arc<dyn PatchSink>, config: &SessionConfig) -> Result<Self> {
        let session = insert_root(&DomWriter::new(&*sink), HARNESS, config)?;
        Ok(Self::new(sink, TurnLedger::new(session), None, config.model.clone(), config.mode))
    }

    /// Continues a session from its journal; turns left open by a process that is gone are cancelled.
    pub fn attach(sink: Arc<dyn PatchSink>, dom: &Dom) -> Result<Self> {
        let root = dom.root().ok_or_else(|| AdapterError::Format("the session DOM has no root".into()))?;
        let text = |name| root.str_attr(name).map(str::to_owned);
        let mode = root.str_attr(attr::MODE).and_then(SessionMode::parse);
        let mut mapper = Self::new(sink, TurnLedger::new(root.id), text(attr::NATIVE_ID), text(attr::MODEL), mode);
        for child in dom.children(root.id) {
            match child.kind {
                E::Turn => mapper.uncompacted.push(child.id),
                E::Compaction => mapper.uncompacted.clear(),
                _ => {}
            }
        }
        // Journals can contain more than one stale turn, including before a closed last turn.
        for turn in dom.children_of_kind(root.id, E::Turn).filter(|t| matches!(t.str_attr(attr::STATUS), Some("running" | "waiting"))) {
            for node in dom.descendants(turn.id) {
                match (node.kind, node.str_attr(attr::STATUS)) {
                    (E::Message, Some("streaming")) => mapper.w().set(node.id, attr::STATUS, MessageStatus::Cancelled)?,
                    (E::Call, Some("running" | "pending")) => mapper.w().close(node.id, CallStatus::Cancelled)?,
                    (E::Decision, Some("waiting")) => mapper.w().close(node.id, DecisionStatus::Cancelled)?,
                    _ => {}
                }
            }
            mapper.w().close(turn.id, TurnStatus::Cancelled)?;
        }
        Ok(mapper)
    }

    fn new(sink: Arc<dyn PatchSink>, ledger: TurnLedger<Key>, native_id: Option<String>, model: Option<String>, mode: Option<SessionMode>) -> Self {
        Self {
            sink,
            ledger,
            native_id,
            model,
            mode,
            stream: None,
            calls: HashMap::new(),
            jobs: HashMap::new(),
            pending: Vec::new(),
            uncompacted: Vec::new(),
            open_compaction: None,
            draining: 0,
        }
    }

    fn w(&self) -> DomWriter<'_> {
        DomWriter::new(&*self.sink)
    }

    /// Claude Code's own session id, from `system/init`.
    pub fn native_id(&self) -> Option<&str> {
        self.native_id.as_deref()
    }

    pub fn state(&self) -> SessionState {
        self.ledger.state()
    }

    pub fn turn_open(&self) -> bool {
        self.ledger.turn().is_some()
    }

    fn turn(&self) -> Option<NodeId> {
        self.ledger.turn().map(|t| t.node)
    }

    /// Records a prompt sent to the CLI with id `uuid`: the `<turn>` and the user `<message>`.
    pub fn open_turn(&mut self, text: &str, by: &str, uuid: &str) -> Result<NodeId> {
        if self.turn_open() {
            return Err(AdapterError::Busy);
        }
        let turn = self.begin_turn(uuid)?;
        let attrs = [(attr::ROLE, V::from(Role::User)), (attr::STATUS, MessageStatus::Done.into()), (attr::ID, uuid.into()), (attr::BY, by.into())];
        self.w().insert(turn, E::Message, attrs, text)?;
        Ok(turn)
    }

    fn begin_turn(&mut self, id: &str) -> Result<NodeId> {
        let node = self.ledger.begin_with(&DomWriter::new(&*self.sink), None)?;
        self.w().set(node, attr::STARTED, now_ms())?;
        if !id.is_empty() {
            self.w().set(node, attr::ID, id)?;
        }
        self.uncompacted.push(node);
        Ok(node)
    }

    /// The turn new events belong to; Claude Code may start one on its own. Until an
    /// interrupted turn's `result` arrives, events are that turn's and have none.
    fn current_turn(&mut self) -> Result<Option<NodeId>> {
        if self.draining > 0 {
            return Ok(None);
        }
        match self.turn() {
            Some(turn) => Ok(Some(turn)),
            None => self.begin_turn("").map(Some),
        }
    }

    /// Ends the open turn, cancelling (or failing) what is still open in it.
    fn end_turn(&mut self, status: TurnStatus) -> Result<()> {
        if status == TurnStatus::Done {
            let messages: Vec<_> = self.ledger.turn().into_iter().flat_map(|turn| turn.open.iter())
                .filter_map(|(key, node)| matches!(key, Key::Message(_)).then_some(*node)).collect();
            for node in messages {
                self.ledger.take(&Key::Message(node));
                self.w().set(node, attr::STATUS, MessageStatus::Done)?;
            }
        }
        let mut calls = Vec::new();
        self.ledger.end_turn(&DomWriter::new(&*self.sink), status, |ev| {
            if let EndEvent::Child { key: Key::Call(id), node } = ev {
                calls.push((id.clone(), node));
            }
        })?;
        for (id, node) in calls {
            // A result that still arrives belongs to a dead turn.
            self.calls.remove(&id);
            self.w().set(node, attr::ENDED, now_ms())?;
        }
        self.pending.clear();
        self.stream = None;
        Ok(())
    }

    /// Sets `model` on the session after a successful switch.
    pub fn record_model(&mut self, model: &str) -> Result<()> {
        if self.model.as_deref() != Some(model) {
            self.model = Some(model.to_owned());
            self.w().set(self.ledger.session, attr::MODEL, model)?;
        }
        Ok(())
    }

    /// Sets `mode` on the session after a successful switch.
    pub fn record_mode(&mut self, mode: SessionMode) -> Result<()> {
        if self.mode != Some(mode) {
            self.mode = Some(mode);
            self.w().set(self.ledger.session, attr::MODE, mode)?;
        }
        Ok(())
    }

    /// Records an interrupt the CLI accepted; the turn's events up to its `result` are dropped.
    pub fn interrupt(&mut self) -> Result<()> {
        if !self.turn_open() {
            return Ok(());
        }
        self.draining += 1;
        self.end_turn(TurnStatus::Cancelled)
    }

    /// Records that the CLI exited: an open turn fails and the session closes.
    pub fn exited(&mut self) -> Result<()> {
        self.draining = 0;
        self.end_turn(TurnStatus::Error)?;
        self.ledger.close();
        Ok(())
    }

    /// Maps one stdout line; lines of unknown `type` are kept as `<ext>`.
    pub fn handle(&mut self, mut raw: Value) -> Result<Option<Output>> {
        raw.sort_all_objects();
        let kind = raw["type"].as_str().unwrap_or_default();
        match kind {
            "system" => self.on_system(&raw),
            // A subagent's stream is summarized from its complete messages.
            "stream_event" if raw["parent_tool_use_id"].is_null() => self.on_stream(&raw["event"]),
            "assistant" => self.on_assistant(&raw),
            "user" => self.on_user(&raw),
            "result" => self.on_result(&raw),
            "control_request" => return self.on_control(&raw),
            "control_cancel_request" => raw["request_id"].as_str().map_or(Ok(()), |id| self.on_cancel(id)),
            "stream_event" | "keep_alive" | "tool_progress" => Ok(()),
            _ => self.ext(kind, &raw),
        }
        .map(|()| None)
    }

    fn on_system(&mut self, raw: &Value) -> Result<()> {
        let mode = raw["permissionMode"].as_str().map(|m| if m == "plan" { SessionMode::Plan } else { SessionMode::Normal });
        let task = raw["task_id"].as_str();
        match raw["subtype"].as_str().unwrap_or_default() {
            "init" => {
                if let Some(id) = raw["session_id"].as_str().filter(|id| self.native_id.as_deref() != Some(id)) {
                    self.native_id = Some(id.to_owned());
                    self.w().set(self.ledger.session, attr::NATIVE_ID, id)?;
                }
                if let Some(model) = raw["model"].as_str() {
                    self.record_model(model)?;
                }
                mode.map_or(Ok(()), |m| self.record_mode(m))
            }
            "status" => mode.map_or(Ok(()), |m| self.record_mode(m)),
            "compact_boundary" => {
                let open = self.turn();
                let covers = self.uncompacted.iter().filter(|t| Some(**t) != open).map(|t| V::from(*t)).collect();
                let mut attrs = vec![(attr::COVERS, V::List(covers)), (attr::CREATED, V::Int(now_ms()))];
                attrs.extend(raw["compact_metadata"]["pre_tokens"].as_i64().map(|t| (attr::TOKENS_BEFORE, V::Int(t))));
                self.open_compaction = Some(self.w().insert(self.ledger.session, E::Compaction, attrs, "")?);
                self.uncompacted.retain(|t| Some(*t) == open);
                Ok(())
            }
            "task_started" => match task {
                Some(task) => self.on_task_started(task, raw),
                None => self.ext("system", raw),
            },
            "task_notification" => {
                let status = match raw["status"].as_str() {
                    Some("completed") => JobStatus::Done,
                    Some("failed") => JobStatus::Error,
                    _ => JobStatus::Cancelled,
                };
                self.end_task(task, status, raw["summary"].as_str())
            }
            "task_updated" => {
                let status = match raw["patch"]["status"].as_str() {
                    Some("completed") => JobStatus::Done,
                    Some("failed") => JobStatus::Error,
                    Some("killed" | "stopped") => JobStatus::Cancelled,
                    _ => return Ok(()),
                };
                self.end_task(task, status, None)
            }
            "task_progress" => Ok(()),
            _ => self.ext("system", raw),
        }
    }

    fn on_task_started(&mut self, task: &str, raw: &Value) -> Result<()> {
        let tool_use_id = raw["tool_use_id"].as_str();
        // A subagent call already made its job; the task id now names it too.
        if let Some(job) = tool_use_id.and_then(|id| self.jobs.get(id)).copied() {
            self.jobs.insert(task.to_owned(), job);
            return Ok(());
        }
        let kind = match raw["task_type"].as_str() {
            Some(t) if t.contains("bash") || t.contains("shell") => JobKind::Shell,
            _ => JobKind::Subagent,
        };
        let call = tool_use_id.and_then(|id| self.calls.get(id)).map(|c| c.node);
        let mut attrs = vec![(attr::KIND, V::from(kind)), (attr::STATUS, JobStatus::Running.into()), (attr::ID, task.into()), (attr::STARTED, V::Int(now_ms()))];
        attrs.extend(raw["description"].as_str().map(|d| (attr::TITLE, d.into())));
        attrs.extend(call.map(|call| (attr::CALL, V::from(call))));
        let parent = call.or(self.turn()).unwrap_or(self.ledger.session);
        let job = self.w().insert(parent, E::Job, attrs, "")?;
        self.jobs.insert(task.to_owned(), job);
        if let Some(id) = tool_use_id {
            self.jobs.insert(id.to_owned(), job);
        }
        Ok(())
    }

    fn end_task(&mut self, task: Option<&str>, status: JobStatus, summary: Option<&str>) -> Result<()> {
        let Some(job) = task.and_then(|id| self.jobs.get(id)).copied() else { return Ok(()) };
        if let Some(summary) = summary.filter(|s| !s.is_empty()) {
            self.w().append(job, summary)?;
        }
        Ok(self.w().close(job, status)?)
    }

    /// A Messages API stream event.
    fn on_stream(&mut self, event: &Value) -> Result<()> {
        let index = event["index"].as_u64().and_then(|i| usize::try_from(i).ok());
        match (event["type"].as_str().unwrap_or_default(), index) {
            ("message_start", _) => {
                if self.current_turn()?.is_some() {
                    let message_id = event["message"]["id"].as_str().unwrap_or_default().to_owned();
                    let model = event["message"]["model"].as_str().map(str::to_owned);
                    self.stream = Some(Streaming { message_id, model, blocks: Vec::new(), finalized: 0 });
                }
            }
            ("content_block_start", Some(index)) if event.get("content_block").is_some() => {
                let (Some(turn), Some(stream)) = (self.turn(), &self.stream) else { return Ok(()) };
                let (block, model) = (&event["content_block"], stream.model.clone());
                let kind = block_kind(block);
                let block = match kind {
                    "" => None,
                    "tool_use" => Some(self.open_call(turn, block["id"].as_str().unwrap_or_default(), block["name"].as_str().unwrap_or_default(), "")?),
                    _ => Some(self.open_message(turn, kind, model, block[kind].as_str().unwrap_or_default())?),
                };
                if let Some(stream) = &mut self.stream {
                    stream.blocks.resize(stream.blocks.len().max(index), None);
                    stream.blocks.push(block);
                }
            }
            ("content_block_delta", Some(index)) => {
                let Some(Some(block)) = self.stream.as_mut().and_then(|s| s.blocks.get_mut(index)) else { return Ok(()) };
                let delta = &event["delta"];
                let field = match (delta["type"].as_str(), block.kind) {
                    (Some("text_delta"), "text") => "text",
                    (Some("thinking_delta"), "thinking") => "thinking",
                    (Some("input_json_delta"), "tool_use") => "partial_json",
                    _ => return Ok(()),
                };
                let text = delta[field].as_str().unwrap_or_default();
                if !text.is_empty() {
                    block.streamed = true;
                    let target = block.target;
                    self.w().append(target, text)?;
                }
            }
            ("content_block_stop", Some(index)) => {
                let block = self.stream.as_ref().and_then(|s| s.blocks.get(index)).copied().flatten();
                if let Some(block) = block.filter(|b| b.kind != "tool_use") {
                    self.finish_message(block.node)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Inserts a streaming assistant `<message>` for a `text` or `thinking` block.
    fn open_message(&mut self, turn: NodeId, kind: &'static str, model: Option<String>, text: &str) -> Result<StreamBlock> {
        let mut attrs = vec![(attr::ROLE, V::from(Role::Assistant)), (attr::STATUS, MessageStatus::Streaming.into())];
        attrs.extend((kind == "thinking").then(|| (attr::KIND, MessageKind::Thinking.into())));
        attrs.extend(model.map(|m| (attr::MODEL, m.into())));
        let node = self.w().insert(turn, E::Message, attrs, text)?;
        self.ledger.open(Key::Message(node), node);
        Ok(StreamBlock { kind, node, target: node, streamed: false })
    }

    fn finish_message(&mut self, node: NodeId) -> Result<()> {
        if self.ledger.take(&Key::Message(node)).is_some() {
            self.w().set(node, attr::STATUS, MessageStatus::Done)?;
        }
        Ok(())
    }

    /// Inserts a running `<call>` with its `<input>`.
    fn open_call(&mut self, turn: NodeId, id: &str, tool: &str, input: &str) -> Result<StreamBlock> {
        let attrs = [(attr::TOOL, V::from(tool)), (attr::CALL_ID, id.into()), (attr::STATUS, CallStatus::Running.into()), (attr::STARTED, V::Int(now_ms()))];
        let call = self.w().insert(turn, E::Call, attrs, "")?;
        let input = self.w().insert(call, E::Input, [(attr::FORMAT, "json")], input)?;
        self.ledger.open(Key::Call(id.to_owned()), call);
        self.calls.insert(id.to_owned(), Call { node: call, job: None });
        Ok(StreamBlock { kind: "tool_use", node: call, target: input, streamed: false })
    }

    /// A call's arguments are complete: title it and start the job a subagent call runs.
    fn complete_call(&mut self, call: NodeId, block: &Value, uuid: Option<&str>) -> Result<()> {
        let (id, tool, input) = (block["id"].as_str().unwrap_or_default(), block["name"].as_str().unwrap_or_default(), &block["input"]);
        if let Some(uuid) = uuid {
            self.w().set(call, UUID_ATTR, uuid)?;
        }
        if let Some(title) = call_title(input) {
            self.w().set(call, attr::TITLE, title)?;
        }
        if !SUBAGENT_TOOLS.contains(&tool) || self.jobs.contains_key(id) {
            return Ok(());
        }
        let mut attrs = vec![(attr::KIND, V::from(JobKind::Subagent)), (attr::STATUS, JobStatus::Running.into()), (attr::CALL, call.into()), (attr::STARTED, V::Int(now_ms()))];
        attrs.extend(input["subagent_type"].as_str().map(|a| (attr::AGENT, a.into())));
        attrs.extend(input["description"].as_str().map(|d| (attr::TITLE, d.into())));
        let job = self.w().insert(call, E::Job, attrs, "")?;
        self.jobs.insert(id.to_owned(), job);
        if input["run_in_background"] != true
            && let Some(entry) = self.calls.get_mut(id)
        {
            entry.job = Some(job);
        }
        Ok(())
    }

    /// A complete API message: settles what its stream made, or writes what it didn't stream.
    fn on_assistant(&mut self, raw: &Value) -> Result<()> {
        let body = &raw["message"];
        let blocks = body["content"].as_array().map_or(&[][..], Vec::as_slice);
        if let Some(parent) = raw["parent_tool_use_id"].as_str() {
            // Subagent messages are summarized on their job.
            let Some(job) = self.jobs.get(parent).copied() else { return Ok(()) };
            for text in blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()) {
                if !text.is_empty() {
                    self.w().append(job, format!("{text}\n"))?;
                }
            }
            return Ok(());
        }
        let Some(turn) = self.current_turn()? else { return Ok(()) };
        if !body.is_object() {
            return Ok(());
        }
        let (message_id, uuid) = (body["id"].as_str().unwrap_or_default(), raw["uuid"].as_str());
        for block in blocks {
            let kind = block_kind(block);
            if kind.is_empty() {
                let kind = format!("block:{}", block["type"].as_str().unwrap_or_default());
                self.w().ext(turn, HARNESS, &kind, block)?;
                continue;
            }
            let text = match kind {
                "tool_use" => block["input"].to_string(),
                _ => block[kind].as_str().unwrap_or_default().to_owned(),
            };
            let node = match self.next_streamed(message_id, kind) {
                Some(streamed) => {
                    if !streamed.streamed && !text.is_empty() {
                        self.w().set_text(streamed.target, text)?;
                    }
                    streamed.node
                }
                None if kind == "tool_use" => {
                    let (id, name) = (block["id"].as_str().unwrap_or_default(), block["name"].as_str().unwrap_or_default());
                    self.open_call(turn, id, name, &text)?.node
                }
                None => self.open_message(turn, kind, body["model"].as_str().map(str::to_owned), &text)?.node,
            };
            if kind == "tool_use" {
                self.complete_call(node, block, uuid)?;
            } else {
                if let Some(uuid) = uuid {
                    self.w().set(node, attr::ID, uuid)?;
                }
                self.finish_message(node)?;
            }
        }
        Ok(())
    }

    /// Takes the next streamed block of `message_id` when it has `kind`.
    fn next_streamed(&mut self, message_id: &str, kind: &str) -> Option<StreamBlock> {
        let stream = self.stream.as_mut().filter(|s| s.message_id == message_id)?;
        while stream.blocks.get(stream.finalized).is_some_and(Option::is_none) {
            stream.finalized += 1;
        }
        let block = stream.blocks.get(stream.finalized).copied().flatten().filter(|b| b.kind == kind)?;
        stream.finalized += 1;
        Some(block)
    }

    fn on_user(&mut self, raw: &Value) -> Result<()> {
        // Tool results of an interrupted turn arrive after it was cancelled.
        if !raw["parent_tool_use_id"].is_null() || self.draining > 0 || raw["isReplay"] == true {
            return Ok(());
        }
        let content = &raw["message"]["content"];
        if content.is_null() {
            return Ok(());
        }
        if let Some(compaction) = self.open_compaction.filter(|_| raw["isCompactSummary"] == true) {
            self.open_compaction = None;
            let text = content_text(content).unwrap_or_else(|| content.to_string());
            return Ok(self.w().set_text(compaction, text)?);
        }
        let Some(blocks) = content.as_array() else { return self.ext("user", raw) };
        let mut rest = Vec::new();
        for block in blocks {
            if block["type"] == "tool_result" {
                self.on_tool_result(block, raw["uuid"].as_str())?;
            } else {
                rest.push(block.clone());
            }
        }
        if rest.is_empty() {
            return Ok(());
        }
        let mut raw = raw.clone();
        raw["message"]["content"] = Value::Array(rest);
        self.ext("user", &raw)
    }

    fn on_tool_result(&mut self, block: &Value, uuid: Option<&str>) -> Result<()> {
        let id = block["tool_use_id"].as_str().unwrap_or_default();
        let Some(&Call { node, job }) = self.calls.get(id) else { return self.ext("tool_result", block) };
        let content = &block["content"];
        let (text, json) = match content_text(content) {
            _ if content.is_null() => (String::new(), false),
            Some(text) => (text, false),
            None => (content.to_string(), true),
        };
        let is_error = block["is_error"] == true;
        let mut attrs = vec![(attr::IS_ERROR, V::Bool(is_error))];
        attrs.extend(uuid.map(|uuid| (UUID_ATTR, uuid.into())));
        attrs.extend(json.then(|| (attr::FORMAT, "json".into())));
        self.w().insert(node, E::Result, attrs, text)?;
        self.w().close(node, if is_error { CallStatus::Error } else { CallStatus::Done })?;
        self.ledger.take(&Key::Call(id.to_owned()));
        if let Some(job) = job {
            self.w().close(job, if is_error { JobStatus::Error } else { JobStatus::Done })?;
        }
        Ok(())
    }

    fn on_result(&mut self, raw: &Value) -> Result<()> {
        if self.draining > 0 {
            // The result of an interrupted turn, already closed as cancelled.
            self.draining -= 1;
            return Ok(());
        }
        let Some(turn) = self.turn() else { return Ok(()) };
        let tokens = USAGE.iter().map(|&(name, key)| (name, raw["usage"][key].as_i64()));
        let tokens = tokens.chain([(attr::DURATION_MS, raw["duration_ms"].as_i64())]);
        let mut attrs: Vec<(&str, V)> = tokens.filter_map(|(name, n)| Some((name, V::Int(n?)))).collect();
        attrs.extend(raw["total_cost_usd"].as_f64().map(|cost| (attr::COST, V::Float(cost))));
        attrs.extend(self.model.as_deref().map(|model| (attr::MODEL, model.into())));
        if !attrs.is_empty() {
            self.w().insert(turn, E::Usage, attrs, "")?;
        }
        let status = if raw["terminal_reason"].as_str().is_some_and(|r| r.starts_with("aborted")) {
            TurnStatus::Cancelled
        } else if raw["is_error"] == true || raw["subtype"].as_str().is_some_and(|s| s != "success") {
            TurnStatus::Error
        } else {
            TurnStatus::Done
        };
        self.end_turn(status)
    }

    fn on_control(&mut self, raw: &Value) -> Result<Option<Output>> {
        let (request_id, request) = (raw["request_id"].as_str().unwrap_or_default(), &raw["request"]);
        let error = |e: String| Ok(Some(Output::Write(control_response(request_id, Err(e)))));
        match request["subtype"].as_str().unwrap_or_default() {
            _ if request.is_null() => error("missing request".to_owned()),
            "can_use_tool" => self.on_can_use_tool(request_id, request),
            "mcp_message" => {
                let server = request["server_name"].as_str().unwrap_or_default().to_owned();
                Ok(Some(Output::Mcp { request_id: request_id.to_owned(), server, message: request["message"].clone() }))
            }
            other => error(format!("unsupported control request `{other}`")),
        }
    }

    fn on_can_use_tool(&mut self, request_id: &str, request: &Value) -> Result<Option<Output>> {
        let Some(turn) = self.current_turn()? else {
            // The turn it belongs to was interrupted.
            let deny = json!({"behavior": "deny", "message": "The turn was interrupted."});
            return Ok(Some(Output::Write(control_response(request_id, Ok(deny)))));
        };
        let (tool, input) = (request["tool_name"].as_str().unwrap_or_default(), &request["input"]);
        let call = request["tool_use_id"].as_str().and_then(|id| self.calls.get(id)).map(|c| c.node);
        let kind = match tool {
            "AskUserQuestion" => DecisionKind::Question,
            "ExitPlanMode" => DecisionKind::Plan,
            _ => DecisionKind::Approval,
        };
        let now = V::Int(now_ms());
        let base = |title: Option<String>| {
            let mut attrs = vec![(attr::KIND, V::from(kind)), (attr::STATUS, DecisionStatus::Waiting.into()), (attr::REQUEST_ID, request_id.into()), (attr::CREATED, now.clone())];
            attrs.extend(call.map(|call| (attr::CALL, V::from(call))));
            attrs.extend(title.map(|t| (attr::TITLE, t.into())));
            attrs
        };
        let asks: Vec<_> = match kind {
            DecisionKind::Question => input["questions"].as_array().into_iter().flatten().map(|q| {
                let mut attrs = base(q["header"].as_str().map(str::to_owned));
                let options = q["options"].as_array().into_iter().flatten();
                attrs.push((attr::OPTIONS, V::List(options.filter_map(|o| o["label"].as_str()).map(V::from).collect())));
                attrs.push((attr::MULTI, V::Bool(q["multiSelect"] == true)));
                (attrs, q["question"].as_str().unwrap_or_default().to_owned())
            }).collect(),
            DecisionKind::Plan => vec![(base(Some("Plan".to_owned())), input["plan"].as_str().unwrap_or_default().to_owned())],
            _ => {
                let title = request["title"].as_str().map_or_else(|| format!("Allow {tool}?"), str::to_owned);
                let mut attrs = base(Some(title));
                attrs.extend(request["decision_reason"].as_str().map(|r| ("x-claude-decision-reason", r.into())));
                attrs.extend(request["blocked_path"].as_str().map(|p| ("x-claude-blocked-path", p.into())));
                let text = request["description"].as_str().map_or_else(|| serde_json::to_string_pretty(input).unwrap_or_default(), str::to_owned);
                vec![(attrs, text)]
            }
        };
        let parent = call.unwrap_or(turn);
        let mut decisions = Vec::with_capacity(asks.len());
        for (attrs, text) in asks {
            let decision = self.w().insert(parent, E::Decision, attrs, text)?;
            self.ledger.wait(decision);
            decisions.push((decision, None));
        }
        let suggestions = request.get("permission_suggestions").filter(|s| !s.is_null()).cloned();
        let pending = Pending { request_id: request_id.to_owned(), kind, tool: tool.to_owned(), input: input.clone(), suggestions, decisions };
        if pending.decisions.is_empty() {
            // A question call with no questions has nothing to ask.
            return Ok(Some(Output::Write(control_response(request_id, Ok(permission_response(&pending))))));
        }
        self.pending.push(pending);
        self.w().set(turn, attr::STATUS, TurnStatus::Waiting)?;
        Ok(None)
    }

    fn on_cancel(&mut self, request_id: &str) -> Result<()> {
        let Some(at) = self.pending.iter().position(|p| p.request_id == request_id) else { return Ok(()) };
        for (decision, answered) in self.pending.remove(at).decisions {
            if answered.is_none() {
                self.w().close(decision, DecisionStatus::Cancelled)?;
                self.ledger.answered(decision);
            }
        }
        self.resume_turn()
    }

    fn resume_turn(&mut self) -> Result<()> {
        match self.turn() {
            Some(turn) if self.pending.is_empty() => Ok(self.w().set(turn, attr::STATUS, TurnStatus::Running)?),
            _ => Ok(()),
        }
    }

    /// Records the answer to a waiting decision; returns the control response
    /// once every decision of its request is answered.
    pub fn answer(&mut self, decision: NodeId, answer: Answer) -> Result<Option<Value>> {
        let (at, slot) = self
            .pending
            .iter()
            .enumerate()
            .find_map(|(at, p)| p.decisions.iter().position(|(d, a)| *d == decision && a.is_none()).map(|slot| (at, slot)))
            .ok_or(AdapterError::UnknownDecision(decision))?;
        let allowed: &[&str] = match self.pending[at].kind {
            DecisionKind::Approval => &[answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY],
            DecisionKind::Plan => &[answer::APPROVE, answer::EDIT, answer::REJECT],
            _ => &[],
        };
        if !allowed.is_empty() && !answer.values.first().is_some_and(|v| allowed.contains(&v.as_str())) {
            return Err(AdapterError::Protocol(format!("the answer must be one of {allowed:?}, got {:?}", answer.values)));
        }
        let w = self.w();
        w.set(decision, attr::ANSWER, V::from(answer.values.clone()))?;
        w.set(decision, attr::BY, answer.by.as_str())?;
        if let Some(reason) = &answer.reason {
            w.set(decision, attr::REASON, reason.as_str())?;
        }
        w.close(decision, DecisionStatus::Answered)?;
        self.ledger.answered(decision);
        self.pending[at].decisions[slot].1 = Some(answer);
        if self.pending[at].decisions.iter().any(|(_, a)| a.is_none()) {
            return Ok(None);
        }
        let pending = self.pending.remove(at);
        let response = permission_response(&pending);
        if pending.kind == DecisionKind::Plan && response["behavior"] == "allow" {
            self.record_mode(SessionMode::Normal)?;
        }
        self.resume_turn()?;
        Ok(Some(control_response(&pending.request_id, Ok(response))))
    }

    fn ext(&mut self, kind: &str, raw: &Value) -> Result<()> {
        let parent = self.turn().unwrap_or(self.ledger.session);
        self.w().ext(parent, HARNESS, kind, raw)?;
        Ok(())
    }
}

/// The `can_use_tool` response for a fully answered request.
fn permission_response(pending: &Pending) -> Value {
    let first = pending.decisions.first().and_then(|(_, a)| a.as_ref());
    let value = first.and_then(|a| a.values.first()).map_or("", String::as_str);
    let reason = first.and_then(|a| a.reason.as_deref());
    let allow = |input: &Value| json!({"behavior": "allow", "updatedInput": input});
    let mut input = pending.input.clone();
    match (pending.kind, value) {
        (DecisionKind::Approval, answer::ALLOW_SESSION) => {
            let rules = pending.suggestions.clone().unwrap_or_else(|| {
                json!([{"type": "addRules", "rules": [{"toolName": pending.tool}], "behavior": "allow", "destination": "session"}])
            });
            json!({"behavior": "allow", "updatedInput": input, "updatedPermissions": rules})
        }
        (DecisionKind::Approval, answer::ALLOW_ONCE) | (DecisionKind::Plan, answer::APPROVE) => allow(&input),
        (DecisionKind::Approval, _) => json!({"behavior": "deny", "message": reason.unwrap_or("The user denied this tool call.")}),
        (DecisionKind::Plan, answer::EDIT) => {
            if let (Some(plan), Some(object)) = (reason, input.as_object_mut()) {
                object.insert("plan".into(), plan.into());
            }
            allow(&input)
        }
        (DecisionKind::Plan, _) => json!({"behavior": "deny", "message": reason.unwrap_or("The user rejected the plan.")}),
        _ => {
            let questions = pending.input["questions"].as_array();
            let mut answers = serde_json::Map::new();
            for (at, (_, answer)) in pending.decisions.iter().enumerate() {
                let question = questions.and_then(|q| q.get(at)?["question"].as_str());
                if let (Some(question), Some(answer)) = (question, answer) {
                    answers.insert(question.to_owned(), answer.values.join(", ").into());
                }
            }
            if let Some(object) = input.as_object_mut() {
                object.insert("answers".into(), Value::Object(answers));
            }
            allow(&input)
        }
    }
}

/// One line for the collapsed call row, from the argument that best names what the call does.
fn call_title(input: &Value) -> Option<String> {
    const KEYS: [&str; 8] = ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"];
    let text = KEYS.iter().find_map(|k| input[k].as_str())?;
    let line = text.lines().next().unwrap_or_default();
    Some(match line.char_indices().nth(120) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    })
}
