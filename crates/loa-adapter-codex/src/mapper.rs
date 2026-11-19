//! Turns Codex app-server notifications and server requests into DOM patches.
//!
//! [`Mapper`] is a plain state machine: it reads JSON-RPC messages, commits
//! drafts to the session sink, and returns the replies the session must write
//! back to Codex. It never touches the process, so tests drive it with
//! scripted transcripts.

use crate::HARNESS;
use crate::json::{int_at, str_at};
use loa_adapter::{AdapterError, Answer, Prompt, Result, SessionConfig, SessionState};
use loa_core::{
    CallStatus, DecisionKind, DecisionStatus, Dom, Draft, ElementKind, JobKind, JobStatus, MessageKind, MessageStatus,
    NodeId, PatchSink, Role, SCHEMA_VERSION, SessionMode, TodoState, TurnStatus, Value, answer, attr, now_ms,
};
use serde_json::{Map, Value as Json, json};
use std::collections::HashMap;
use std::sync::Arc;

/// Tool names written to `<call tool>` for Codex's built in items. They match
/// the function names Codex uses in its rollouts.
pub mod tool {
    pub const EXEC: &str = "exec_command";
    pub const APPLY_PATCH: &str = "apply_patch";
    pub const WEB_SEARCH: &str = "web_search";
    pub const VIEW_IMAGE: &str = "view_image";
}

/// Marks an assistant `<message>` that holds a plan-mode `plan` item.
pub const X_ITEM: &str = "x-codex-item";
/// Exit code of a command, on its `<result>`.
pub const X_EXIT_CODE: &str = "x-exit-code";

/// JSON-RPC "method not found".
const METHOD_NOT_FOUND: i64 = -32601;

/// What the session must send back to Codex after a request or an answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    /// The result of server request `id`.
    Result { id: Json, result: Json },
    /// An error for server request `id`.
    Error { id: Json, code: i64, message: String },
    /// Codex asked to run an app tool (`item/tool/call`). The session runs it
    /// through the host tool handler and answers `id` with
    /// [`host_tool_result`].
    HostTool { id: Json, tool: String, arguments: String },
}

/// The response body for a dynamic tool call.
pub fn host_tool_result(outcome: std::result::Result<String, String>) -> Json {
    let (text, success) = match outcome {
        Ok(text) => (text, true),
        Err(text) => (text, false),
    };
    json!({ "contentItems": [{ "type": "inputText", "text": text }], "success": success })
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ItemKind {
    Message,
    Thinking,
    Call,
    UserMessage,
}

struct Item {
    node: NodeId,
    kind: ItemKind,
    /// Bytes appended through deltas.
    streamed: usize,
    /// `<result>` opened by streamed command output.
    result: Option<NodeId>,
    result_streamed: usize,
    /// Unified diff of a file change, the body of its approval decision.
    diff: String,
}

impl Item {
    fn new(node: NodeId, kind: ItemKind) -> Self {
        Self { node, kind, streamed: 0, result: None, result_streamed: 0, diff: String::new() }
    }
}

#[derive(Default)]
struct Usage {
    seen: bool,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
}

struct Turn {
    node: NodeId,
    id: Option<String>,
    /// The user message is in the DOM already.
    prompted: bool,
    items: HashMap<String, Item>,
    usage: Usage,
    /// Decisions in this turn still waiting.
    waiting: Vec<NodeId>,
}

#[derive(Clone, Debug)]
enum PendingKind {
    Command { session_option: bool },
    FileChange,
    Question { group: String, question: String },
    Elicitation { form: bool },
    Permissions { requested: Json },
}

struct Pending {
    rpc_id: Json,
    kind: PendingKind,
}

struct InputGroup {
    rpc_id: Json,
    waiting: usize,
    answers: Map<String, Json>,
}

/// Session DOM writer for one Codex thread.
pub struct Mapper {
    sink: Arc<dyn PatchSink>,
    session: NodeId,
    model: Option<String>,
    turns: Vec<NodeId>,
    /// Turns already covered by a `<compaction>`.
    covered: usize,
    turn: Option<Turn>,
    todos: Vec<NodeId>,
    /// Subagent `<job>` nodes by agent thread id.
    jobs: HashMap<String, NodeId>,
    decisions: HashMap<NodeId, Pending>,
    groups: HashMap<String, InputGroup>,
    closed: bool,
}

fn rpc_key(id: &Json) -> String {
    match id {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn node_int(id: NodeId) -> i64 {
    i64::try_from(id.0).unwrap_or(i64::MAX)
}

/// Text items of a `userMessage` content list, joined.
fn user_text(content: Option<&Json>) -> String {
    content
        .and_then(Json::as_array)
        .map(|items| items.iter().filter_map(|i| str_at(i, "text")).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

/// Summary sections then raw reasoning blocks, separated the way
/// `summaryPartAdded` separates streamed sections.
fn reasoning_text(item: &Json) -> String {
    let parts = ["summary", "content"]
        .iter()
        .filter_map(|key| item.get(*key).and_then(Json::as_array))
        .flatten()
        .filter_map(Json::as_str);
    parts.collect::<Vec<_>>().join("\n\n")
}

fn camel_to_snake(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4);
    for c in text.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Renders the `changes` of a `fileChange` item as one unified diff.
fn render_diff(changes: Option<&Json>) -> String {
    let mut out = String::new();
    for change in changes.and_then(Json::as_array).into_iter().flatten() {
        let path = str_at(change, "path").unwrap_or_default();
        let kind = change.get("kind").and_then(|k| str_at(k, "type")).unwrap_or("update");
        out.push_str(&format!("{kind} {path}\n"));
        if let Some(diff) = str_at(change, "diff") {
            out.push_str(diff);
            if !diff.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out
}

/// Text of MCP or dynamic tool content items; JSON when there is no text.
fn content_text(items: Option<&Json>) -> String {
    let Some(items) = items.and_then(Json::as_array) else { return String::new() };
    let texts: Vec<&str> = items.iter().filter_map(|i| str_at(i, "text")).collect();
    if texts.len() == items.len() { texts.join("\n") } else { Json::Array(items.clone()).to_string() }
}

impl Mapper {
    /// Starts the DOM of a new session for Codex thread `thread_id`.
    pub fn start(config: &SessionConfig, thread_id: &str, model: Option<&str>, sink: Arc<dyn PatchSink>) -> Result<Self> {
        let model = model.map(str::to_owned).or_else(|| config.model.clone());
        let mut attrs = vec![
            (attr::ID, Value::from(config.session_id.as_str())),
            (attr::HARNESS, Value::from(HARNESS)),
            (attr::NODE, Value::from(config.node.as_str())),
            (attr::CREATED, Value::Int(now_ms())),
            (attr::SCHEMA, Value::Int(i64::from(SCHEMA_VERSION))),
            (attr::CWD, Value::from(config.cwd.to_string_lossy().into_owned())),
            (attr::NATIVE_ID, Value::from(thread_id)),
        ];
        let optional = [
            (attr::PROJECT, config.project.clone()),
            (attr::MODEL, model.clone()),
            (attr::MODEL_ROLE, config.model_role.clone()),
            (attr::EFFORT, config.effort.clone()),
        ];
        attrs.extend(optional.into_iter().filter_map(|(name, value)| value.map(|v| (name, Value::from(v)))));
        if let Some(mode) = config.mode {
            attrs.push((attr::MODE, Value::from(mode)));
        }
        let session = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?.node;
        Ok(Self::empty(sink, session, model))
    }

    fn empty(sink: Arc<dyn PatchSink>, session: NodeId, model: Option<String>) -> Self {
        Self {
            sink,
            session,
            model,
            turns: Vec::new(),
            covered: 0,
            turn: None,
            todos: Vec::new(),
            jobs: HashMap::new(),
            decisions: HashMap::new(),
            groups: HashMap::new(),
            closed: false,
        }
    }

    /// Continues a session loaded from its journal. A turn the DOM still shows
    /// as running belonged to a Codex process that is gone, so it is cancelled.
    pub fn attach(dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Self> {
        let root = dom.root().filter(|r| r.kind == ElementKind::Session).ok_or(AdapterError::Format(
            "the DOM has no <session> root".to_owned(),
        ))?;
        let model = root.str_attr(attr::MODEL).map(str::to_owned);
        let mut mapper = Self::empty(sink, root.id, model);
        for child in dom.children(root.id) {
            match child.kind {
                ElementKind::Turn => mapper.turns.push(child.id),
                ElementKind::Todo => mapper.todos.push(child.id),
                ElementKind::Compaction => {
                    let last = child.attr(attr::COVERS).map(|c| c.as_list().unwrap_or_default()).and_then(|c| c.last());
                    if let Some(last) = last.and_then(Value::as_int) {
                        mapper.covered = mapper.turns.iter().position(|t| node_int(*t) == last).map_or(0, |p| p + 1);
                    }
                }
                _ => {}
            }
        }
        let stale: Vec<NodeId> = dom
            .descendants(root.id)
            .filter(|n| match n.kind {
                ElementKind::Turn => matches!(n.str_attr(attr::STATUS), Some("running" | "waiting")),
                ElementKind::Message => n.str_attr(attr::STATUS) == Some(MessageStatus::Streaming.as_str()),
                ElementKind::Call => matches!(n.str_attr(attr::STATUS), Some("running" | "pending")),
                ElementKind::Decision => n.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str()),
                ElementKind::Job => n.str_attr(attr::STATUS) == Some(JobStatus::Running.as_str()),
                _ => false,
            })
            .map(|n| n.id)
            .collect();
        let now = Value::Int(now_ms());
        for id in stale {
            // Turns come before their children in document order, so children
            // are cancelled after the turn; every status set is independent.
            let kind = dom.get(id).map(|n| n.kind);
            mapper.commit(Draft::set(id, attr::STATUS, "cancelled"))?;
            if matches!(kind, Some(ElementKind::Turn | ElementKind::Call | ElementKind::Decision | ElementKind::Job)) {
                mapper.commit(Draft::set(id, attr::ENDED, now.clone()))?;
            }
        }
        Ok(mapper)
    }

    fn commit(&self, draft: Draft) -> Result<NodeId> {
        Ok(self.sink.commit(draft)?.node)
    }

    pub fn session_node(&self) -> NodeId {
        self.session
    }

    pub fn state(&self) -> SessionState {
        match &self.turn {
            _ if self.closed => SessionState::Closed,
            None => SessionState::Idle,
            Some(turn) if !turn.waiting.is_empty() => SessionState::Waiting,
            Some(_) => SessionState::Running,
        }
    }

    /// Codex's id for the running turn, once known.
    pub fn turn_id(&self) -> Option<&str> {
        self.turn.as_ref()?.id.as_deref()
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Records the prompt the app is about to send: a new `<turn>` with the
    /// user `<message>`.
    pub fn begin_turn(&mut self, prompt: &Prompt) -> Result<()> {
        if self.turn.is_some() {
            return Err(AdapterError::Busy);
        }
        let node = self.commit(Draft::insert(
            self.session,
            ElementKind::Turn,
            [(attr::STATUS, Value::from(TurnStatus::Running)), (attr::STARTED, Value::Int(now_ms()))],
            "",
        ))?;
        self.commit(Draft::insert(
            node,
            ElementKind::Message,
            [
                (attr::ROLE, Value::from(Role::User)),
                (attr::STATUS, Value::from(MessageStatus::Done)),
                (attr::BY, Value::from(prompt.by.as_str())),
            ],
            prompt.text.as_str(),
        ))?;
        self.turns.push(node);
        self.turn = Some(Turn { node, id: None, prompted: true, items: HashMap::new(), usage: Usage::default(), waiting: Vec::new() });
        Ok(())
    }

    /// `turn/start` answered with the turn id.
    pub fn turn_accepted(&mut self, turn_id: &str) -> Result<()> {
        let Some(turn) = self.turn.as_mut() else { return Ok(()) };
        if turn.id.is_none() {
            turn.id = Some(turn_id.to_owned());
            let node = turn.node;
            self.commit(Draft::set(node, attr::ID, turn_id))?;
        }
        Ok(())
    }

    /// `turn/start` failed: the turn ends with the error.
    pub fn turn_rejected(&mut self, message: &str) -> Result<()> {
        if let Some(turn) = &self.turn {
            let node = turn.node;
            self.ext(node, "error", message)?;
        }
        self.end_turn(TurnStatus::Error)
    }

    /// The Codex process exited. A running turn ends with an error.
    pub fn process_exited(&mut self, detail: &str) -> Result<()> {
        if let Some(turn) = &self.turn {
            let node = turn.node;
            self.ext(node, "exit", detail)?;
            self.end_turn(TurnStatus::Error)?;
        }
        self.closed = true;
        Ok(())
    }

    pub fn set_model(&mut self, model: &str) -> Result<()> {
        self.model = Some(model.to_owned());
        self.commit(Draft::set(self.session, attr::MODEL, model))?;
        Ok(())
    }

    pub fn set_mode(&mut self, mode: SessionMode) -> Result<()> {
        self.commit(Draft::set(self.session, attr::MODE, mode))?;
        Ok(())
    }

    fn ext(&self, parent: NodeId, ty: &str, text: &str) -> Result<NodeId> {
        self.commit(Draft::insert(
            parent,
            ElementKind::Ext,
            [(attr::HARNESS, Value::from(HARNESS)), (attr::TYPE, Value::from(ty))],
            text,
        ))
    }

    /// Opens a turn Codex started on its own (no prompt from the app).
    fn ensure_turn(&mut self, turn_id: Option<&str>) -> Result<&mut Turn> {
        let open = match (&self.turn, turn_id) {
            (None, _) => true,
            (Some(turn), Some(id)) => turn.id.as_deref().is_some_and(|current| current != id),
            (Some(_), None) => false,
        };
        if open {
            self.end_turn(TurnStatus::Done)?;
            let mut attrs = vec![(attr::STATUS, Value::from(TurnStatus::Running)), (attr::STARTED, Value::Int(now_ms()))];
            if let Some(id) = turn_id {
                attrs.push((attr::ID, Value::from(id)));
            }
            let node = self.commit(Draft::insert(self.session, ElementKind::Turn, attrs, ""))?;
            self.turns.push(node);
            self.turn = Some(Turn {
                node,
                id: turn_id.map(str::to_owned),
                prompted: false,
                items: HashMap::new(),
                usage: Usage::default(),
                waiting: Vec::new(),
            });
        } else if let (Some(turn), Some(id)) = (self.turn.as_mut(), turn_id)
            && turn.id.is_none()
        {
            turn.id = Some(id.to_owned());
            let node = turn.node;
            self.commit(Draft::set(node, attr::ID, id))?;
        }
        self.turn.as_mut().ok_or_else(|| AdapterError::Protocol("no turn".to_owned()))
    }

    /// Handles one notification.
    pub fn notification(&mut self, method: &str, params: &Json) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        let turn_id = str_at(params, "turnId");
        match method {
            "turn/started" => {
                let id = params.get("turn").and_then(|t| str_at(t, "id"));
                self.ensure_turn(id)?;
            }
            "turn/completed" => self.turn_completed(params.get("turn").unwrap_or(&Json::Null))?,
            "item/started" => {
                if let Some(item) = params.get("item") {
                    self.item_started(turn_id, item)?;
                }
            }
            "item/completed" => {
                if let Some(item) = params.get("item") {
                    self.item_completed(turn_id, item)?;
                }
            }
            "item/agentMessage/delta" | "item/plan/delta" => {
                let kind = if method == "item/plan/delta" { "plan" } else { "agentMessage" };
                self.text_delta(turn_id, params, ItemKind::Message, kind)?;
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                self.text_delta(turn_id, params, ItemKind::Thinking, "reasoning")?;
            }
            "item/reasoning/summaryPartAdded" => self.summary_part(params)?,
            "item/commandExecution/outputDelta" | "item/fileChange/outputDelta" => self.output_delta(params)?,
            "turn/plan/updated" => self.plan_updated(params)?,
            "thread/tokenUsage/updated" => self.token_usage(params),
            "model/rerouted" => {
                if let Some(model) = str_at(params, "toModel") {
                    self.set_model(model)?;
                }
            }
            "serverRequest/resolved" => {
                if let Some(id) = params.get("requestId") {
                    self.request_cleared(id)?;
                }
            }
            "error" => {
                let retrying = params.get("willRetry").and_then(Json::as_bool).unwrap_or(false);
                if !retrying && let Some(turn) = &self.turn {
                    let node = turn.node;
                    let message = params.get("error").and_then(|e| str_at(e, "message")).unwrap_or_default();
                    self.ext(node, "error", message)?;
                }
            }
            _ => {
                // Unknown turn scoped notifications are kept; thread and
                // account housekeeping has no place in the session.
                if let (Some(turn), Some(id)) = (&self.turn, turn_id)
                    && turn.id.as_deref() == Some(id)
                {
                    let node = turn.node;
                    self.ext(node, method, &params.to_string())?;
                }
            }
        }
        Ok(())
    }

    fn message_attrs(&self, kind: ItemKind, item_id: &str, status: MessageStatus) -> Vec<(&'static str, Value)> {
        let mut attrs = vec![
            (attr::ROLE, Value::from(Role::Assistant)),
            (attr::STATUS, Value::from(status)),
            (attr::ID, Value::from(item_id)),
        ];
        if kind == ItemKind::Thinking {
            attrs.push((attr::KIND, Value::from(MessageKind::Thinking)));
        }
        if let Some(model) = &self.model {
            attrs.push((attr::MODEL, Value::from(model.as_str())));
        }
        attrs
    }

    /// The message node of item `id`, inserted when the start event was missed.
    fn message_item(&mut self, turn_id: Option<&str>, id: &str, kind: ItemKind, item_type: &str) -> Result<NodeId> {
        if let Some(item) = self.turn.as_ref().and_then(|t| t.items.get(id)) {
            return Ok(item.node);
        }
        let mut attrs = self.message_attrs(kind, id, MessageStatus::Streaming);
        if item_type == "plan" {
            attrs.push((X_ITEM, Value::from("plan")));
        }
        let parent = self.ensure_turn(turn_id)?.node;
        let node = self.commit(Draft::insert(parent, ElementKind::Message, attrs, ""))?;
        if let Some(turn) = self.turn.as_mut() {
            turn.items.insert(id.to_owned(), Item::new(node, kind));
        }
        Ok(node)
    }

    fn text_delta(&mut self, turn_id: Option<&str>, params: &Json, kind: ItemKind, item_type: &str) -> Result<()> {
        let (Some(id), Some(delta)) = (str_at(params, "itemId"), str_at(params, "delta")) else { return Ok(()) };
        if delta.is_empty() {
            return Ok(());
        }
        let node = self.message_item(turn_id, id, kind, item_type)?;
        self.commit(Draft::append(node, delta))?;
        if let Some(item) = self.turn.as_mut().and_then(|t| t.items.get_mut(id)) {
            item.streamed += delta.len();
        }
        Ok(())
    }

    fn summary_part(&mut self, params: &Json) -> Result<()> {
        let Some(id) = str_at(params, "itemId") else { return Ok(()) };
        let Some(item) = self.turn.as_mut().and_then(|t| t.items.get_mut(id)) else { return Ok(()) };
        if item.streamed == 0 {
            return Ok(());
        }
        item.streamed += 2;
        let node = item.node;
        self.commit(Draft::append(node, "\n\n"))?;
        Ok(())
    }

    fn output_delta(&mut self, params: &Json) -> Result<()> {
        let (Some(id), Some(delta)) = (str_at(params, "itemId"), str_at(params, "delta")) else { return Ok(()) };
        let Some(item) = self.turn.as_ref().and_then(|t| t.items.get(id)) else { return Ok(()) };
        let (call, result) = (item.node, item.result);
        let result = match result {
            Some(result) => {
                self.commit(Draft::append(result, delta))?;
                result
            }
            None => self.commit(Draft::insert(call, ElementKind::Result, Vec::<(&str, Value)>::new(), delta))?,
        };
        if let Some(item) = self.turn.as_mut().and_then(|t| t.items.get_mut(id)) {
            item.result = Some(result);
            item.result_streamed += delta.len();
        }
        Ok(())
    }

    fn insert_call(&mut self, turn_id: Option<&str>, item: &Json, tool: &str, title: Option<String>, input: Json) -> Result<()> {
        let Some(id) = str_at(item, "id") else { return Ok(()) };
        if self.turn.as_ref().is_some_and(|t| t.items.contains_key(id)) {
            return Ok(());
        }
        let started = int_at(item, "startedAtMs").unwrap_or_else(now_ms);
        let mut attrs = vec![
            (attr::TOOL, Value::from(tool)),
            (attr::CALL_ID, Value::from(id)),
            (attr::STATUS, Value::from(CallStatus::Running)),
            (attr::STARTED, Value::Int(started)),
        ];
        if let Some(title) = title {
            attrs.push((attr::TITLE, Value::from(title)));
        }
        let parent = self.ensure_turn(turn_id)?.node;
        let node = self.commit(Draft::insert(parent, ElementKind::Call, attrs, ""))?;
        self.commit(Draft::insert(node, ElementKind::Input, Vec::<(&str, Value)>::new(), input.to_string()))?;
        let mut entry = Item::new(node, ItemKind::Call);
        if str_at(item, "type") == Some("fileChange") {
            entry.diff = render_diff(item.get("changes"));
        }
        if let Some(turn) = self.turn.as_mut() {
            turn.items.insert(id.to_owned(), entry);
        }
        Ok(())
    }

    /// Inserts the `<call>` for a tool item. Returns false for item types that aren't calls.
    fn start_call(&mut self, turn_id: Option<&str>, item: &Json) -> Result<bool> {
        let ty = str_at(item, "type").unwrap_or_default();
        match ty {
            "commandExecution" => {
                let command = str_at(item, "command").unwrap_or_default();
                let input = json!({ "command": command, "cwd": item.get("cwd") });
                self.insert_call(turn_id, item, tool::EXEC, Some(command.to_owned()), input)?;
            }
            "fileChange" => {
                let paths: Vec<&str> = item
                    .get("changes")
                    .and_then(Json::as_array)
                    .map(|c| c.iter().filter_map(|c| str_at(c, "path")).collect())
                    .unwrap_or_default();
                let title = format!("Edit {}", paths.join(", "));
                let input = json!({ "changes": item.get("changes") });
                self.insert_call(turn_id, item, tool::APPLY_PATCH, Some(title), input)?;
            }
            "mcpToolCall" => {
                let server = str_at(item, "server").unwrap_or_default();
                let name = str_at(item, "tool").unwrap_or_default();
                let tool = format!("mcp__{server}__{name}");
                let input = item.get("arguments").cloned().unwrap_or(Json::Null);
                self.insert_call(turn_id, item, &tool, Some(format!("{server}: {name}")), input)?;
            }
            "dynamicToolCall" => {
                let name = str_at(item, "tool").unwrap_or_default();
                let tool = match str_at(item, "namespace") {
                    Some(namespace) => format!("{namespace}.{name}"),
                    None => name.to_owned(),
                };
                let input = item.get("arguments").cloned().unwrap_or(Json::Null);
                self.insert_call(turn_id, item, &tool, None, input)?;
            }
            "collabAgentToolCall" => {
                let tool = camel_to_snake(str_at(item, "tool").unwrap_or("agent"));
                let input = json!({
                    "prompt": item.get("prompt"),
                    "model": item.get("model"),
                    "receiverThreadIds": item.get("receiverThreadIds"),
                });
                let title = str_at(item, "prompt").map(|p| p.lines().next().unwrap_or_default().to_owned());
                self.insert_call(turn_id, item, &tool, title, input)?;
            }
            "webSearch" => {
                let query = str_at(item, "query").unwrap_or_default();
                self.insert_call(turn_id, item, tool::WEB_SEARCH, Some(query.to_owned()), json!({ "query": query }))?;
            }
            "imageView" => {
                let path = str_at(item, "path").unwrap_or_default();
                self.insert_call(turn_id, item, tool::VIEW_IMAGE, Some(path.to_owned()), json!({ "path": path }))?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn item_started(&mut self, turn_id: Option<&str>, item: &Json) -> Result<()> {
        let Some(id) = str_at(item, "id") else { return Ok(()) };
        match str_at(item, "type").unwrap_or_default() {
            "userMessage" => self.user_message(turn_id, id, item)?,
            "agentMessage" => {
                let node = self.message_item(turn_id, id, ItemKind::Message, "agentMessage")?;
                self.seed_text(id, node, str_at(item, "text").unwrap_or_default())?;
            }
            "plan" => {
                let node = self.message_item(turn_id, id, ItemKind::Message, "plan")?;
                self.seed_text(id, node, str_at(item, "text").unwrap_or_default())?;
            }
            "reasoning" => {
                let node = self.message_item(turn_id, id, ItemKind::Thinking, "reasoning")?;
                self.seed_text(id, node, &reasoning_text(item))?;
            }
            _ => {
                self.start_call(turn_id, item)?;
            }
        }
        Ok(())
    }

    /// Text an item already carries when it starts counts as streamed.
    fn seed_text(&mut self, id: &str, node: NodeId, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let Some(item) = self.turn.as_mut().and_then(|t| t.items.get_mut(id)) else { return Ok(()) };
        if item.streamed > 0 {
            return Ok(());
        }
        item.streamed = text.len();
        self.commit(Draft::append(node, text))?;
        Ok(())
    }

    fn user_message(&mut self, turn_id: Option<&str>, id: &str, item: &Json) -> Result<()> {
        let turn = self.ensure_turn(turn_id)?;
        if turn.items.contains_key(id) {
            return Ok(());
        }
        if turn.prompted {
            // The prompt the app sent; its message is in the DOM already.
            let node = turn.node;
            turn.items.insert(id.to_owned(), Item::new(node, ItemKind::UserMessage));
            return Ok(());
        }
        turn.prompted = true;
        let parent = turn.node;
        let text = user_text(item.get("content"));
        let node = self.commit(Draft::insert(
            parent,
            ElementKind::Message,
            [
                (attr::ROLE, Value::from(Role::User)),
                (attr::STATUS, Value::from(MessageStatus::Done)),
                (attr::ID, Value::from(id)),
            ],
            text,
        ))?;
        if let Some(turn) = self.turn.as_mut() {
            turn.items.insert(id.to_owned(), Item::new(node, ItemKind::UserMessage));
        }
        Ok(())
    }

    fn finish_message(&mut self, turn_id: Option<&str>, id: &str, kind: ItemKind, item_type: &str, text: &str) -> Result<()> {
        let node = self.message_item(turn_id, id, kind, item_type)?;
        let streamed = self.turn.as_ref().and_then(|t| t.items.get(id)).map_or(0, |i| i.streamed);
        if streamed != text.len() {
            self.commit(Draft::set_text(node, text))?;
        }
        self.commit(Draft::set(node, attr::STATUS, MessageStatus::Done))?;
        if let Some(turn) = self.turn.as_mut() {
            turn.items.remove(id);
        }
        Ok(())
    }

    fn item_completed(&mut self, turn_id: Option<&str>, item: &Json) -> Result<()> {
        let Some(id) = str_at(item, "id") else { return Ok(()) };
        let ty = str_at(item, "type").unwrap_or_default();
        match ty {
            "userMessage" => self.user_message(turn_id, id, item),
            "agentMessage" => {
                self.finish_message(turn_id, id, ItemKind::Message, ty, str_at(item, "text").unwrap_or_default())
            }
            "plan" => self.finish_message(turn_id, id, ItemKind::Message, ty, str_at(item, "text").unwrap_or_default()),
            "reasoning" => self.finish_message(turn_id, id, ItemKind::Thinking, ty, &reasoning_text(item)),
            "contextCompaction" => {
                let covers: Vec<Value> = self.turns[self.covered..].iter().map(|t| Value::Int(node_int(*t))).collect();
                self.covered = self.turns.len();
                self.commit(Draft::insert(
                    self.session,
                    ElementKind::Compaction,
                    [(attr::COVERS, Value::List(covers)), (attr::CREATED, Value::Int(now_ms()))],
                    "",
                ))?;
                Ok(())
            }
            _ => {
                if !self.turn.as_ref().is_some_and(|t| t.items.contains_key(id)) && !self.start_call(turn_id, item)? {
                    let parent = self.ensure_turn(turn_id)?.node;
                    self.ext(parent, &format!("item:{ty}"), &item.to_string())?;
                    return Ok(());
                }
                self.finish_call(id, item)
            }
        }
    }

    fn finish_call(&mut self, id: &str, item: &Json) -> Result<()> {
        let Some(entry) = self.turn.as_mut().and_then(|t| t.items.remove(id)) else { return Ok(()) };
        let status_text = str_at(item, "status").unwrap_or("completed");
        let mut exit_code = None;
        let (output, mut status) = match str_at(item, "type").unwrap_or_default() {
            "commandExecution" => {
                exit_code = int_at(item, "exitCode");
                (str_at(item, "aggregatedOutput").map(str::to_owned), CallStatus::Done)
            }
            "fileChange" => {
                let summary: Vec<String> = item
                    .get("changes")
                    .and_then(Json::as_array)
                    .into_iter()
                    .flatten()
                    .map(|c| {
                        let kind = c.get("kind").and_then(|k| str_at(k, "type")).unwrap_or("update");
                        format!("{kind} {}", str_at(c, "path").unwrap_or_default())
                    })
                    .collect();
                (Some(summary.join("\n")), CallStatus::Done)
            }
            "mcpToolCall" => match item.get("error").and_then(|e| str_at(e, "message")) {
                Some(message) => (Some(message.to_owned()), CallStatus::Error),
                None => (Some(content_text(item.get("result").and_then(|r| r.get("content")))), CallStatus::Done),
            },
            "dynamicToolCall" => {
                let failed = item.get("success").and_then(Json::as_bool) == Some(false);
                let status = if failed { CallStatus::Error } else { CallStatus::Done };
                (Some(content_text(item.get("contentItems"))), status)
            }
            "collabAgentToolCall" => {
                self.agent_jobs(entry.node, item)?;
                (Some(item.get("agentsStates").map(Json::to_string).unwrap_or_default()), CallStatus::Done)
            }
            _ => (None, CallStatus::Done),
        };
        status = match status_text {
            "failed" => CallStatus::Error,
            "declined" => CallStatus::Cancelled,
            "interrupted" => CallStatus::Cancelled,
            _ if exit_code.is_some_and(|c| c != 0) => CallStatus::Error,
            _ => status,
        };
        let is_error = matches!(status, CallStatus::Error);
        let result = match entry.result {
            Some(result) => {
                if let Some(output) = &output
                    && output.len() != entry.result_streamed
                {
                    self.commit(Draft::set_text(result, output.as_str()))?;
                }
                self.commit(Draft::set(result, attr::IS_ERROR, is_error))?;
                result
            }
            None => self.commit(Draft::insert(
                entry.node,
                ElementKind::Result,
                [(attr::IS_ERROR, Value::Bool(is_error))],
                output.unwrap_or_default(),
            ))?,
        };
        if let Some(code) = exit_code {
            self.commit(Draft::set(result, X_EXIT_CODE, Value::Int(code)))?;
        }
        if let Some(duration) = int_at(item, "durationMs") {
            self.commit(Draft::insert(entry.node, ElementKind::Usage, [(attr::DURATION_MS, Value::Int(duration))], ""))?;
        }
        self.commit(Draft::set(entry.node, attr::STATUS, status))?;
        let ended = int_at(item, "completedAtMs").unwrap_or_else(now_ms);
        self.commit(Draft::set(entry.node, attr::ENDED, Value::Int(ended)))?;
        Ok(())
    }

    /// `<job kind="subagent">` per agent thread a spawn reaches; later collab
    /// calls about the same thread update its status.
    fn agent_jobs(&mut self, call: NodeId, item: &Json) -> Result<()> {
        let Some(parent) = self.turn.as_ref().map(|t| t.node) else { return Ok(()) };
        let spawn = str_at(item, "tool") == Some("spawnAgent");
        let states = item.get("agentsStates");
        for thread in item.get("receiverThreadIds").and_then(Json::as_array).into_iter().flatten().filter_map(Json::as_str) {
            let state = states.and_then(|s| s.get(thread)).and_then(|s| str_at(s, "status"));
            let status = match state {
                Some("completed" | "shutdown") => JobStatus::Done,
                Some("errored" | "notFound") => JobStatus::Error,
                Some("interrupted") => JobStatus::Cancelled,
                _ => JobStatus::Running,
            };
            match self.jobs.get(thread).copied() {
                Some(job) if state.is_some() => {
                    self.commit(Draft::set(job, attr::STATUS, status))?;
                    if status != JobStatus::Running {
                        self.commit(Draft::set(job, attr::ENDED, Value::Int(now_ms())))?;
                    }
                }
                Some(_) => {}
                None if spawn => {
                    let mut attrs = vec![
                        (attr::KIND, Value::from(JobKind::Subagent)),
                        (attr::STATUS, Value::from(status)),
                        (attr::ID, Value::from(thread)),
                        (attr::AGENT, Value::from(thread)),
                        (attr::CALL, Value::Int(node_int(call))),
                        (attr::STARTED, Value::Int(now_ms())),
                    ];
                    if let Some(prompt) = str_at(item, "prompt") {
                        attrs.push((attr::TITLE, Value::from(prompt.lines().next().unwrap_or_default())));
                    }
                    let job = self.commit(Draft::insert(parent, ElementKind::Job, attrs, ""))?;
                    self.jobs.insert(thread.to_owned(), job);
                }
                None => {}
            }
        }
        Ok(())
    }

    fn plan_updated(&mut self, params: &Json) -> Result<()> {
        let steps = params.get("plan").and_then(Json::as_array).cloned().unwrap_or_default();
        for (index, step) in steps.iter().enumerate() {
            let state = match str_at(step, "status") {
                Some("completed") => TodoState::Done,
                Some("inProgress") => TodoState::InProgress,
                _ => TodoState::Pending,
            };
            let text = str_at(step, "step").unwrap_or_default();
            match self.todos.get(index).copied() {
                Some(todo) => {
                    self.commit(Draft::set(todo, attr::STATE, state))?;
                    self.commit(Draft::set_text(todo, text))?;
                }
                None => {
                    let todo = self.commit(Draft::insert(self.session, ElementKind::Todo, [(attr::STATE, Value::from(state))], text))?;
                    self.todos.push(todo);
                }
            }
        }
        for todo in self.todos.drain(steps.len()..).collect::<Vec<_>>() {
            self.commit(Draft::set(todo, attr::STATE, TodoState::Cancelled))?;
        }
        Ok(())
    }

    fn token_usage(&mut self, params: &Json) {
        let Some(last) = params.get("tokenUsage").and_then(|u| u.get("last")) else { return };
        let Some(turn) = self.turn.as_mut() else { return };
        let usage = &mut turn.usage;
        usage.seen = true;
        usage.input += int_at(last, "inputTokens").unwrap_or(0);
        usage.output += int_at(last, "outputTokens").unwrap_or(0);
        usage.cache_read += int_at(last, "cachedInputTokens").unwrap_or(0);
        usage.cache_write += int_at(last, "cacheWriteInputTokens").unwrap_or(0);
    }

    fn turn_completed(&mut self, turn: &Json) -> Result<()> {
        let status = match str_at(turn, "status") {
            Some("completed") => TurnStatus::Done,
            Some("interrupted") => TurnStatus::Cancelled,
            Some("failed") => TurnStatus::Error,
            _ => return Ok(()),
        };
        if let Some(id) = str_at(turn, "id")
            && self.turn.as_ref().is_some_and(|t| t.id.as_deref().is_some_and(|current| current != id))
        {
            // A turn Codex never announced to this session.
            return Ok(());
        }
        if let (Some(open), Some(message)) = (&self.turn, turn.get("error").and_then(|e| str_at(e, "message"))) {
            let node = open.node;
            self.ext(node, "error", message)?;
        }
        self.end_turn(status)
    }

    /// Ends the open turn with `status`: open messages, calls and decisions
    /// end with it, usage is recorded.
    fn end_turn(&mut self, status: TurnStatus) -> Result<()> {
        let Some(turn) = self.turn.take() else { return Ok(()) };
        let (message_status, call_status) = match status {
            TurnStatus::Done => (MessageStatus::Done, CallStatus::Cancelled),
            TurnStatus::Error => (MessageStatus::Error, CallStatus::Error),
            _ => (MessageStatus::Cancelled, CallStatus::Cancelled),
        };
        let now = Value::Int(now_ms());
        for item in turn.items.values() {
            match item.kind {
                ItemKind::Message | ItemKind::Thinking => {
                    self.commit(Draft::set(item.node, attr::STATUS, message_status))?;
                }
                ItemKind::Call => {
                    self.commit(Draft::set(item.node, attr::STATUS, call_status))?;
                    self.commit(Draft::set(item.node, attr::ENDED, now.clone()))?;
                }
                ItemKind::UserMessage => {}
            }
        }
        for decision in &turn.waiting {
            self.close_decision(*decision, DecisionStatus::Cancelled)?;
        }
        if turn.usage.seen {
            let usage = &turn.usage;
            self.commit(Draft::insert(
                turn.node,
                ElementKind::Usage,
                [
                    (attr::INPUT_TOKENS, Value::Int(usage.input)),
                    (attr::OUTPUT_TOKENS, Value::Int(usage.output)),
                    (attr::CACHE_READ_TOKENS, Value::Int(usage.cache_read)),
                    (attr::CACHE_WRITE_TOKENS, Value::Int(usage.cache_write)),
                ],
                "",
            ))?;
        }
        self.commit(Draft::set(turn.node, attr::STATUS, status))?;
        self.commit(Draft::set(turn.node, attr::ENDED, now))?;
        Ok(())
    }

    fn close_decision(&mut self, decision: NodeId, status: DecisionStatus) -> Result<()> {
        if let Some(pending) = self.decisions.remove(&decision)
            && let PendingKind::Question { group, .. } = pending.kind
        {
            self.groups.remove(&group);
        }
        self.commit(Draft::set(decision, attr::STATUS, status))?;
        self.commit(Draft::set(decision, attr::ENDED, Value::Int(now_ms())))?;
        Ok(())
    }

    /// Codex cleared a request on its own (turn ended, interrupted).
    fn request_cleared(&mut self, rpc_id: &Json) -> Result<()> {
        let cleared: Vec<NodeId> =
            self.decisions.iter().filter(|(_, p)| &p.rpc_id == rpc_id).map(|(node, _)| *node).collect();
        for decision in cleared {
            self.close_decision(decision, DecisionStatus::Cancelled)?;
            self.unwait(decision)?;
        }
        Ok(())
    }

    /// Takes `decision` off the turn's waiting list; the turn runs again when none are left.
    fn unwait(&mut self, decision: NodeId) -> Result<()> {
        let Some(turn) = self.turn.as_mut() else { return Ok(()) };
        let before = turn.waiting.len();
        turn.waiting.retain(|d| *d != decision);
        if before > 0 && turn.waiting.is_empty() {
            let node = turn.node;
            self.commit(Draft::set(node, attr::STATUS, TurnStatus::Running))?;
        }
        Ok(())
    }

    fn insert_decision(
        &mut self,
        (parent, call): (NodeId, Option<NodeId>),
        rpc_id: &Json,
        kind: DecisionKind,
        mut attrs: Vec<(&'static str, Value)>,
        text: String,
        pending: PendingKind,
    ) -> Result<NodeId> {
        attrs.push((attr::KIND, Value::from(kind)));
        attrs.push((attr::STATUS, Value::from(DecisionStatus::Waiting)));
        attrs.push((attr::REQUEST_ID, Value::from(rpc_key(rpc_id))));
        attrs.push((attr::CREATED, Value::Int(now_ms())));
        if let Some(call) = call {
            attrs.push((attr::CALL, Value::Int(node_int(call))));
        }
        let decision = self.commit(Draft::insert(parent, ElementKind::Decision, attrs, text))?;
        self.decisions.insert(decision, Pending { rpc_id: rpc_id.clone(), kind: pending });
        if let Some(turn) = self.turn.as_mut() {
            let was_running = turn.waiting.is_empty();
            turn.waiting.push(decision);
            if was_running {
                let node = turn.node;
                self.commit(Draft::set(node, attr::STATUS, TurnStatus::Waiting))?;
            }
        }
        Ok(decision)
    }

    /// The `<call>` of item `itemId` and the node a decision about it sits under.
    fn decision_target(&mut self, params: &Json) -> Result<(NodeId, Option<NodeId>)> {
        let call = str_at(params, "itemId").and_then(|id| self.turn.as_ref()?.items.get(id)).map(|i| i.node);
        if let Some(call) = call {
            return Ok((call, Some(call)));
        }
        let parent = match (&self.turn, str_at(params, "turnId")) {
            (Some(turn), _) => turn.node,
            (None, Some(id)) => self.ensure_turn(Some(id))?.node,
            (None, None) => self.session,
        };
        Ok((parent, None))
    }

    /// Handles one server request. Returns the reply to write now, if any;
    /// approvals and questions are answered later through [`Mapper::answer`].
    pub fn request(&mut self, id: Json, method: &str, params: &Json) -> Result<Option<Reply>> {
        let approval_options = |session: bool| {
            let mut options = vec![Value::from(answer::ALLOW_ONCE)];
            if session {
                options.push(Value::from(answer::ALLOW_SESSION));
            }
            options.push(Value::from(answer::DENY));
            Value::List(options)
        };
        let with_reason = |mut text: String| {
            if let Some(reason) = str_at(params, "reason") {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(reason);
            }
            text
        };
        match method {
            "item/commandExecution/requestApproval" => {
                let (parent, call) = self.decision_target(params)?;
                let session_option = str_at(params, "kind") != Some("humanCommand");
                let (title, body) = match params.get("networkApprovalContext").filter(|c| !c.is_null()) {
                    Some(context) => {
                        let host = str_at(context, "host").unwrap_or_default();
                        (format!("Allow network access to {host}"), host.to_owned())
                    }
                    None => ("Run command".to_owned(), str_at(params, "command").unwrap_or_default().to_owned()),
                };
                self.insert_decision(
                    (parent, call),
                    &id,
                    DecisionKind::Approval,
                    vec![(attr::TITLE, Value::from(title)), (attr::OPTIONS, approval_options(session_option))],
                    with_reason(body),
                    PendingKind::Command { session_option },
                )?;
                Ok(None)
            }
            "item/fileChange/requestApproval" => {
                let (parent, call) = self.decision_target(params)?;
                let diff = str_at(params, "itemId")
                    .and_then(|item| self.turn.as_ref()?.items.get(item))
                    .map(|i| i.diff.clone())
                    .unwrap_or_default();
                self.insert_decision(
                    (parent, call),
                    &id,
                    DecisionKind::Approval,
                    vec![(attr::TITLE, Value::from("Apply patch")), (attr::OPTIONS, approval_options(true))],
                    with_reason(diff),
                    PendingKind::FileChange,
                )?;
                Ok(None)
            }
            "item/permissions/requestApproval" => {
                let (parent, call) = self.decision_target(params)?;
                let requested = params.get("permissions").cloned().unwrap_or(Json::Null);
                self.insert_decision(
                    (parent, call),
                    &id,
                    DecisionKind::Approval,
                    vec![(attr::TITLE, Value::from("Grant permissions")), (attr::OPTIONS, approval_options(true))],
                    with_reason(requested.to_string()),
                    PendingKind::Permissions { requested },
                )?;
                Ok(None)
            }
            "item/tool/requestUserInput" => {
                let (parent, call) = self.decision_target(params)?;
                let questions = params.get("questions").and_then(Json::as_array).cloned().unwrap_or_default();
                if questions.is_empty() {
                    return Ok(Some(Reply::Result { id, result: json!({ "answers": {} }) }));
                }
                let group = rpc_key(&id);
                self.groups.insert(group.clone(), InputGroup { rpc_id: id.clone(), waiting: questions.len(), answers: Map::new() });
                for question in &questions {
                    let options: Vec<Value> = question
                        .get("options")
                        .and_then(Json::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|o| str_at(o, "label"))
                        .map(Value::from)
                        .collect();
                    let mut attrs = vec![(attr::OPTIONS, Value::List(options))];
                    if let Some(header) = str_at(question, "header") {
                        attrs.push((attr::TITLE, Value::from(header)));
                    }
                    self.insert_decision(
                        (parent, call),
                        &id,
                        DecisionKind::Question,
                        attrs,
                        str_at(question, "question").unwrap_or_default().to_owned(),
                        PendingKind::Question {
                            group: group.clone(),
                            question: str_at(question, "id").unwrap_or_default().to_owned(),
                        },
                    )?;
                }
                Ok(None)
            }
            "mcpServer/elicitation/request" => {
                let (parent, call) = self.decision_target(params)?;
                let server = str_at(params, "serverName").unwrap_or_default();
                let message = str_at(params, "message").unwrap_or_default();
                let form = matches!(str_at(params, "mode"), Some("form" | "openai/form"));
                if form {
                    let schema = params.get("requestedSchema").map(Json::to_string).unwrap_or_default();
                    self.insert_decision(
                        (parent, call),
                        &id,
                        DecisionKind::Form,
                        vec![(attr::TITLE, Value::from(server)), (attr::FIELDS, Value::from(schema))],
                        message.to_owned(),
                        PendingKind::Elicitation { form: true },
                    )?;
                } else {
                    let url = str_at(params, "url").unwrap_or_default();
                    self.insert_decision(
                        (parent, call),
                        &id,
                        DecisionKind::Approval,
                        vec![(attr::TITLE, Value::from(server)), (attr::OPTIONS, approval_options(false))],
                        format!("{message}\n\n{url}"),
                        PendingKind::Elicitation { form: false },
                    )?;
                }
                Ok(None)
            }
            "item/tool/call" => {
                let tool = str_at(params, "tool").unwrap_or_default().to_owned();
                let arguments = params.get("arguments").map(Json::to_string).unwrap_or_else(|| "{}".to_owned());
                Ok(Some(Reply::HostTool { id, tool, arguments }))
            }
            _ => Ok(Some(Reply::Error { id, code: METHOD_NOT_FOUND, message: format!("{method} isn't supported") })),
        }
    }

    /// Records `answer` on `decision` and returns the reply Codex expects.
    pub fn answer(&mut self, decision: NodeId, answer: &Answer) -> Result<Option<Reply>> {
        let pending = self.decisions.get(&decision).ok_or(AdapterError::UnknownDecision(decision))?;
        let first = answer.values.first().map(String::as_str);
        let unknown = || AdapterError::Protocol(format!("`{}` isn't an answer to this decision", first.unwrap_or_default()));
        let approval = |session_option: bool| -> Result<&'static str> {
            match first {
                Some(answer::ALLOW_ONCE) => Ok("accept"),
                Some(answer::ALLOW_SESSION) if session_option => Ok("acceptForSession"),
                Some(answer::DENY) => Ok("decline"),
                _ => Err(unknown()),
            }
        };
        let rpc_id = pending.rpc_id.clone();
        let reply = match &pending.kind {
            PendingKind::Command { session_option } => {
                Some(Reply::Result { id: rpc_id, result: json!({ "decision": approval(*session_option)? }) })
            }
            PendingKind::FileChange => Some(Reply::Result { id: rpc_id, result: json!({ "decision": approval(true)? }) }),
            PendingKind::Permissions { requested } => {
                let result = match approval(true)? {
                    "accept" => json!({ "permissions": requested, "scope": "turn" }),
                    "acceptForSession" => json!({ "permissions": requested, "scope": "session" }),
                    _ => json!({ "permissions": {} }),
                };
                Some(Reply::Result { id: rpc_id, result })
            }
            PendingKind::Elicitation { form: true } => {
                let result = match first {
                    Some(answer::DENY) | Some(answer::REJECT) => json!({ "action": "decline", "content": null }),
                    Some(text) => match serde_json::from_str::<Json>(text) {
                        Ok(content @ Json::Object(_)) => json!({ "action": "accept", "content": content }),
                        _ => return Err(AdapterError::Protocol("a form answer must be one JSON object".to_owned())),
                    },
                    None => return Err(unknown()),
                };
                Some(Reply::Result { id: rpc_id, result })
            }
            PendingKind::Elicitation { form: false } => {
                let action = if approval(false)? == "accept" { "accept" } else { "decline" };
                Some(Reply::Result { id: rpc_id, result: json!({ "action": action, "content": null }) })
            }
            PendingKind::Question { group, question } => {
                let Some(entry) = self.groups.get_mut(group) else { return Err(AdapterError::UnknownDecision(decision)) };
                entry.answers.insert(question.clone(), json!({ "answers": answer.values }));
                entry.waiting = entry.waiting.saturating_sub(1);
                if entry.waiting == 0 {
                    let entry = self.groups.remove(group).ok_or(AdapterError::UnknownDecision(decision))?;
                    Some(Reply::Result { id: entry.rpc_id, result: json!({ "answers": entry.answers }) })
                } else {
                    None
                }
            }
        };
        self.decisions.remove(&decision);
        let values: Vec<Value> = answer.values.iter().map(|v| Value::from(v.as_str())).collect();
        self.commit(Draft::set(decision, attr::ANSWER, Value::List(values)))?;
        self.commit(Draft::set(decision, attr::BY, answer.by.as_str()))?;
        if let Some(reason) = &answer.reason {
            self.commit(Draft::set(decision, attr::REASON, reason.as_str()))?;
        }
        self.commit(Draft::set(decision, attr::STATUS, DecisionStatus::Answered))?;
        self.commit(Draft::set(decision, attr::ENDED, Value::Int(now_ms())))?;
        self.unwait(decision)?;
        Ok(reply)
    }
}
