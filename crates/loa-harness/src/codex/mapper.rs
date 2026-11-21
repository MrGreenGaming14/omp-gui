//! Codex app-server notifications and server requests to DOM writes, dispatched by method name.

use super::{HARNESS, str_at};
use crate::kit::rpc::id_key;
use crate::kit::{EndEvent, TurnLedger};
use loa_contracts::harness::{AdapterError, Answer, Prompt, Result, SessionConfig, SessionState};
use loa_core::{
    CallStatus, DecisionKind, DecisionStatus, Dom, DomWriter, ElementKind as E, JobKind, JobStatus, MessageKind,
    MessageStatus, NodeId, Role, SCHEMA_VERSION, SessionMode, TodoState, TurnStatus, Value, answer, attr, now_ms,
};
use loa_core::schema::TEXT;
use serde_json::{Map, Value as Json, json};
use std::collections::HashMap;

type Attrs = Vec<(&'static str, Value)>;

/// What the session sends back to Codex.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Result { id: Json, result: Json },
    Error { id: Json, code: i64, message: String },
    /// `item/tool/call`, answered by the session through the host tool handler.
    HostTool { id: Json, tool: String, arguments: String },
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Kind {
    Message,
    Thinking,
    Call,
    User,
}

struct Item {
    node: NodeId,
    kind: Kind,
    /// Bytes appended through deltas.
    streamed: usize,
    /// `<result>` opened by streamed command output.
    result: Option<NodeId>,
    result_streamed: usize,
    /// Unified diff of a file change, the body of its approval decision.
    diff: String,
}

enum Pending {
    Command { session_option: bool },
    FileChange,
    Question { group: String, question: String },
    Form,
    Url,
    Permissions(Json),
}

struct InputGroup {
    rpc_id: Json,
    waiting: usize,
    answers: Map<String, Json>,
}

type Notification = fn(&mut Mapper, &Json, &DomWriter) -> Result<()>;
type Request = fn(&mut Mapper, Json, &Json, &DomWriter) -> Result<Option<Reply>>;

const NOTIFICATIONS: &[(&str, Notification)] = &[
    ("turn/started", |m, p, w| m.ensure_turn(p.pointer("/turn/id").and_then(Json::as_str), w).map(drop)),
    ("turn/completed", |m, p, w| m.turn_completed(p.get("turn").unwrap_or(&Json::Null), w)),
    ("item/started", |m, p, w| m.item(p, true, w)),
    ("item/completed", |m, p, w| m.item(p, false, w)),
    ("item/agentMessage/delta", |m, p, w| m.text_delta(p, Kind::Message, "agentMessage", w)),
    ("item/plan/delta", |m, p, w| m.text_delta(p, Kind::Message, "plan", w)),
    ("item/reasoning/summaryTextDelta", |m, p, w| m.text_delta(p, Kind::Thinking, "reasoning", w)),
    ("item/reasoning/textDelta", |m, p, w| m.text_delta(p, Kind::Thinking, "reasoning", w)),
    ("item/reasoning/summaryPartAdded", Mapper::summary_part),
    ("item/commandExecution/outputDelta", Mapper::output_delta),
    ("item/fileChange/outputDelta", Mapper::output_delta),
    ("turn/plan/updated", Mapper::plan_updated),
    ("thread/tokenUsage/updated", |m, p, _| { m.token_usage(p); Ok(()) }),
    ("model/rerouted", |m, p, w| str_at(p, "toModel").map_or(Ok(()), |model| m.set_model(model, w))),
    ("serverRequest/resolved", |m, p, w| p.get("requestId").map_or(Ok(()), |id| m.request_cleared(id, w))),
    ("error", Mapper::error),
];

const REQUESTS: &[(&str, Request)] = &[
    ("item/commandExecution/requestApproval", Mapper::command_approval),
    ("item/fileChange/requestApproval", |m, id, p, w| {
        let target = m.decision_target(p, w)?;
        let diff = str_at(p, "itemId").and_then(|i| m.items.get(i)).map(|i| i.diff.clone()).unwrap_or_default();
        m.approval(target, id, p, ("Apply patch".into(), diff), true, Pending::FileChange, w)
    }),
    ("item/permissions/requestApproval", |m, id, p, w| {
        let target = m.decision_target(p, w)?;
        let requested = p.get("permissions").cloned().unwrap_or(Json::Null);
        let body = ("Grant permissions".into(), requested.to_string());
        m.approval(target, id, p, body, true, Pending::Permissions(requested), w)
    }),
    ("item/tool/requestUserInput", Mapper::user_input),
    ("mcpServer/elicitation/request", Mapper::elicitation),
    ("item/tool/call", |_, id, p, _| {
        let tool = str_at(p, "tool").unwrap_or_default().to_owned();
        let arguments = p.get("arguments").map_or_else(|| "{}".to_owned(), Json::to_string);
        Ok(Some(Reply::HostTool { id, tool, arguments }))
    }),
];

/// Session DOM writer for one Codex thread.
pub struct Mapper {
    ledger: TurnLedger<String>,
    model: Option<String>,
    turns: Vec<NodeId>,
    /// Turns already covered by a `<compaction>`.
    covered: usize,
    /// Codex's id for the open turn.
    turn_id: Option<String>,
    /// The open turn holds its user message.
    prompted: bool,
    items: HashMap<String, Item>,
    /// Input, output, cache read and cache write tokens of the open turn.
    usage: Option<[i64; 4]>,
    todos: Vec<NodeId>,
    /// Subagent `<job>` nodes by agent thread id.
    jobs: HashMap<String, NodeId>,
    decisions: HashMap<NodeId, (Json, Pending)>,
    groups: HashMap<String, InputGroup>,
}

/// Summary sections then raw reasoning blocks, separated as `summaryPartAdded` separates them.
fn reasoning_text(item: &Json) -> String {
    let parts = ["summary", "content"].iter().filter_map(|k| item.get(*k)?.as_array()).flatten();
    parts.filter_map(Json::as_str).collect::<Vec<_>>().join("\n\n")
}

fn change_kind(change: &Json) -> &str {
    change.get("kind").and_then(|k| str_at(k, "type")).unwrap_or("update")
}

fn changes(item: &Json) -> impl Iterator<Item = &Json> {
    item.get("changes").and_then(Json::as_array).into_iter().flatten()
}

/// Text of MCP or dynamic tool content items; JSON when there is no text.
fn content_text(items: Option<&Json>) -> String {
    let Some(items) = items.and_then(Json::as_array) else { return String::new() };
    let texts: Vec<&str> = items.iter().filter_map(|i| str_at(i, "text")).collect();
    if texts.len() == items.len() { texts.join("\n") } else { Json::Array(items.clone()).to_string() }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().to_owned()
}

impl Mapper {
    /// Writes the `<session>` of a new Codex thread.
    pub fn start(config: &SessionConfig, thread_id: &str, model: Option<&str>, w: &DomWriter) -> Result<Self> {
        let model = model.map(str::to_owned).or_else(|| config.model.clone());
        let mut attrs: Attrs = vec![
            (attr::ID, config.session_id.as_str().into()),
            (attr::HARNESS, HARNESS.into()),
            (attr::NODE, config.node.as_str().into()),
            (attr::CREATED, now_ms().into()),
            (attr::SCHEMA, i64::from(SCHEMA_VERSION).into()),
            (attr::CWD, config.cwd.to_string_lossy().into_owned().into()),
            (attr::NATIVE_ID, thread_id.into()),
        ];
        let optional = [
            (attr::PROJECT, &config.project),
            (attr::MODEL, &model),
            (attr::MODEL_ROLE, &config.model_role),
            (attr::EFFORT, &config.effort),
        ];
        attrs.extend(optional.into_iter().filter_map(|(name, value)| Some((name, value.as_deref()?.into()))));
        attrs.extend(config.mode.map(|mode| (attr::MODE, mode.into())));
        let session = w.insert(NodeId::DOCUMENT, E::Session, attrs, "")?;
        Ok(Self::new(session, model))
    }

    fn new(session: NodeId, model: Option<String>) -> Self {
        Self {
            ledger: TurnLedger::new(session),
            model,
            turns: Vec::new(),
            covered: 0,
            turn_id: None,
            prompted: false,
            items: HashMap::new(),
            usage: None,
            todos: Vec::new(),
            jobs: HashMap::new(),
            decisions: HashMap::new(),
            groups: HashMap::new(),
        }
    }

    /// Continues a journaled session; what its dead process left running is cancelled.
    pub fn attach(dom: &Dom, w: &DomWriter) -> Result<Self> {
        let root = dom.root().filter(|r| r.kind == E::Session);
        let root = root.ok_or_else(|| AdapterError::Format("the DOM has no <session> root".to_owned()))?;
        let mut mapper = Self::new(root.id, root.str_attr(attr::MODEL).map(str::to_owned));
        for child in dom.children(root.id) {
            match child.kind {
                E::Turn => mapper.turns.push(child.id),
                E::Todo => mapper.todos.push(child.id),
                E::Compaction => {
                    if let Some(last) = child.attr(attr::COVERS).and_then(Value::as_list).and_then(<[_]>::last).and_then(Value::as_int) {
                        let covered = mapper.turns.iter().position(|t| Value::from(*t).as_int() == Some(last));
                        mapper.covered = covered.map_or(0, |p| p + 1);
                    }
                }
                _ => {}
            }
        }
        for node in dom.descendants(root.id) {
            let status = node.str_attr(attr::STATUS).unwrap_or_default();
            match node.kind {
                E::Message if status == "streaming" => w.set(node.id, attr::STATUS, "cancelled")?,
                E::Turn if matches!(status, "running" | "waiting") => w.close(node.id, "cancelled")?,
                E::Call if matches!(status, "running" | "pending") => w.close(node.id, "cancelled")?,
                E::Decision if status == "waiting" => w.close(node.id, "cancelled")?,
                E::Job if status == "running" => w.close(node.id, "cancelled")?,
                _ => {}
            }
        }
        Ok(mapper)
    }

    pub fn state(&self) -> SessionState {
        self.ledger.state()
    }

    /// Codex's id for the running turn, once known.
    pub fn turn_id(&self) -> Option<&str> {
        self.turn_id.as_deref()
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Opens a `<turn>` with the user message of the prompt the app is about to send.
    pub fn begin_turn(&mut self, prompt: &Prompt, w: &DomWriter) -> Result<()> {
        if self.ledger.turn().is_some() {
            return Err(AdapterError::Busy);
        }
        let node = self.ledger.begin(w, prompt)?;
        self.opened(node, None, true, w)
    }

    fn opened(&mut self, node: NodeId, id: Option<&str>, prompted: bool, w: &DomWriter) -> Result<()> {
        w.set(node, attr::STARTED, now_ms())?;
        self.turns.push(node);
        (self.turn_id, self.prompted, self.usage) = (None, prompted, None);
        self.items.clear();
        id.map_or(Ok(()), |id| self.turn_accepted(id, w))
    }

    /// Records Codex's id for the open turn.
    pub fn turn_accepted(&mut self, id: &str, w: &DomWriter) -> Result<()> {
        let Some(turn) = self.ledger.turn() else { return Ok(()) };
        if self.turn_id.is_none() {
            w.set(turn.node, attr::ID, id)?;
            self.turn_id = Some(id.to_owned());
        }
        Ok(())
    }

    /// `turn/start` failed: the turn ends with the error.
    pub fn turn_rejected(&mut self, message: &str, w: &DomWriter) -> Result<()> {
        if let Some(turn) = self.ledger.turn() {
            w.ext(turn.node, HARNESS, "error", message)?;
        }
        self.end_turn(TurnStatus::Error, w)
    }

    /// The process exited; a running turn ends with an error.
    pub fn process_exited(&mut self, detail: &str, w: &DomWriter) -> Result<()> {
        if let Some(turn) = self.ledger.turn() {
            w.ext(turn.node, HARNESS, "exit", detail)?;
            self.end_turn(TurnStatus::Error, w)?;
        }
        self.ledger.close();
        Ok(())
    }

    pub fn set_model(&mut self, model: &str, w: &DomWriter) -> Result<()> {
        self.model = Some(model.to_owned());
        Ok(w.set(self.ledger.session, attr::MODEL, model)?)
    }

    pub fn set_effort(&mut self, effort: &str, w: &DomWriter) -> Result<()> {
        Ok(w.set(self.ledger.session, attr::EFFORT, effort)?)
    }

    pub fn set_mode(&mut self, mode: SessionMode, w: &DomWriter) -> Result<()> {
        Ok(w.set(self.ledger.session, attr::MODE, mode)?)
    }

    /// Handles one notification (`id` is `None`) or server request; returns the reply to write now.
    pub fn handle(&mut self, id: Option<Json>, method: &str, params: &Json, w: &DomWriter) -> Result<Option<Reply>> {
        match id {
            Some(id) => match REQUESTS.iter().find(|(name, _)| *name == method) {
                Some((_, handler)) => handler(self, id, params, w),
                None => Ok(Some(Reply::Error { id, code: -32601, message: format!("{method} isn't supported") })),
            },
            None if self.state() == SessionState::Closed => Ok(None),
            None => match NOTIFICATIONS.iter().find(|(name, _)| *name == method) {
                Some((_, handler)) => handler(self, params, w).map(|()| None),
                None => {
                    // Unknown notifications of the open turn are kept; thread housekeeping isn't.
                    if let (Some(turn), Some(id)) = (self.ledger.turn(), str_at(params, "turnId"))
                        && self.turn_id.as_deref() == Some(id)
                    {
                        w.ext(turn.node, HARNESS, method, params)?;
                    }
                    Ok(None)
                }
            },
        }
    }

    /// The open turn, opening one Codex started on its own when `turn_id` isn't it.
    fn ensure_turn(&mut self, turn_id: Option<&str>, w: &DomWriter) -> Result<NodeId> {
        let open = match (self.ledger.turn(), turn_id) {
            (None, _) => true,
            (Some(_), Some(id)) => self.turn_id.as_deref().is_some_and(|current| current != id),
            (Some(_), None) => false,
        };
        if open {
            self.end_turn(TurnStatus::Done, w)?;
            let node = self.ledger.begin_with(w, None)?;
            self.opened(node, turn_id, false, w)?;
        } else if let Some(id) = turn_id {
            self.turn_accepted(id, w)?;
        }
        self.ledger.turn().map(|t| t.node).ok_or_else(|| AdapterError::Protocol("no turn".to_owned()))
    }

    fn error(&mut self, params: &Json, w: &DomWriter) -> Result<()> {
        if params.get("willRetry").and_then(Json::as_bool) != Some(true)
            && let Some(turn) = self.ledger.turn()
        {
            let message = params.get("error").and_then(|e| str_at(e, "message")).unwrap_or_default();
            w.ext(turn.node, HARNESS, "error", message)?;
        }
        Ok(())
    }

    /// The message node of item `id`, inserted when its start was missed.
    fn message_item(&mut self, turn_id: Option<&str>, id: &str, kind: Kind, item_type: &str, w: &DomWriter) -> Result<NodeId> {
        if let Some(item) = self.items.get(id) {
            return Ok(item.node);
        }
        let mut attrs: Attrs =
            vec![(attr::ROLE, Role::Assistant.into()), (attr::STATUS, MessageStatus::Streaming.into()), (attr::ID, id.into())];
        if kind == Kind::Thinking {
            attrs.push((attr::KIND, MessageKind::Thinking.into()));
        }
        attrs.extend(self.model.as_deref().map(|m| (attr::MODEL, m.into())));
        if item_type == "plan" {
            attrs.push(("x-codex-item", "plan".into()));
        }
        let parent = self.ensure_turn(turn_id, w)?;
        let node = w.insert(parent, E::Message, attrs, "")?;
        self.track(id, node, kind);
        Ok(node)
    }

    fn track(&mut self, id: &str, node: NodeId, kind: Kind) {
        if kind != Kind::User {
            self.ledger.open(id.to_owned(), node);
        }
        let item = Item { node, kind, streamed: 0, result: None, result_streamed: 0, diff: String::new() };
        self.items.insert(id.to_owned(), item);
    }

    fn text_delta(&mut self, params: &Json, kind: Kind, item_type: &str, w: &DomWriter) -> Result<()> {
        let (Some(id), Some(delta)) = (str_at(params, "itemId"), str_at(params, "delta")) else { return Ok(()) };
        if !delta.is_empty() {
            let node = self.message_item(str_at(params, "turnId"), id, kind, item_type, w)?;
            w.append(node, delta)?;
            if let Some(item) = self.items.get_mut(id) { item.streamed += delta.len(); }
        }
        Ok(())
    }

    fn summary_part(&mut self, params: &Json, w: &DomWriter) -> Result<()> {
        let Some(item) = str_at(params, "itemId").and_then(|id| self.items.get_mut(id)) else { return Ok(()) };
        if item.streamed > 0 {
            item.streamed += 2;
            w.append(item.node, "\n\n")?;
        }
        Ok(())
    }

    fn output_delta(&mut self, params: &Json, w: &DomWriter) -> Result<()> {
        let (Some(id), Some(delta)) = (str_at(params, "itemId"), str_at(params, "delta")) else { return Ok(()) };
        let Some(item) = self.items.get_mut(id) else { return Ok(()) };
        match item.result {
            Some(result) => w.append(result, delta)?,
            None => item.result = Some(w.insert(item.node, E::Result, Attrs::new(), delta)?),
        }
        item.result_streamed += delta.len();
        Ok(())
    }

    /// Inserts the `<call>` of a tool item; false for item types that aren't calls.
    fn start_call(&mut self, turn_id: Option<&str>, item: &Json, w: &DomWriter) -> Result<bool> {
        let s = |key| str_at(item, key).unwrap_or_default();
        let (tool, title, input) = match s("type") {
            "commandExecution" => ("exec_command".into(), Some(s("command").into()), json!({ "command": s("command"), "cwd": item.get("cwd") })),
            "fileChange" => {
                let paths: Vec<&str> = changes(item).filter_map(|c| str_at(c, "path")).collect();
                ("apply_patch".into(), Some(format!("Edit {}", paths.join(", "))), json!({ "changes": item.get("changes") }))
            }
            "mcpToolCall" => {
                let title = format!("{}: {}", s("server"), s("tool"));
                (format!("mcp__{}__{}", s("server"), s("tool")), Some(title), item.get("arguments").cloned().unwrap_or_default())
            }
            "dynamicToolCall" => {
                let tool = str_at(item, "namespace").map_or_else(|| s("tool").to_owned(), |ns| format!("{ns}.{}", s("tool")));
                (tool, None, item.get("arguments").cloned().unwrap_or_default())
            }
            "collabAgentToolCall" => {
                let snake = str_at(item, "tool").unwrap_or("agent").chars().fold(String::new(), |mut out, c| {
                    if c.is_ascii_uppercase() {
                        out.push('_');
                    }
                    out.push(c.to_ascii_lowercase());
                    out
                });
                let input = json!({ "prompt": item.get("prompt"), "model": item.get("model"), "receiverThreadIds": item.get("receiverThreadIds") });
                (snake, str_at(item, "prompt").map(first_line), input)
            }
            "webSearch" => ("web_search".into(), Some(s("query").into()), json!({ "query": s("query") })),
            "imageView" => ("view_image".into(), Some(s("path").into()), json!({ "path": s("path") })),
            _ => return Ok(false),
        };
        let Some(id) = str_at(item, "id") else { return Ok(true) };
        if self.items.contains_key(id) {
            return Ok(true);
        }
        let started = item.get("startedAtMs").and_then(Json::as_i64).unwrap_or_else(now_ms);
        let mut attrs: Attrs = vec![
            (attr::TOOL, tool.into()),
            (attr::CALL_ID, id.into()),
            (attr::STATUS, CallStatus::Running.into()),
            (attr::STARTED, started.into()),
        ];
        attrs.extend(title.map(|t| (attr::TITLE, t.into())));
        let parent = self.ensure_turn(turn_id, w)?;
        let node = w.insert(parent, E::Call, attrs, "")?;
        w.insert(node, E::Input, Attrs::new(), input.to_string())?;
        self.track(id, node, Kind::Call);
        if s("type") == "fileChange"
            && let Some(entry) = self.items.get_mut(id)
        {
            for change in changes(item) {
                entry.diff.push_str(&format!("{} {}\n", change_kind(change), str_at(change, "path").unwrap_or_default()));
                if let Some(diff) = str_at(change, "diff") {
                    entry.diff.push_str(diff);
                    if !diff.ends_with('\n') {
                        entry.diff.push('\n');
                    }
                }
            }
        }
        Ok(true)
    }

    /// `item/started` (`started`) or `item/completed`.
    fn item(&mut self, params: &Json, started: bool, w: &DomWriter) -> Result<()> {
        let (turn_id, Some(item)) = (str_at(params, "turnId"), params.get("item")) else { return Ok(()) };
        let Some(id) = str_at(item, "id") else { return Ok(()) };
        let ty = str_at(item, "type").unwrap_or_default();
        let (kind, text) = match ty {
            "userMessage" => return self.user_message(turn_id, id, item, w),
            "agentMessage" | "plan" => (Kind::Message, str_at(item, "text").unwrap_or_default().to_owned()),
            "reasoning" => (Kind::Thinking, reasoning_text(item)),
            "contextCompaction" if !started => {
                let covers: Vec<Value> = self.turns[self.covered..].iter().map(|t| Value::from(*t)).collect();
                self.covered = self.turns.len();
                let attrs: Attrs = vec![(attr::COVERS, Value::List(covers)), (attr::CREATED, now_ms().into())];
                return Ok(w.insert(self.ledger.session, E::Compaction, attrs, "").map(drop)?);
            }
            _ if started => return self.start_call(turn_id, item, w).map(drop),
            _ => {
                if !self.items.contains_key(id) && !self.start_call(turn_id, item, w)? {
                    let parent = self.ensure_turn(turn_id, w)?;
                    w.ext(parent, HARNESS, &format!("item:{ty}"), item)?;
                    return Ok(());
                }
                return self.finish_call(id, item, w);
            }
        };
        let node = self.message_item(turn_id, id, kind, ty, w)?;
        let streamed = self.items.get(id).map_or(0, |i| i.streamed);
        if started {
            // Text an item carries when it starts counts as streamed.
            if !text.is_empty() && streamed == 0 {
                w.append(node, text.as_str())?;
                if let Some(item) = self.items.get_mut(id) { item.streamed = text.len(); }
            }
            return Ok(());
        }
        if streamed != text.len() {
            w.set(node, TEXT, text)?;
        }
        w.set(node, attr::STATUS, MessageStatus::Done)?;
        self.items.remove(id);
        self.ledger.take(&id.to_owned());
        Ok(())
    }

    fn user_message(&mut self, turn_id: Option<&str>, id: &str, item: &Json, w: &DomWriter) -> Result<()> {
        let turn = self.ensure_turn(turn_id, w)?;
        if self.items.contains_key(id) {
            return Ok(());
        }
        if self.prompted {
            // The prompt the app sent; its message is in the DOM already.
            self.track(id, turn, Kind::User);
            return Ok(());
        }
        self.prompted = true;
        let content = item.get("content").and_then(Json::as_array).into_iter().flatten();
        let text = content.filter_map(|i| str_at(i, "text")).collect::<Vec<_>>().join("\n");
        let attrs = [(attr::ROLE, Role::User.as_str()), (attr::STATUS, MessageStatus::Done.as_str()), (attr::ID, id)];
        let node = w.insert(turn, E::Message, attrs, text)?;
        self.track(id, node, Kind::User);
        Ok(())
    }

    fn finish_call(&mut self, id: &str, item: &Json, w: &DomWriter) -> Result<()> {
        let Some(entry) = self.items.remove(id) else { return Ok(()) };
        self.ledger.take(&id.to_owned());
        let exit_code = item.get("exitCode").and_then(Json::as_i64).filter(|_| str_at(item, "type") == Some("commandExecution"));
        let (output, status) = match str_at(item, "type").unwrap_or_default() {
            "commandExecution" => (str_at(item, "aggregatedOutput").map(str::to_owned), CallStatus::Done),
            "fileChange" => {
                let summary: Vec<String> =
                    changes(item).map(|c| format!("{} {}", change_kind(c), str_at(c, "path").unwrap_or_default())).collect();
                (Some(summary.join("\n")), CallStatus::Done)
            }
            "mcpToolCall" => match item.get("error").and_then(|e| str_at(e, "message")) {
                Some(message) => (Some(message.to_owned()), CallStatus::Error),
                None => (Some(content_text(item.pointer("/result/content"))), CallStatus::Done),
            },
            "dynamicToolCall" => {
                let failed = item.get("success").and_then(Json::as_bool) == Some(false);
                (Some(content_text(item.get("contentItems"))), if failed { CallStatus::Error } else { CallStatus::Done })
            }
            "collabAgentToolCall" => {
                self.agent_jobs(entry.node, item, w)?;
                (Some(item.get("agentsStates").map(Json::to_string).unwrap_or_default()), CallStatus::Done)
            }
            _ => (None, CallStatus::Done),
        };
        let status = match str_at(item, "status") {
            Some("failed") => CallStatus::Error,
            Some("declined" | "interrupted") => CallStatus::Cancelled,
            _ if exit_code.is_some_and(|c| c != 0) => CallStatus::Error,
            _ => status,
        };
        let is_error = status == CallStatus::Error;
        let result = match entry.result {
            Some(result) => {
                if let Some(output) = output.filter(|o| o.len() != entry.result_streamed) {
                    w.set(result, TEXT, output)?;
                }
                w.set(result, attr::IS_ERROR, is_error)?;
                result
            }
            None => w.insert(entry.node, E::Result, [(attr::IS_ERROR, is_error)], output.unwrap_or_default())?,
        };
        if let Some(code) = exit_code {
            w.set(result, "x-exit-code", code)?;
        }
        if let Some(duration) = item.get("durationMs").and_then(Json::as_i64) {
            w.insert(entry.node, E::Usage, [(attr::DURATION_MS, duration)], "")?;
        }
        w.set(entry.node, attr::STATUS, status)?;
        Ok(w.set(entry.node, attr::ENDED, item.get("completedAtMs").and_then(Json::as_i64).unwrap_or_else(now_ms))?)
    }

    /// A `<job kind="subagent">` per agent thread a spawn reaches; later collab calls update its status.
    fn agent_jobs(&mut self, call: NodeId, item: &Json, w: &DomWriter) -> Result<()> {
        let Some(parent) = self.ledger.turn().map(|t| t.node) else { return Ok(()) };
        let spawn = str_at(item, "tool") == Some("spawnAgent");
        for thread in item.get("receiverThreadIds").and_then(Json::as_array).into_iter().flatten().filter_map(Json::as_str) {
            let state = item.get("agentsStates").and_then(|s| s.get(thread)).and_then(|s| str_at(s, "status"));
            let status = match state {
                Some("completed" | "shutdown") => JobStatus::Done,
                Some("errored" | "notFound") => JobStatus::Error,
                Some("interrupted") => JobStatus::Cancelled,
                _ => JobStatus::Running,
            };
            match self.jobs.get(thread).copied() {
                Some(job) if state.is_some() && status != JobStatus::Running => w.close(job, status)?,
                Some(job) if state.is_some() => w.set(job, attr::STATUS, status)?,
                None if spawn => {
                    let mut attrs: Attrs = vec![
                        (attr::KIND, JobKind::Subagent.into()),
                        (attr::STATUS, status.into()),
                        (attr::ID, thread.into()),
                        (attr::AGENT, thread.into()),
                        (attr::CALL, call.into()),
                        (attr::STARTED, now_ms().into()),
                    ];
                    attrs.extend(str_at(item, "prompt").map(|p| (attr::TITLE, first_line(p).into())));
                    self.jobs.insert(thread.to_owned(), w.insert(parent, E::Job, attrs, "")?);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn plan_updated(&mut self, params: &Json, w: &DomWriter) -> Result<()> {
        let steps = params.get("plan").and_then(Json::as_array).map_or(&[][..], Vec::as_slice);
        for (index, step) in steps.iter().enumerate() {
            let state = match str_at(step, "status") {
                Some("completed") => TodoState::Done,
                Some("inProgress") => TodoState::InProgress,
                _ => TodoState::Pending,
            };
            let text = str_at(step, "step").unwrap_or_default();
            match self.todos.get(index).copied() {
                Some(todo) => {
                    w.set(todo, attr::STATE, state)?;
                    w.set(todo, TEXT, text)?;
                }
                None => self.todos.push(w.insert(self.ledger.session, E::Todo, [(attr::STATE, state)], text)?),
            }
        }
        for todo in self.todos.drain(steps.len()..) {
            w.set(todo, attr::STATE, TodoState::Cancelled)?;
        }
        Ok(())
    }

    fn token_usage(&mut self, params: &Json) {
        let (Some(last), Some(_)) = (params.pointer("/tokenUsage/last"), self.ledger.turn()) else { return };
        let usage = self.usage.get_or_insert([0; 4]);
        for (total, key) in usage.iter_mut().zip(["inputTokens", "outputTokens", "cachedInputTokens", "cacheWriteInputTokens"]) {
            *total += last.get(key).and_then(Json::as_i64).unwrap_or(0);
        }
    }

    fn turn_completed(&mut self, turn: &Json, w: &DomWriter) -> Result<()> {
        let status = match str_at(turn, "status") {
            Some("completed") => TurnStatus::Done,
            Some("interrupted") => TurnStatus::Cancelled,
            Some("failed") => TurnStatus::Error,
            _ => return Ok(()),
        };
        if let (Some(id), Some(current)) = (str_at(turn, "id"), &self.turn_id)
            && self.ledger.turn().is_some()
            && current != id
        {
            // A turn Codex never announced to this session.
            return Ok(());
        }
        if let (Some(open), Some(message)) = (self.ledger.turn(), turn.get("error").and_then(|e| str_at(e, "message"))) {
            w.ext(open.node, HARNESS, "error", message)?;
        }
        self.end_turn(status, w)
    }

    /// Ends the open turn: open messages, calls and decisions end with it, usage is recorded.
    fn end_turn(&mut self, status: TurnStatus, w: &DomWriter) -> Result<()> {
        let Some(turn) = self.ledger.turn().map(|t| t.node) else { return Ok(()) };
        if let Some([input, output, read, write]) = self.usage.take() {
            let tokens = [
                (attr::INPUT_TOKENS, input),
                (attr::OUTPUT_TOKENS, output),
                (attr::CACHE_READ_TOKENS, read),
                (attr::CACHE_WRITE_TOKENS, write),
            ];
            w.insert(turn, E::Usage, tokens, "")?;
        }
        let (mut children, mut decisions) = (Vec::new(), Vec::new());
        self.ledger.end_turn(w, status, |event| match event {
            EndEvent::Child { key, node } => children.push((key.clone(), node)),
            EndEvent::Decision(decision) => decisions.push(decision),
            EndEvent::Turn { .. } => {}
        })?;
        let now = now_ms();
        for (key, node) in children {
            match self.items.get(&key).map(|i| i.kind) {
                Some(Kind::Call) => w.set(node, attr::ENDED, now)?,
                _ if status == TurnStatus::Done => w.set(node, attr::STATUS, MessageStatus::Done)?,
                _ => {}
            }
        }
        decisions.into_iter().for_each(|d| self.forget(d));
        (self.turn_id, self.prompted) = (None, false);
        self.items.clear();
        Ok(())
    }

    fn forget(&mut self, decision: NodeId) {
        if let Some((_, Pending::Question { group, .. })) = self.decisions.remove(&decision) {
            self.groups.remove(&group);
        }
    }

    /// Codex withdrew a request (turn ended, interrupted).
    fn request_cleared(&mut self, rpc_id: &Json, w: &DomWriter) -> Result<()> {
        let cleared: Vec<NodeId> = self.decisions.iter().filter(|(_, (id, _))| id == rpc_id).map(|(node, _)| *node).collect();
        for decision in cleared {
            self.forget(decision);
            w.close(decision, DecisionStatus::Cancelled)?;
            self.unwait(decision, w)?;
        }
        Ok(())
    }

    /// Takes `decision` off the waiting list; the turn runs again when none are left.
    fn unwait(&mut self, decision: NodeId, w: &DomWriter) -> Result<()> {
        let Some(before) = self.ledger.turn().map(|t| t.waiting.len()) else { return Ok(()) };
        self.ledger.answered(decision);
        match self.ledger.turn() {
            Some(turn) if before > 0 && turn.waiting.is_empty() => Ok(w.set(turn.node, attr::STATUS, TurnStatus::Running)?),
            _ => Ok(()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn decide(
        &mut self,
        (parent, call): (NodeId, Option<NodeId>),
        rpc_id: &Json,
        kind: DecisionKind,
        mut attrs: Attrs,
        text: String,
        pending: Pending,
        w: &DomWriter,
    ) -> Result<()> {
        attrs.extend([
            (attr::KIND, kind.into()),
            (attr::STATUS, DecisionStatus::Waiting.into()),
            (attr::REQUEST_ID, id_key(rpc_id).into()),
            (attr::CREATED, now_ms().into()),
        ]);
        attrs.extend(call.map(|c| (attr::CALL, c.into())));
        let decision = w.insert(parent, E::Decision, attrs, text)?;
        self.decisions.insert(decision, (rpc_id.clone(), pending));
        if let Some(turn) = self.ledger.turn() {
            if turn.waiting.is_empty() {
                w.set(turn.node, attr::STATUS, TurnStatus::Waiting)?;
            }
            self.ledger.wait(decision);
        }
        Ok(())
    }

    /// The `<call>` of item `itemId` and the node a decision about it sits under.
    fn decision_target(&mut self, params: &Json, w: &DomWriter) -> Result<(NodeId, Option<NodeId>)> {
        if let Some(call) = str_at(params, "itemId").and_then(|id| self.items.get(id)) {
            return Ok((call.node, Some(call.node)));
        }
        let parent = match (self.ledger.turn(), str_at(params, "turnId")) {
            (Some(turn), _) => turn.node,
            (None, Some(id)) => self.ensure_turn(Some(id), w)?,
            (None, None) => self.ledger.session,
        };
        Ok((parent, None))
    }

    fn options(session: bool) -> Value {
        let options = [Some(answer::ALLOW_ONCE), session.then_some(answer::ALLOW_SESSION), Some(answer::DENY)];
        Value::List(options.into_iter().flatten().map(Value::from).collect())
    }

    /// An approval decision; `body` gets the request's `reason` appended.
    #[allow(clippy::too_many_arguments)]
    fn approval(
        &mut self,
        target: (NodeId, Option<NodeId>),
        id: Json,
        params: &Json,
        (title, mut body): (String, String),
        session: bool,
        pending: Pending,
        w: &DomWriter,
    ) -> Result<Option<Reply>> {
        if let Some(reason) = str_at(params, "reason") {
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str(reason);
        }
        let attrs = vec![(attr::TITLE, title.into()), (attr::OPTIONS, Self::options(session))];
        self.decide(target, &id, DecisionKind::Approval, attrs, body, pending, w)?;
        Ok(None)
    }

    fn command_approval(&mut self, id: Json, params: &Json, w: &DomWriter) -> Result<Option<Reply>> {
        let target = self.decision_target(params, w)?;
        let session_option = str_at(params, "kind") != Some("humanCommand");
        let body = match params.get("networkApprovalContext").filter(|c| !c.is_null()) {
            Some(context) => {
                let host = str_at(context, "host").unwrap_or_default();
                (format!("Allow network access to {host}"), host.to_owned())
            }
            None => ("Run command".to_owned(), str_at(params, "command").unwrap_or_default().to_owned()),
        };
        self.approval(target, id, params, body, session_option, Pending::Command { session_option }, w)
    }

    fn user_input(&mut self, id: Json, params: &Json, w: &DomWriter) -> Result<Option<Reply>> {
        let target = self.decision_target(params, w)?;
        let questions = params.get("questions").and_then(Json::as_array).map_or(&[][..], Vec::as_slice);
        if questions.is_empty() {
            return Ok(Some(Reply::Result { id, result: json!({ "answers": {} }) }));
        }
        let group = id_key(&id);
        self.groups.insert(group.clone(), InputGroup { rpc_id: id.clone(), waiting: questions.len(), answers: Map::new() });
        for question in questions {
            let options = question.get("options").and_then(Json::as_array).into_iter().flatten();
            let mut attrs: Attrs = vec![(attr::OPTIONS, Value::List(options.filter_map(|o| str_at(o, "label")).map(Value::from).collect()))];
            attrs.extend(str_at(question, "header").map(|h| (attr::TITLE, h.into())));
            let text = str_at(question, "question").unwrap_or_default().to_owned();
            let pending = Pending::Question { group: group.clone(), question: str_at(question, "id").unwrap_or_default().to_owned() };
            self.decide(target, &id, DecisionKind::Question, attrs, text, pending, w)?;
        }
        Ok(None)
    }

    fn elicitation(&mut self, id: Json, params: &Json, w: &DomWriter) -> Result<Option<Reply>> {
        let target = self.decision_target(params, w)?;
        let (server, message) = (str_at(params, "serverName").unwrap_or_default(), str_at(params, "message").unwrap_or_default());
        if matches!(str_at(params, "mode"), Some("form" | "openai/form")) {
            let schema = params.get("requestedSchema").map(Json::to_string).unwrap_or_default();
            let attrs = vec![(attr::TITLE, server.into()), (attr::FIELDS, schema.into())];
            self.decide(target, &id, DecisionKind::Form, attrs, message.to_owned(), Pending::Form, w)?;
            return Ok(None);
        }
        let body = format!("{message}\n\n{}", str_at(params, "url").unwrap_or_default());
        let attrs = vec![(attr::TITLE, server.into()), (attr::OPTIONS, Self::options(false))];
        self.decide(target, &id, DecisionKind::Approval, attrs, body, Pending::Url, w)?;
        Ok(None)
    }

    /// Records `answer` on `decision` and returns the reply Codex expects.
    pub fn answer(&mut self, decision: NodeId, answer: &Answer, w: &DomWriter) -> Result<Option<Reply>> {
        let (_, pending) = self.decisions.get(&decision).ok_or(AdapterError::UnknownDecision(decision))?;
        let first = answer.values.first().map(String::as_str);
        let unknown = || AdapterError::Protocol(format!("`{}` isn't an answer to this decision", first.unwrap_or_default()));
        let approval = |session: bool| match first {
            Some(answer::ALLOW_ONCE) => Ok("accept"),
            Some(answer::ALLOW_SESSION) if session => Ok("acceptForSession"),
            Some(answer::DENY) => Ok("decline"),
            _ => Err(unknown()),
        };
        let result = match pending {
            Pending::Command { session_option } => json!({ "decision": approval(*session_option)? }),
            Pending::FileChange => json!({ "decision": approval(true)? }),
            Pending::Permissions(requested) => match approval(true)? {
                "accept" => json!({ "permissions": requested, "scope": "turn" }),
                "acceptForSession" => json!({ "permissions": requested, "scope": "session" }),
                _ => json!({ "permissions": {} }),
            },
            Pending::Form => match first {
                Some(answer::DENY | answer::REJECT) => json!({ "action": "decline", "content": null }),
                Some(text) => match serde_json::from_str::<Json>(text) {
                    Ok(content @ Json::Object(_)) => json!({ "action": "accept", "content": content }),
                    _ => return Err(AdapterError::Protocol("a form answer must be one JSON object".to_owned())),
                },
                None => return Err(unknown()),
            },
            Pending::Url => json!({ "action": if approval(false)? == "accept" { "accept" } else { "decline" }, "content": null }),
            Pending::Question { group, question } => {
                let entry = self.groups.get_mut(group).ok_or(AdapterError::UnknownDecision(decision))?;
                entry.answers.insert(question.clone(), json!({ "answers": answer.values }));
                entry.waiting = entry.waiting.saturating_sub(1);
                Json::Null
            }
        };
        let reply = match self.decisions.remove(&decision) {
            Some((_, Pending::Question { group, .. })) => match self.groups.get(&group) {
                Some(entry) if entry.waiting == 0 => {
                    let entry = self.groups.remove(&group).ok_or(AdapterError::UnknownDecision(decision))?;
                    Some(Reply::Result { id: entry.rpc_id, result: json!({ "answers": entry.answers }) })
                }
                _ => None,
            },
            Some((id, _)) => Some(Reply::Result { id, result }),
            None => None,
        };
        w.set(decision, attr::ANSWER, answer.values.clone())?;
        w.set(decision, attr::BY, answer.by.as_str())?;
        if let Some(reason) = &answer.reason {
            w.set(decision, attr::REASON, reason.as_str())?;
        }
        w.close(decision, DecisionStatus::Answered)?;
        self.unwait(decision, w)?;
        Ok(reply)
    }
}
