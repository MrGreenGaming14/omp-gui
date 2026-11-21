//! ACP traffic as session DOM writes: chunks become messages, tool calls
//! `<call>`s, plans `<todo>`s, permission requests approval `<decision>`s;
//! anything else is kept as `<ext harness="acp">`.

use super::HARNESS;
use super::wire::{CurrentModeUpdate, PermissionOption, PermissionOptionKind, Plan, PlanEntryStatus, PromptResponse, RequestPermissionRequest, SessionInfoUpdate, StopReason, ToolCallFields, ToolCallStatus};
use crate::kit::{EventMapper, TurnLedger};
use loa_contracts::harness::{AdapterError, Answer, Prompt, Result};
use loa_core::schema::TEXT;
use loa_core::{CallStatus, CommitError, DecisionKind, DecisionStatus, Dom, DomWriter, ElementKind as E, MessageKind, MessageStatus, NodeId, Role, SessionMode, TodoState, TurnStatus, Value, answer, attr};
use serde::{Deserialize, Deserializer};
use serde_json::{Value as Json, json};
use std::collections::HashMap;

/// Session attribute holding the agent's own mode id.
pub const X_MODE: &str = "x-acp-mode";
/// Session attribute naming the agent program (`agentInfo`).
pub const X_AGENT: &str = "x-acp-agent";
/// Decision attributes keeping the offered option ids and kinds, parallel to `options`.
pub const X_OPTION_IDS: &str = "x-acp-option-ids";
pub const X_OPTION_KINDS: &str = "x-acp-option-kinds";
/// Decision attribute holding the option id that was sent back.
pub const X_OPTION: &str = "x-acp-option";
/// Turn attribute holding a stop reason other than `end_turn`.
pub const X_STOP_REASON: &str = "x-acp-stop-reason";
/// Todo attribute holding the plan entry priority.
pub const X_PRIORITY: &str = "x-acp-priority";

type Out<T = ()> = std::result::Result<T, CommitError>;

/// What the session feeds the [`AcpMapper`].
#[derive(Debug)]
pub enum Input {
    /// The user's prompt, sent as `session/prompt` request `id`; opens the turn.
    Prompt { id: String, prompt: Prompt },
    /// The agent's response to request `id`; `Err` holds the failure.
    Response { id: String, result: std::result::Result<Json, String> },
    /// `session/load` request `id` replays history the DOM already has.
    Load { id: String },
    Interrupt,
    /// The agent is gone; `shutdown` when the app closed it.
    Closed { shutdown: bool },
    /// The params of a `session/update`.
    Update(Json),
    /// A `session/request_permission` request.
    Permission { id: Json, params: Json },
    /// The answer to the permission request `request`.
    Answer { request: Json, answer: Answer },
}

/// Fixture lines: `{"$":"send"|"result"|"interrupt"|"closed"|"load"|"loaded"|"answer",…}`
/// or a JSON-RPC message from the agent. The prompt is request `1`, the load `99`.
impl<'de> Deserialize<'de> for Input {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let mut raw = Json::deserialize(d)?;
        let text = |raw: &Json, key: &str| raw[key].as_str().map(str::to_owned);
        Ok(match raw["$"].as_str() {
            Some("send") => Input::Prompt { id: "1".into(), prompt: Prompt { text: text(&raw, "text").unwrap_or_default(), by: "owner".into(), ..Prompt::default() } },
            Some("result") => Input::Response { id: "1".into(), result: text(&raw, "error").map_or_else(|| Ok(raw["result"].take()), Err) },
            Some("interrupt") => Input::Interrupt,
            Some("closed") => Input::Closed { shutdown: false },
            Some("load") => Input::Load { id: "99".into() },
            Some("loaded") => Input::Response { id: "99".into(), result: Ok(Json::Null) },
            Some("answer") => {
                let values = raw["values"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect();
                let answer = Answer { values, reason: text(&raw, "reason"), by: text(&raw, "by").unwrap_or_else(|| "owner".into()) };
                Input::Answer { request: raw["request"].take(), answer }
            }
            _ if raw["method"] == "session/update" => Input::Update(raw["params"].take()),
            _ => Input::Permission { id: raw["id"].take(), params: raw["params"].take() },
        })
    }
}

/// What the mapper has the session send to the agent.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    /// The result of the agent's request `id`.
    Respond { id: Json, result: Json },
    /// A JSON-RPC "invalid params" error for the agent's request `id`.
    Reject { id: Json, message: String },
    /// `session/cancel`.
    Cancel,
}

impl Reply {
    fn cancelled(id: Json) -> Self {
        Reply::Respond { id, result: json!({ "outcome": { "outcome": "cancelled" } }) }
    }
}

/// How a turn ended.
enum TurnEnd {
    Stop(StopReason),
    /// `session/prompt` failed with this message.
    Failed(String),
    Closed { shutdown: bool },
}

struct Pending {
    rpc_id: Json,
    options: Vec<PermissionOption>,
}

#[derive(Default)]
struct Turn {
    message: Option<(NodeId, MessageKind, Option<String>)>,
    calls: HashMap<String, Call>,
    /// The `session/prompt` request whose response ends the turn.
    prompt: Option<String>,
    interrupted: bool,
}

struct Call {
    node: NodeId,
    input: Option<NodeId>,
    status: CallStatus,
    content: Vec<Json>,
    raw_output: Option<Json>,
}

struct Todo {
    node: NodeId,
    text: String,
    state: TodoState,
    priority: Option<String>,
}

/// The DOM side of one ACP session.
pub struct AcpMapper {
    ledger: TurnLedger<String>,
    turn: Turn,
    /// Assistant messages are attributed to it.
    model: Option<String>,
    /// The agent's session id; updates for other sessions are ignored.
    native: Option<String>,
    mode_id: Option<String>,
    todos: Vec<Todo>,
    decisions: HashMap<NodeId, Pending>,
    /// A running `session/load`, whose replayed history is skipped.
    replay: Option<String>,
}

impl EventMapper for AcpMapper {
    type Event = Input;
    type Reply = Reply;

    fn apply(&mut self, input: Input, w: &DomWriter) -> Result<Vec<Reply>> {
        match input {
            Input::Prompt { id, prompt } => {
                let turn = self.ledger.begin(w, &prompt)?;
                w.set(turn, attr::STARTED, w.now())?;
                self.turn = Turn { prompt: Some(id), ..Turn::default() };
            }
            Input::Response { id, result } => {
                if self.replay.as_ref() == Some(&id) {
                    self.replay = None;
                }
                if self.turn.prompt.as_ref() == Some(&id) && self.ledger.turn().is_some() {
                    let end = match result.map(serde_json::from_value::<PromptResponse>) {
                        Ok(Ok(response)) => TurnEnd::Stop(response.stop_reason),
                        Ok(Err(e)) => TurnEnd::Failed(format!("malformed session/prompt response: {e}")),
                        Err(message) => TurnEnd::Failed(message),
                    };
                    return Ok(self.end_turn(w, end)?);
                }
            }
            Input::Load { id } => self.replay = Some(id),
            Input::Interrupt if self.ledger.turn().is_some() => {
                self.turn.interrupted = true;
                let mut replies = self.cancel_decisions(w)?;
                replies.push(Reply::Cancel);
                return Ok(replies);
            }
            Input::Interrupt => {}
            Input::Closed { shutdown } => {
                let replies = self.end_turn(w, TurnEnd::Closed { shutdown })?;
                self.ledger.close();
                return Ok(replies);
            }
            Input::Update(params) => {
                let ours = match (&self.native, params["sessionId"].as_str()) {
                    (Some(ours), Some(theirs)) => ours == theirs,
                    _ => true,
                };
                if ours && self.replay.is_none() && params.get("update").is_some() {
                    self.update(w, params["update"].clone())?;
                }
            }
            Input::Permission { id, params } => match serde_json::from_value(params) {
                Ok(request) => self.permission(w, id, request)?,
                Err(e) => return Ok(vec![Reply::Reject { id, message: e.to_string() }]),
            },
            Input::Answer { request, answer } => {
                let node = self.decisions.iter().find(|(_, d)| d.rpc_id == request).map(|(node, _)| *node);
                let node = node.ok_or_else(|| AdapterError::Protocol(format!("no permission request {request}")))?;
                return Ok(self.answer(node, &answer, w)?.into_iter().collect());
            }
        }
        Ok(Vec::new())
    }

    fn answer(&mut self, decision: NodeId, answer: &Answer, w: &DomWriter) -> Result<Option<Reply>> {
        let pending = self.decisions.get(&decision).ok_or(AdapterError::UnknownDecision(decision))?;
        let value = answer.values.first().ok_or_else(|| AdapterError::Protocol("the answer is empty".to_owned()))?;
        let option = pick_option(&pending.options, value).ok_or_else(|| AdapterError::Protocol(format!("`{value}` matches none of the offered options")))?.clone();
        let Pending { rpc_id, .. } = self.decisions.remove(&decision).ok_or(AdapterError::UnknownDecision(decision))?;
        let canonical = match option.kind {
            PermissionOptionKind::AllowOnce => answer::ALLOW_ONCE,
            PermissionOptionKind::AllowAlways => answer::ALLOW_SESSION,
            PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways => answer::DENY,
        };
        w.set(decision, attr::ANSWER, Value::List(vec![canonical.into()]))?;
        w.set(decision, X_OPTION, option.option_id.as_str())?;
        if !answer.by.is_empty() {
            w.set(decision, attr::BY, answer.by.as_str())?;
        }
        answer.reason.iter().try_for_each(|reason| w.set(decision, attr::REASON, reason.as_str()))?;
        w.close(decision, DecisionStatus::Answered)?;
        self.ledger.answered(decision);
        if let Some(turn) = self.ledger.turn().filter(|_| self.decisions.is_empty()) {
            w.set(turn.node, attr::STATUS, TurnStatus::Running)?;
        }
        Ok(Some(Reply::Respond { id: rpc_id, result: json!({ "outcome": { "optionId": option.option_id, "outcome": "selected" } }) }))
    }

    fn ledger(&self) -> &TurnLedger<String> {
        &self.ledger
    }

    fn ledger_mut(&mut self) -> &mut TurnLedger<String> {
        &mut self.ledger
    }
}

impl AcpMapper {
    /// Writes under the `<session>` `root`; assistant messages carry `model`.
    pub fn new(root: NodeId, model: Option<String>) -> Self {
        let (ledger, turn, decisions) = (TurnLedger::new(root), Turn::default(), HashMap::new());
        Self { ledger, turn, model, native: None, mode_id: None, todos: Vec::new(), decisions, replay: None }
    }

    /// Takes over the todos and mode a resumed session already has.
    pub fn adopt(&mut self, dom: &Dom) {
        let root = self.root();
        self.todos = dom.children_of_kind(root, E::Todo).map(|todo| Todo {
            node: todo.id,
            text: todo.text.clone(),
            state: todo.str_attr(attr::STATE).and_then(TodoState::parse).unwrap_or(TodoState::Pending),
            priority: todo.str_attr(X_PRIORITY).map(str::to_owned),
        }).collect();
        self.mode_id = dom.get(root).and_then(|r| r.str_attr(X_MODE)).map(str::to_owned);
    }

    pub fn root(&self) -> NodeId {
        self.ledger.session
    }

    /// The agent's session id is `id`.
    pub fn set_native(&mut self, w: &DomWriter, id: &str) -> Out {
        self.native = Some(id.to_owned());
        w.set(self.root(), attr::NATIVE_ID, id)
    }

    /// Tests replay one agent session; this names it.
    pub fn adopt_native(&mut self, id: &str) {
        self.native = Some(id.to_owned());
    }

    /// The agent switched to `model`: later assistant messages carry it.
    pub fn model_changed(&mut self, w: &DomWriter, model: &str) -> Out {
        if self.model.as_deref() == Some(model) {
            return Ok(());
        }
        self.model = Some(model.to_owned());
        w.set(self.root(), attr::MODEL, model)
    }

    pub fn mode_id(&self) -> Option<&str> {
        self.mode_id.as_deref()
    }

    /// Records the agent's current mode on the session.
    pub fn mode_changed(&mut self, w: &DomWriter, mode_id: &str) -> Out {
        if self.mode_id.as_deref() == Some(mode_id) {
            return Ok(());
        }
        self.mode_id = Some(mode_id.to_owned());
        w.set(self.root(), X_MODE, mode_id)?;
        w.set(self.root(), attr::MODE, if mode_id == "plan" { SessionMode::Plan } else { SessionMode::Normal })
    }

    fn turn_node(&self) -> Option<NodeId> {
        self.ledger.turn().map(|t| t.node)
    }

    /// Closes the running turn; the waiting permission requests are answered cancelled.
    fn end_turn(&mut self, w: &DomWriter, end: TurnEnd) -> Out<Vec<Reply>> {
        let Some(node) = self.turn_node() else { return Ok(Vec::new()) };
        let turn = std::mem::take(&mut self.turn);
        let cancelled = turn.interrupted || matches!(end, TurnEnd::Stop(StopReason::Cancelled) | TurnEnd::Closed { shutdown: true });
        let (turn_status, message_status) = match &end {
            _ if cancelled => (TurnStatus::Cancelled, MessageStatus::Cancelled),
            TurnEnd::Stop(StopReason::EndTurn) => (TurnStatus::Done, MessageStatus::Done),
            // Truncated output is still a complete message; the turn records why it stopped.
            TurnEnd::Stop(_) => (TurnStatus::Error, MessageStatus::Done),
            TurnEnd::Failed(_) | TurnEnd::Closed { .. } => (TurnStatus::Error, MessageStatus::Error),
        };
        if let Some((message, ..)) = turn.message {
            w.set(message, attr::STATUS, message_status)?;
        }
        for call in turn.calls.values().filter(|c| matches!(c.status, CallStatus::Pending | CallStatus::Running)) {
            w.close(call.node, CallStatus::Cancelled)?;
        }
        let replies = self.cancel_decisions(w)?;
        match end {
            TurnEnd::Stop(StopReason::EndTurn) | TurnEnd::Closed { shutdown: true } => {}
            TurnEnd::Stop(reason) => w.set(node, X_STOP_REASON, reason.as_str())?,
            TurnEnd::Failed(message) => { w.ext(node, HARNESS, "error", message)?; }
            TurnEnd::Closed { shutdown: false } => { w.ext(node, HARNESS, "error", "the agent exited")?; }
        }
        self.ledger.end_turn(w, turn_status, |_| {})?;
        Ok(replies)
    }

    /// Cancels every waiting decision and answers their requests cancelled.
    fn cancel_decisions(&mut self, w: &DomWriter) -> Out<Vec<Reply>> {
        let mut replies = Vec::with_capacity(self.decisions.len());
        for (node, pending) in self.decisions.drain() {
            w.close(node, DecisionStatus::Cancelled)?;
            self.ledger.answered(node);
            replies.push(Reply::cancelled(pending.rpc_id));
        }
        if let Some(turn) = self.ledger.turn().filter(|_| !replies.is_empty()) {
            w.set(turn.node, attr::STATUS, TurnStatus::Running)?;
        }
        Ok(replies)
    }

    fn update(&mut self, w: &DomWriter, mut update: Json) -> Out {
        let Some(kind) = update.get("sessionUpdate").and_then(Json::as_str).map(str::to_owned) else {
            return self.ext_here(w, "malformed", &update);
        };
        let kept = |e: serde_json::Error, update: &Json| format!("{e}: {update}");
        match kind.as_str() {
            "agent_message_chunk" => self.chunk(w, MessageKind::Text, &kind, update),
            "agent_thought_chunk" => self.chunk(w, MessageKind::Thinking, &kind, update),
            "tool_call" | "tool_call_update" => match ToolCallFields::deserialize(&update) {
                Ok(fields) if self.turn_node().is_some() => self.tool_call(w, fields).map(drop),
                // Malformed, or outside any turn: keep it raw.
                _ => self.ext_here(w, &kind, &update),
            },
            "plan" => match Plan::deserialize(&update) {
                Ok(plan) => self.plan(w, plan),
                Err(e) => self.ext_here(w, &kind, &kept(e, &update)),
            },
            "current_mode_update" => match CurrentModeUpdate::deserialize(&update) {
                Ok(mode) => self.mode_changed(w, &mode.current_mode_id),
                Err(e) => self.ext_here(w, &kind, &kept(e, &update)),
            },
            "session_info_update" => match SessionInfoUpdate::deserialize(&update) {
                Ok(SessionInfoUpdate { title: Some(title) }) => w.set(self.root(), attr::TITLE, title),
                _ => self.ext_here(w, &kind, &update),
            },
            _ => {
                update.as_object_mut().map(|o| o.remove("sessionUpdate"));
                self.ext_here(w, &kind, &update)
            }
        }
    }

    fn chunk(&mut self, w: &DomWriter, kind: MessageKind, update_kind: &str, update: Json) -> Out {
        let message_id = update.get("messageId").and_then(Json::as_str).map(str::to_owned);
        let content = update.get("content").filter(|c| c["type"] == "text");
        let Some(text) = content.and_then(|c| c["text"].as_str()) else { return self.ext_here(w, update_kind, &update) };
        let Some(turn) = self.turn_node() else { return w.ext(self.root(), HARNESS, update_kind, text).map(drop) };
        let open = self.turn.message.as_ref().filter(|(_, k, id)| *k == kind && (message_id.is_none() || id.is_none() || *id == message_id));
        if let Some((node, ..)) = open {
            return w.append(*node, text);
        }
        if let Some((previous, ..)) = self.turn.message.take() {
            w.set(previous, attr::STATUS, MessageStatus::Done)?;
        }
        let mut attrs = vec![(attr::ROLE, Value::from(Role::Assistant)), (attr::STATUS, MessageStatus::Streaming.into())];
        attrs.extend((kind == MessageKind::Thinking).then(|| (attr::KIND, kind.into())));
        attrs.extend(self.model.as_deref().map(|m| (attr::MODEL, m.into())));
        attrs.extend(message_id.as_deref().map(|id| (attr::ID, id.into())));
        let node = w.insert(turn, E::Message, attrs, text)?;
        self.turn.message = Some((node, kind, message_id));
        Ok(())
    }

    /// Inserts or updates the `<call>` of a `tool_call` or `tool_call_update`
    /// inside the running turn.
    fn tool_call(&mut self, w: &DomWriter, fields: ToolCallFields) -> Out<Option<NodeId>> {
        let Some(turn) = self.turn_node() else { return Ok(None) };
        let id = fields.tool_call_id.clone();
        let tool = fields.name.as_deref().or(fields.kind.as_deref());
        let node = match self.turn.calls.get(&id) {
            Some(call) => {
                let node = call.node;
                fields.title.iter().try_for_each(|title| w.set(node, attr::TITLE, title.as_str()))?;
                tool.iter().try_for_each(|tool| w.set(node, attr::TOOL, *tool))?;
                node
            }
            None => {
                if let Some((message, ..)) = self.turn.message.take() {
                    w.set(message, attr::STATUS, MessageStatus::Done)?;
                }
                let title = fields.title.as_deref().unwrap_or(&id);
                let attrs = [(attr::TOOL, Value::from(tool.unwrap_or("other"))), (attr::CALL_ID, id.as_str().into()), (attr::TITLE, title.into()), (attr::STATUS, CallStatus::Pending.into()), (attr::STARTED, w.now().into())];
                let node = w.insert(turn, E::Call, attrs, "")?;
                self.turn.calls.insert(id.clone(), Call { node, input: None, status: CallStatus::Pending, content: Vec::new(), raw_output: None });
                node
            }
        };
        let call = self.turn.calls.get_mut(&id).expect("inserted above");
        if let Some(input) = fields.raw_input.filter(|v| !v.is_null()) {
            match call.input {
                Some(existing) => w.set(existing, TEXT, input.to_string())?,
                None => call.input = Some(w.insert(node, E::Input, [(attr::FORMAT, "json")], input.to_string())?),
            }
        }
        if let Some(content) = fields.content {
            call.content = content;
        }
        if let Some(output) = fields.raw_output.filter(|v| !v.is_null()) {
            call.raw_output = Some(output);
        }
        let status = match fields.status {
            None | Some(ToolCallStatus::Pending) => return Ok(Some(node)),
            Some(ToolCallStatus::InProgress) => CallStatus::Running,
            Some(ToolCallStatus::Completed) => CallStatus::Done,
            Some(ToolCallStatus::Failed) => CallStatus::Error,
        };
        if call.status == status || matches!(call.status, CallStatus::Done | CallStatus::Error) {
            return Ok(Some(node));
        }
        call.status = status;
        if status == CallStatus::Running {
            w.set(node, attr::STATUS, status)?;
        } else {
            let (content, raw_output) = (std::mem::take(&mut call.content), call.raw_output.take());
            finish_call(w, node, status, content, raw_output)?;
        }
        Ok(Some(node))
    }

    /// Rewrites the `<todo>` list to match the agent's full plan.
    fn plan(&mut self, w: &DomWriter, plan: Plan) -> Out {
        let kept = plan.entries.len();
        for (index, entry) in plan.entries.into_iter().enumerate() {
            let state = match entry.status {
                PlanEntryStatus::Pending => TodoState::Pending,
                PlanEntryStatus::InProgress => TodoState::InProgress,
                PlanEntryStatus::Completed => TodoState::Done,
            };
            let Some(todo) = self.todos.get_mut(index) else {
                let mut attrs = vec![(attr::STATE, Value::from(state))];
                attrs.extend(entry.priority.as_deref().map(|p| (X_PRIORITY, p.into())));
                let node = w.insert(self.root(), E::Todo, attrs, entry.content.as_str())?;
                self.todos.push(Todo { node, text: entry.content, state, priority: entry.priority });
                continue;
            };
            if todo.text != entry.content {
                w.set(todo.node, TEXT, entry.content.as_str())?;
            }
            if todo.state != state {
                w.set(todo.node, attr::STATE, state)?;
            }
            if todo.priority != entry.priority {
                match &entry.priority {
                    Some(priority) => w.set(todo.node, X_PRIORITY, priority.as_str())?,
                    None => w.clear(todo.node, X_PRIORITY)?,
                }
            }
            *todo = Todo { node: todo.node, text: entry.content, state, priority: entry.priority };
        }
        for gone in self.todos.split_off(kept.min(self.todos.len())) {
            w.remove(gone.node)?;
        }
        Ok(())
    }

    /// Inserts the approval `<decision>` of a `session/request_permission`.
    fn permission(&mut self, w: &DomWriter, rpc_id: Json, request: RequestPermissionRequest) -> Out {
        let title = request.tool_call.title.clone();
        let input = request.tool_call.raw_input.as_ref().filter(|v| !v.is_null()).map(Json::to_string);
        let content = request.tool_call.content.clone().unwrap_or_default();
        // The request's tool call doubles as an update: it may be the first the client hears of it.
        let call = match self.turn.calls.get(&request.tool_call.tool_call_id) {
            Some(call) => Some(call.node),
            None => self.tool_call(w, request.tool_call)?,
        };
        let parent = call.or(self.turn_node()).unwrap_or(self.root());
        let list = |f: fn(&PermissionOption) -> String| Value::from(request.options.iter().map(f).collect::<Vec<_>>());
        let request_id = rpc_id.as_str().map_or_else(|| rpc_id.to_string(), str::to_owned);
        let mut attrs = vec![(attr::KIND, Value::from(DecisionKind::Approval)), (attr::STATUS, DecisionStatus::Waiting.into())];
        attrs.extend([(attr::OPTIONS, list(|o| o.name.clone())), (X_OPTION_IDS, list(|o| o.option_id.clone())), (X_OPTION_KINDS, list(|o| kind_str(o.kind).to_owned()))]);
        attrs.extend([(attr::REQUEST_ID, request_id.into()), (attr::CREATED, w.now().into())]);
        attrs.extend(call.map(|call| (attr::CALL, Value::Int(i64::try_from(call.0).unwrap_or(i64::MAX)))));
        attrs.extend(title.map(|title| (attr::TITLE, title.into())));
        let node = w.insert(parent, E::Decision, attrs, input.unwrap_or_default())?;
        for mut item in content {
            item.sort_all_objects();
            w.ext(node, HARNESS, "tool_call_content", item)?;
        }
        if let Some(turn) = self.turn_node() {
            w.set(turn, attr::STATUS, TurnStatus::Waiting)?;
        }
        self.ledger.wait(node);
        self.decisions.insert(node, Pending { rpc_id, options: request.options });
        Ok(())
    }

    fn ext_here(&self, w: &DomWriter, kind: &str, raw: &(impl std::fmt::Display + ?Sized)) -> Out {
        w.ext(self.turn_node().unwrap_or(self.root()), HARNESS, kind, raw).map(drop)
    }
}

/// Writes the `<result>` of a finished call; content without a text form
/// (diffs, terminals, resources) goes to `<ext>` children.
fn finish_call(w: &DomWriter, node: NodeId, status: CallStatus, content: Vec<Json>, raw_output: Option<Json>) -> Out {
    let (texts, other): (Vec<Json>, Vec<Json>) = content.into_iter().partition(|item| item["type"] == "content" && item.pointer("/content/text").is_some_and(Json::is_string));
    let mut text = texts.iter().filter_map(|item| item.pointer("/content/text").and_then(Json::as_str)).collect::<Vec<_>>().join("\n");
    let mut attrs = vec![(attr::IS_ERROR, Value::Bool(status == CallStatus::Error))];
    match raw_output.filter(|_| text.is_empty()) {
        Some(Json::String(output)) => text = output,
        Some(output) => {
            attrs.push((attr::FORMAT, "json".into()));
            text = output.to_string();
        }
        None => {}
    }
    w.insert(node, E::Result, attrs, text)?;
    for mut item in other {
        item.sort_all_objects();
        w.ext(node, HARNESS, "tool_call_content", item)?;
    }
    w.close(node, status)
}

/// The offered option `value` names: its id, its label, or a
/// [`loa_core::answer`] constant matched by option kind.
pub(crate) fn pick_option<'a>(options: &'a [PermissionOption], value: &str) -> Option<&'a PermissionOption> {
    let kinds: &[PermissionOptionKind] = match value {
        answer::ALLOW_ONCE => &[PermissionOptionKind::AllowOnce],
        answer::ALLOW_SESSION => &[PermissionOptionKind::AllowAlways],
        answer::DENY => &[PermissionOptionKind::RejectOnce, PermissionOptionKind::RejectAlways],
        _ => &[],
    };
    let named = options.iter().find(|o| o.option_id == value).or_else(|| options.iter().find(|o| o.name == value));
    named.or_else(|| kinds.iter().find_map(|kind| options.iter().find(|o| o.kind == *kind)))
}

fn kind_str(kind: PermissionOptionKind) -> &'static str {
    match kind {
        PermissionOptionKind::AllowOnce => "allow_once",
        PermissionOptionKind::AllowAlways => "allow_always",
        PermissionOptionKind::RejectOnce => "reject_once",
        PermissionOptionKind::RejectAlways => "reject_always",
    }
}
