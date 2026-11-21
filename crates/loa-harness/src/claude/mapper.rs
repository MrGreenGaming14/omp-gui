//! Claude Code's decoded stream lines as DOM writes; turn bookkeeping is a
//! [`TurnLedger`]. Whatever goes back to the CLI comes out as an [`Output`].

use crate::claude::HARNESS;
use crate::claude::protocol::{
    Block, Content, StreamDelta as Delta, Line as Event, Message, Permission, Request, Stream, System, TurnResult, control_error,
    control_success,
};
use crate::kit::{EndEvent, TurnLedger};
use loa_contracts::harness::{AdapterError, Answer, Result, SessionConfig, SessionState};
use loa_core::{
    CallStatus, DecisionKind, DecisionStatus, Dom, DomWriter, Draft, ElementKind as E, JobKind, JobStatus, MessageKind,
    MessageStatus, NodeId, PatchSink, Role, SCHEMA_VERSION, SessionMode, TurnStatus, Value as V, answer, attr, now_ms,
};
use serde::Deserialize;
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

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Kind {
    Text,
    Thinking,
    ToolUse,
    Other,
}

/// A streamed content block: its `<message>` or `<call>` and the call's `<input>`.
#[derive(Clone, Copy)]
struct StreamBlock {
    kind: Kind,
    node: Option<NodeId>,
    input: Option<NodeId>,
    streamed: bool,
}

const NO_BLOCK: StreamBlock = StreamBlock { kind: Kind::Other, node: None, input: None, streamed: false };

/// The API message whose stream events are arriving.
struct Streaming {
    message_id: String,
    model: Option<String>,
    blocks: Vec<StreamBlock>,
    /// Blocks already matched to a complete `assistant` message.
    finalized: usize,
}

struct Call {
    node: NodeId,
    /// The subagent that ends with this call's result.
    job: Option<NodeId>,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum PendingKind {
    Approval,
    Question,
    Plan,
}

/// A `can_use_tool` request waiting for the user; a question request has one decision per question.
struct Pending {
    request_id: String,
    kind: PendingKind,
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

fn kind_of(block: &Block) -> Kind {
    match block {
        Block::Text { .. } => Kind::Text,
        Block::Thinking { .. } => Kind::Thinking,
        Block::ToolUse { .. } => Kind::ToolUse,
        _ => Kind::Other,
    }
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
        attrs.extend(config.mode.map(|mode| (attr::MODE, mode.into())));
        let session = sink.commit(Draft::insert(NodeId::DOCUMENT, E::Session, attrs, ""))?.node;
        Ok(Self::new(sink, TurnLedger::new(session), None, config.model.clone(), config.mode))
    }

    /// Continues a session from its journal; a turn left open by a process that is gone is cancelled.
    pub fn attach(sink: Arc<dyn PatchSink>, dom: &Dom) -> Result<Self> {
        let root = dom.root().ok_or_else(|| AdapterError::Format("the session DOM has no root".into()))?;
        let text = |name| root.str_attr(name).map(str::to_owned);
        let mode = root.str_attr(attr::MODE).and_then(SessionMode::parse);
        let mut mapper = Self::new(sink, TurnLedger::from_dom(dom), text(attr::NATIVE_ID), text(attr::MODEL), mode);
        for child in dom.children(root.id) {
            match child.kind {
                E::Turn => mapper.uncompacted.push(child.id),
                E::Compaction => mapper.uncompacted.clear(),
                _ => {}
            }
        }
        // Journals can contain more than one stale turn, including before a closed last turn.
        let tracked = mapper.turn();
        for turn in dom.children_of_kind(root.id, E::Turn).filter(|t| {
            Some(t.id) != tracked && matches!(t.str_attr(attr::STATUS), Some("running" | "waiting"))
        }) {
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
        if let Some(turn) = mapper.ledger.turn().map(|t| t.node) {
            for node in dom.descendants(turn) {
                let status = node.str_attr(attr::STATUS).unwrap_or_default();
                match node.kind {
                    E::Message if status == MessageStatus::Streaming.as_str() => {
                        mapper.ledger.open(Key::Message(node.id), node.id);
                    }
                    E::Call if [CallStatus::Running.as_str(), CallStatus::Pending.as_str()].contains(&status) => {
                        let id = node.str_attr(attr::CALL_ID).map_or_else(|| node.id.0.to_string(), str::to_owned);
                        mapper.ledger.open(Key::Call(id), node.id);
                    }
                    _ => {}
                }
            }
            mapper.end_turn(TurnStatus::Cancelled)?;
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
        let mut children = Vec::new();
        self.ledger.end_turn(&DomWriter::new(&*self.sink), status, |ev| {
            if let EndEvent::Child { key, node } = ev {
                children.push((key.clone(), node));
            }
        })?;
        for (key, node) in children {
            match key {
                Key::Message(_) => {}
                Key::Call(id) => {
                    // A result that still arrives belongs to a dead turn.
                    self.calls.remove(&id);
                    self.w().set(node, attr::ENDED, now_ms())?;
                }
            }
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

    /// Maps one stdout line; lines of unexpected shape are kept as `<ext>`.
    pub fn handle(&mut self, mut raw: Value) -> Result<Option<Output>> {
        raw.sort_all_objects();
        let kind = raw.get("type").and_then(Value::as_str).unwrap_or_default();
        let normalized;
        let event = match Event::deserialize(&raw) {
            Ok(event) => event,
            Err(_) => {
                normalized = crate::claude::normalize::normalize(&raw);
                match Event::deserialize(&normalized) {
                    Ok(event) => event,
                    Err(_) => return self.ext(kind, &raw).map(|()| None),
                }
            }
        };
        match event {
            Event::System(system) => self.on_system(&system, &raw),
            Event::StreamEvent { parent_tool_use_id: Some(_), .. } => Ok(()),
            Event::StreamEvent { event, .. } => self.on_stream(event),
            Event::Assistant(msg) => self.on_assistant(&msg, &raw),
            Event::User(msg) => self.on_user(&msg, &raw),
            Event::Result(result) => self.on_result(&result),
            Event::ControlRequest { request_id, request } => return self.on_control(request_id, request, &raw),
            Event::ControlCancelRequest { request_id } => request_id.map_or(Ok(()), |id| self.on_cancel(id)),
            Event::KeepAlive | Event::ToolProgress => Ok(()),
            Event::Other => self.ext(kind, &raw),
        }
        .map(|()| None)
    }

    fn on_system(&mut self, msg: &System, raw: &Value) -> Result<()> {
        let mode = |m: &str| if m == "plan" { SessionMode::Plan } else { SessionMode::Normal };
        match msg.subtype {
            "init" => {
                if let Some(id) = msg.session_id.filter(|id| self.native_id.as_deref() != Some(id)) {
                    self.native_id = Some(id.to_owned());
                    self.w().set(self.ledger.session, attr::NATIVE_ID, id)?;
                }
                if let Some(model) = msg.model {
                    self.record_model(model)?;
                }
                msg.permission_mode.map_or(Ok(()), |m| self.record_mode(mode(m)))
            }
            "status" => msg.permission_mode.map_or(Ok(()), |m| self.record_mode(mode(m))),
            "compact_boundary" => {
                let open = self.turn();
                let covers = self.uncompacted.iter().filter(|t| Some(**t) != open).map(|t| V::from(*t)).collect();
                let mut attrs = vec![(attr::COVERS, V::List(covers)), (attr::CREATED, V::Int(now_ms()))];
                attrs.extend(msg.compact_metadata.as_ref().and_then(|m| m.pre_tokens).map(|t| (attr::TOKENS_BEFORE, V::Int(t))));
                self.open_compaction = Some(self.w().insert(self.ledger.session, E::Compaction, attrs, "")?);
                self.uncompacted.retain(|t| Some(*t) == open);
                Ok(())
            }
            "task_started" => match msg.task_id {
                Some(task) => self.on_task_started(task, msg),
                None => self.ext("system", raw),
            },
            "task_notification" => {
                let status = match msg.status {
                    Some("completed") => JobStatus::Done,
                    Some("failed") => JobStatus::Error,
                    _ => JobStatus::Cancelled,
                };
                self.end_task(msg.task_id, status, msg.summary)
            }
            "task_updated" => {
                let status = match msg.patch.as_ref().and_then(|p| p.status) {
                    Some("completed") => JobStatus::Done,
                    Some("failed") => JobStatus::Error,
                    Some("killed" | "stopped") => JobStatus::Cancelled,
                    _ => return Ok(()),
                };
                self.end_task(msg.task_id, status, None)
            }
            "task_progress" => Ok(()),
            _ => self.ext("system", raw),
        }
    }

    fn on_task_started(&mut self, task: &str, msg: &System) -> Result<()> {
        // A subagent call already made its job; the task id now names it too.
        if let Some(job) = msg.tool_use_id.and_then(|id| self.jobs.get(id)).copied() {
            self.jobs.insert(task.to_owned(), job);
            return Ok(());
        }
        let kind = match msg.task_type {
            Some(t) if t.contains("bash") || t.contains("shell") => JobKind::Shell,
            _ => JobKind::Subagent,
        };
        let call = msg.tool_use_id.and_then(|id| self.calls.get(id)).map(|c| c.node);
        let mut attrs = vec![(attr::KIND, V::from(kind)), (attr::STATUS, JobStatus::Running.into()), (attr::ID, task.into()), (attr::STARTED, V::Int(now_ms()))];
        attrs.extend(msg.description.map(|d| (attr::TITLE, d.into())));
        attrs.extend(call.map(|call| (attr::CALL, V::from(call))));
        let parent = call.or(self.turn()).unwrap_or(self.ledger.session);
        let job = self.w().insert(parent, E::Job, attrs, "")?;
        self.jobs.insert(task.to_owned(), job);
        if let Some(id) = msg.tool_use_id {
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

    fn on_stream(&mut self, event: Stream) -> Result<()> {
        match event {
            Stream::MessageStart { message } => {
                if self.current_turn()?.is_some() {
                    let (message_id, model) = (message.id.to_owned(), message.model.map(str::to_owned));
                    self.stream = Some(Streaming { message_id, model, blocks: Vec::new(), finalized: 0 });
                }
            }
            Stream::ContentBlockStart { index, content_block } => {
                let (Some(turn), Some(stream)) = (self.turn(), &self.stream) else { return Ok(()) };
                let model = stream.model.clone();
                let block = match content_block {
                    Block::Text { text } => self.open_message(turn, MessageKind::Text, model, text)?,
                    Block::Thinking { thinking } => self.open_message(turn, MessageKind::Thinking, model, thinking)?,
                    Block::ToolUse { id, name, .. } => self.open_call(turn, id, name, "")?,
                    _ => NO_BLOCK,
                };
                if let Some(stream) = &mut self.stream {
                    stream.blocks.resize(stream.blocks.len().max(index), NO_BLOCK);
                    stream.blocks.push(block);
                }
            }
            Stream::ContentBlockDelta { index, delta } => {
                let Some(block) = self.stream.as_mut().and_then(|s| s.blocks.get_mut(index)) else { return Ok(()) };
                let (target, text) = match (delta, block.kind) {
                    (Delta::TextDelta { text }, Kind::Text) | (Delta::ThinkingDelta { thinking: text }, Kind::Thinking) => (block.node, text),
                    (Delta::InputJsonDelta { partial_json }, Kind::ToolUse) => (block.input, partial_json),
                    _ => return Ok(()),
                };
                if let Some(target) = target.filter(|_| !text.is_empty()) {
                    block.streamed = true;
                    self.w().append(target, text)?;
                }
            }
            Stream::ContentBlockStop { index } => {
                let block = self.stream.as_ref().and_then(|s| s.blocks.get(index));
                if let Some(StreamBlock { kind: Kind::Text | Kind::Thinking, node: Some(node), .. }) = block.copied() {
                    self.finish_message(node)?;
                }
            }
            Stream::Other => {}
        }
        Ok(())
    }

    fn open_message(&mut self, turn: NodeId, kind: MessageKind, model: Option<String>, text: &str) -> Result<StreamBlock> {
        let mut attrs = vec![(attr::ROLE, V::from(Role::Assistant)), (attr::STATUS, MessageStatus::Streaming.into())];
        attrs.extend((kind == MessageKind::Thinking).then(|| (attr::KIND, kind.into())));
        attrs.extend(model.map(|m| (attr::MODEL, m.into())));
        let node = self.w().insert(turn, E::Message, attrs, text)?;
        self.ledger.open(Key::Message(node), node);
        let kind = if kind == MessageKind::Thinking { Kind::Thinking } else { Kind::Text };
        Ok(StreamBlock { kind, node: Some(node), input: None, streamed: false })
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
        Ok(StreamBlock { kind: Kind::ToolUse, node: Some(call), input: Some(input), streamed: false })
    }

    /// A call's arguments are complete: title it and start the job a subagent call runs.
    fn complete_call(&mut self, call: NodeId, id: &str, tool: &str, input: &Value, uuid: Option<&str>) -> Result<()> {
        if let Some(uuid) = uuid {
            self.w().set(call, UUID_ATTR, uuid)?;
        }
        if let Some(title) = call_title(input) {
            self.w().set(call, attr::TITLE, title)?;
        }
        if !SUBAGENT_TOOLS.contains(&tool) || self.jobs.contains_key(id) {
            return Ok(());
        }
        let text = |key| input.get(key).and_then(Value::as_str);
        let mut attrs = vec![(attr::KIND, V::from(JobKind::Subagent)), (attr::STATUS, JobStatus::Running.into()), (attr::CALL, call.into()), (attr::STARTED, V::Int(now_ms()))];
        attrs.extend(text("subagent_type").map(|a| (attr::AGENT, a.into())));
        attrs.extend(text("description").map(|d| (attr::TITLE, d.into())));
        let job = self.w().insert(call, E::Job, attrs, "")?;
        self.jobs.insert(id.to_owned(), job);
        if input.get("run_in_background").and_then(Value::as_bool) != Some(true)
            && let Some(entry) = self.calls.get_mut(id)
        {
            entry.job = Some(job);
        }
        Ok(())
    }

    fn on_assistant(&mut self, msg: &Message, raw: &Value) -> Result<()> {
        let blocks = match msg.message.as_ref().and_then(|m| m.content.as_ref()) {
            Some(Content::Blocks(blocks)) => blocks.as_slice(),
            _ => &[],
        };
        if let Some(parent) = msg.parent_tool_use_id {
            // Subagent messages are summarized on their job.
            let Some(job) = self.jobs.get(parent).copied() else { return Ok(()) };
            for block in blocks {
                if let Block::Text { text } = block
                    && !text.is_empty()
                {
                    self.w().append(job, format!("{text}\n"))?;
                }
            }
            return Ok(());
        }
        let Some(turn) = self.current_turn()? else { return Ok(()) };
        let Some(body) = &msg.message else { return Ok(()) };
        let model = body.model.map(str::to_owned);
        for (at, block) in blocks.iter().enumerate() {
            let kind = kind_of(block);
            match (self.next_streamed(body.id, kind), block) {
                (Some(StreamBlock { node: Some(node), streamed, .. }), Block::Text { text } | Block::Thinking { thinking: text }) => {
                    if !streamed && !text.is_empty() {
                        self.sink.commit(Draft::set_text(node, *text))?;
                    }
                    self.finish_assistant_message(node, msg.uuid)?;
                }
                (Some(StreamBlock { node: Some(node), input, streamed, .. }), Block::ToolUse { id, name, input: args }) => {
                    if !streamed && let Some(input) = input {
                        self.sink.commit(Draft::set_text(input, args.to_string()))?;
                    }
                    self.complete_call(node, id, name, args, msg.uuid)?;
                }
                (Some(_), _) => {}
                (None, Block::Text { text }) => {
                    let block = self.open_message(turn, MessageKind::Text, model.clone(), text)?;
                    self.finish_assistant_message(block.node.unwrap_or(turn), msg.uuid)?;
                }
                (None, Block::Thinking { thinking }) => {
                    let block = self.open_message(turn, MessageKind::Thinking, model.clone(), thinking)?;
                    self.finish_assistant_message(block.node.unwrap_or(turn), msg.uuid)?;
                }
                (None, Block::ToolUse { id, name, input }) => {
                    let call = self.open_call(turn, id, name, &input.to_string())?.node.unwrap_or(turn);
                    self.complete_call(call, id, name, input, msg.uuid)?;
                }
                (None, _) => {
                    let raw = &raw["message"]["content"][at];
                    let kind = format!("block:{}", raw["type"].as_str().unwrap_or_default());
                    self.w().ext(turn, HARNESS, &kind, raw)?;
                }
            }
        }
        Ok(())
    }

    fn finish_assistant_message(&mut self, node: NodeId, uuid: Option<&str>) -> Result<()> {
        if let Some(uuid) = uuid {
            self.w().set(node, attr::ID, uuid)?;
        }
        self.finish_message(node)
    }

    /// Takes the next streamed block of `message_id` when it has `kind`.
    fn next_streamed(&mut self, message_id: &str, kind: Kind) -> Option<StreamBlock> {
        let stream = self.stream.as_mut().filter(|s| s.message_id == message_id)?;
        // Blocks the stream made no element for (signatures, redactions) are skipped.
        while stream.blocks.get(stream.finalized).is_some_and(|b| b.kind == Kind::Other && kind != Kind::Other) {
            stream.finalized += 1;
        }
        let block = *stream.blocks.get(stream.finalized).filter(|b| b.kind == kind && kind != Kind::Other)?;
        stream.finalized += 1;
        Some(block)
    }

    fn on_user(&mut self, msg: &Message, raw: &Value) -> Result<()> {
        // Tool results of an interrupted turn arrive after it was cancelled.
        if msg.parent_tool_use_id.is_some() || self.draining > 0 || msg.is_replay {
            return Ok(());
        }
        let Some(content) = msg.message.as_ref().and_then(|m| m.content.as_ref()) else { return Ok(()) };
        if let Some(compaction) = self.open_compaction.filter(|_| msg.is_compact_summary) {
            self.open_compaction = None;
            let text = content.text().unwrap_or_else(|| raw["message"]["content"].to_string());
            self.sink.commit(Draft::set_text(compaction, text))?;
            return Ok(());
        }
        let Content::Blocks(blocks) = content else { return self.ext("user", raw) };
        let mut rest = Vec::new();
        for (at, block) in blocks.iter().enumerate() {
            let raw_block = &raw["message"]["content"][at];
            match block {
                Block::ToolResult { tool_use_id, content, is_error } => {
                    self.on_tool_result(tool_use_id, content.as_ref(), *is_error, raw_block, msg.uuid)?;
                }
                _ => rest.push(raw_block.clone()),
            }
        }
        if rest.is_empty() {
            return Ok(());
        }
        let mut raw = raw.clone();
        raw["message"]["content"] = Value::Array(rest);
        self.ext("user", &raw)
    }

    fn on_tool_result(&mut self, id: &str, content: Option<&Content>, is_error: bool, raw: &Value, uuid: Option<&str>) -> Result<()> {
        let Some(&Call { node, job }) = self.calls.get(id) else {
            let parent = self.turn().unwrap_or(self.ledger.session);
            return Ok(self.w().ext(parent, HARNESS, "tool_result", raw).map(drop)?);
        };
        let (text, json) = match content.map(Content::text) {
            None => (String::new(), false),
            Some(Some(text)) => (text, false),
            Some(None) => (raw["content"].to_string(), true),
        };
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

    fn on_result(&mut self, msg: &TurnResult) -> Result<()> {
        if self.draining > 0 {
            // The result of an interrupted turn, already closed as cancelled.
            self.draining -= 1;
            return Ok(());
        }
        let Some(turn) = self.turn() else { return Ok(()) };
        let usage = msg.usage.as_ref();
        let tokens = [
            (attr::INPUT_TOKENS, usage.and_then(|u| u.input_tokens)),
            (attr::OUTPUT_TOKENS, usage.and_then(|u| u.output_tokens)),
            (attr::CACHE_READ_TOKENS, usage.and_then(|u| u.cache_read_input_tokens)),
            (attr::CACHE_WRITE_TOKENS, usage.and_then(|u| u.cache_creation_input_tokens)),
            (attr::DURATION_MS, msg.duration_ms),
        ];
        let mut attrs: Vec<(&str, V)> = tokens.into_iter().filter_map(|(name, n)| Some((name, V::Int(n?)))).collect();
        attrs.extend(msg.total_cost_usd.map(|cost| (attr::COST, V::Float(cost))));
        attrs.extend(self.model.as_deref().map(|model| (attr::MODEL, model.into())));
        if !attrs.is_empty() {
            self.w().insert(turn, E::Usage, attrs, "")?;
        }
        let status = if msg.terminal_reason.is_some_and(|r| r.starts_with("aborted")) {
            TurnStatus::Cancelled
        } else if msg.is_error || msg.subtype.is_some_and(|s| s != "success") {
            TurnStatus::Error
        } else {
            TurnStatus::Done
        };
        self.end_turn(status)
    }

    fn on_control(&mut self, request_id: &str, request: Option<Request>, raw: &Value) -> Result<Option<Output>> {
        let error = |e: &str| Ok(Some(Output::Write(control_error(request_id, e))));
        match request {
            None => error("missing request"),
            Some(Request::CanUseTool(permission)) => self.on_can_use_tool(request_id, permission),
            Some(Request::McpMessage { server_name, message }) => {
                Ok(Some(Output::Mcp { request_id: request_id.to_owned(), server: server_name.to_owned(), message }))
            }
            Some(Request::Other) => {
                error(&format!("unsupported control request `{}`", raw["request"]["subtype"].as_str().unwrap_or_default()))
            }
        }
    }

    fn on_can_use_tool(&mut self, request_id: &str, request: Permission) -> Result<Option<Output>> {
        let Some(turn) = self.current_turn()? else {
            // The turn it belongs to was interrupted.
            let deny = json!({"behavior": "deny", "message": "The turn was interrupted."});
            return Ok(Some(Output::Write(control_success(request_id, deny))));
        };
        let input = request.input;
        let call = request.tool_use_id.and_then(|id| self.calls.get(id)).map(|c| c.node);
        let now = V::Int(now_ms());
        let base = |kind: DecisionKind, title: Option<String>| {
            let mut attrs = vec![(attr::KIND, V::from(kind)), (attr::STATUS, DecisionStatus::Waiting.into()), (attr::REQUEST_ID, request_id.into()), (attr::CREATED, now.clone())];
            attrs.extend(call.map(|call| (attr::CALL, V::from(call))));
            attrs.extend(title.map(|t| (attr::TITLE, t.into())));
            attrs
        };
        let mut asks = Vec::new();
        let kind = match request.tool_name {
            "AskUserQuestion" => {
                for q in input.get("questions").and_then(Value::as_array).into_iter().flatten() {
                    let mut attrs = base(DecisionKind::Question, q.get("header").and_then(Value::as_str).map(str::to_owned));
                    let options = q.get("options").and_then(Value::as_array).into_iter().flatten();
                    attrs.push((attr::OPTIONS, V::List(options.filter_map(|o| o.get("label")?.as_str()).map(V::from).collect())));
                    attrs.push((attr::MULTI, V::Bool(q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false))));
                    asks.push((attrs, q.get("question").and_then(Value::as_str).unwrap_or_default().to_owned()));
                }
                PendingKind::Question
            }
            "ExitPlanMode" => {
                let plan = input.get("plan").and_then(Value::as_str).unwrap_or_default().to_owned();
                asks.push((base(DecisionKind::Plan, Some("Plan".to_owned())), plan));
                PendingKind::Plan
            }
            tool => {
                let title = request.title.map_or_else(|| format!("Allow {tool}?"), str::to_owned);
                let mut attrs = base(DecisionKind::Approval, Some(title));
                attrs.extend(request.decision_reason.map(|r| ("x-claude-decision-reason", r.into())));
                attrs.extend(request.blocked_path.map(|p| ("x-claude-blocked-path", p.into())));
                let text = request.description.map_or_else(|| serde_json::to_string_pretty(&input).unwrap_or_default(), str::to_owned);
                asks.push((attrs, text));
                PendingKind::Approval
            }
        };
        let parent = call.unwrap_or(turn);
        let mut decisions = Vec::with_capacity(asks.len());
        for (attrs, text) in asks {
            let decision = self.w().insert(parent, E::Decision, attrs, text)?;
            self.ledger.wait(decision);
            decisions.push((decision, None));
        }
        let tool = request.tool_name.to_owned();
        let pending = Pending { request_id: request_id.to_owned(), kind, tool, input, suggestions: request.permission_suggestions, decisions };
        if pending.decisions.is_empty() {
            // A question call with no questions has nothing to ask.
            return Ok(Some(Output::Write(control_success(request_id, permission_response(&pending)))));
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
            PendingKind::Approval => &[answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY],
            PendingKind::Plan => &[answer::APPROVE, answer::EDIT, answer::REJECT],
            PendingKind::Question => &[],
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
        if pending.kind == PendingKind::Plan && response["behavior"] == "allow" {
            self.record_mode(SessionMode::Normal)?;
        }
        self.resume_turn()?;
        Ok(Some(control_success(&pending.request_id, response)))
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
        (PendingKind::Approval, answer::ALLOW_SESSION) => {
            let rules = pending.suggestions.clone().unwrap_or_else(|| {
                json!([{"type": "addRules", "rules": [{"toolName": pending.tool}], "behavior": "allow", "destination": "session"}])
            });
            json!({"behavior": "allow", "updatedInput": input, "updatedPermissions": rules})
        }
        (PendingKind::Approval, answer::ALLOW_ONCE) | (PendingKind::Plan, answer::APPROVE) => allow(&input),
        (PendingKind::Approval, _) => json!({"behavior": "deny", "message": reason.unwrap_or("The user denied this tool call.")}),
        (PendingKind::Plan, answer::EDIT) => {
            if let (Some(plan), Some(object)) = (reason, input.as_object_mut()) {
                object.insert("plan".into(), plan.into());
            }
            allow(&input)
        }
        (PendingKind::Plan, _) => json!({"behavior": "deny", "message": reason.unwrap_or("The user rejected the plan.")}),
        (PendingKind::Question, _) => {
            let questions = pending.input.get("questions").and_then(Value::as_array);
            let mut answers = serde_json::Map::new();
            for (at, (_, answer)) in pending.decisions.iter().enumerate() {
                let question = questions.and_then(|q| q.get(at)?.get("question")?.as_str());
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
    let text = KEYS.iter().find_map(|k| input.get(k)?.as_str())?;
    let line = text.lines().next().unwrap_or_default();
    Some(match line.char_indices().nth(120) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    })
}
