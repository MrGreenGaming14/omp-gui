//! Turns ACP traffic into DOM patches following the adapter conventions in
//! `docs/ARCHITECTURE.md`. Pure bookkeeping over a [`PatchSink`]: no I/O.

use crate::wire::{
    CurrentModeUpdate, PermissionOption, PermissionOptionKind, Plan, PlanEntryStatus, RequestPermissionRequest,
    SessionInfoUpdate, StopReason, ToolCallFields, ToolCallStatus,
};
use loa_contracts::harness::{Prompt, SessionState};
use loa_core::{
    CallStatus, CommitError, DecisionKind, DecisionStatus, Draft, ElementKind, MessageKind, MessageStatus, NodeId,
    PatchSink, Role, SessionMode, TodoState, TurnStatus, Value, answer, attr, now_ms,
};
use serde_json::Value as Json;
use std::collections::HashMap;
use std::sync::Arc;

/// Harness id written to `<session harness>` and `<ext harness>`.
pub const HARNESS: &str = "acp";

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

/// How a turn ended.
pub enum TurnEnd {
    Stop(StopReason),
    /// `session/prompt` failed with this message.
    Failed(String),
    /// The agent went away. `shutdown` is true when the app closed it.
    Closed { shutdown: bool },
}

/// A permission request waiting for [`Projector::answer`].
pub struct PendingDecision {
    /// JSON-RPC id of the agent's `session/request_permission`.
    pub rpc_id: Json,
    pub options: Vec<PermissionOption>,
}

struct Turn {
    node: NodeId,
    message: Option<OpenMessage>,
    calls: HashMap<String, Call>,
    /// `session/prompt` request id; its response ends the turn.
    prompt_request: Option<u64>,
    interrupted: bool,
}

struct OpenMessage {
    node: NodeId,
    kind: MessageKind,
    id: Option<String>,
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

/// DOM side of one ACP session.
pub struct Projector {
    sink: Arc<dyn PatchSink>,
    root: NodeId,
    model: Option<String>,
    turn: Option<Turn>,
    decisions: HashMap<NodeId, PendingDecision>,
    todos: Vec<Todo>,
    /// Id of a running `session/load`: the history it replays is already in the DOM.
    replay_request: Option<u64>,
    closed: bool,
    /// The agent's current mode id.
    mode_id: Option<String>,
}

impl Projector {
    /// Continues the session whose `<session>` element is `root`. Assistant
    /// messages are attributed to `model`.
    pub fn new(sink: Arc<dyn PatchSink>, root: NodeId, model: Option<String>) -> Self {
        Self {
            sink,
            root,
            model,
            turn: None,
            decisions: HashMap::new(),
            todos: Vec::new(),
            replay_request: None,
            closed: false,
            mode_id: None,
        }
    }

    /// The agent switched to `model`: later assistant messages carry it, and
    /// the session records it.
    pub fn model_changed(&mut self, model: &str) {
        if self.model.as_deref() != Some(model) {
            self.model = Some(model.to_owned());
            self.set(self.root, attr::MODEL, model.into());
        }
    }

    /// Takes over `<todo>` elements already in the DOM so plan updates edit them.
    pub fn adopt_todo(&mut self, node: NodeId, text: String, state: TodoState, priority: Option<String>) {
        self.todos.push(Todo { node, text, state, priority });
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn state(&self) -> SessionState {
        match &self.turn {
            _ if self.closed => SessionState::Closed,
            None => SessionState::Idle,
            Some(_) if !self.decisions.is_empty() => SessionState::Waiting,
            Some(_) => SessionState::Running,
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn has_turn(&self) -> bool {
        self.turn.is_some()
    }

    pub fn set_replay(&mut self, request: Option<u64>) {
        self.replay_request = request;
    }

    /// Inserts the `<turn>` and the user `<message>` for `prompt`.
    pub fn begin_turn(&mut self, prompt: &Prompt) -> Result<(), CommitError> {
        let now = now_ms();
        let turn = self
            .sink
            .commit(Draft::insert(
                self.root,
                ElementKind::Turn,
                [(attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, Value::Int(now))],
                "",
            ))?
            .node;
        let mut attrs = vec![(attr::ROLE, Role::User.into()), (attr::STATUS, MessageStatus::Done.into())];
        if !prompt.by.is_empty() {
            attrs.push((attr::BY, prompt.by.as_str().into()));
        }
        self.sink.commit(Draft::insert(turn, ElementKind::Message, attrs, prompt.text.as_str()))?;
        self.turn = Some(Turn {
            node: turn,
            message: None,
            calls: HashMap::new(),
            prompt_request: None,
            interrupted: false,
        });
        Ok(())
    }

    pub fn set_prompt_request(&mut self, id: u64) {
        if let Some(turn) = &mut self.turn {
            turn.prompt_request = Some(id);
        }
    }

    pub fn mark_interrupted(&mut self) {
        if let Some(turn) = &mut self.turn {
            turn.interrupted = true;
        }
    }

    /// Routes a JSON-RPC response. Ends the turn when it answers the running
    /// `session/prompt` and returns the permission requests left waiting.
    pub fn on_response(&mut self, id: u64, result: &Result<Json, crate::rpc::RpcError>) -> Vec<Json> {
        if self.replay_request == Some(id) {
            self.replay_request = None;
        }
        let is_prompt = self.turn.as_ref().is_some_and(|t| t.prompt_request == Some(id));
        if !is_prompt {
            return Vec::new();
        }
        let end = match result {
            Ok(value) => match serde_json::from_value::<crate::wire::PromptResponse>(value.clone()) {
                Ok(response) => TurnEnd::Stop(response.stop_reason),
                Err(e) => TurnEnd::Failed(format!("malformed session/prompt response: {e}")),
            },
            Err(e) => TurnEnd::Failed(e.to_string()),
        };
        self.end_turn(end)
    }

    /// Closes the running turn and returns the permission requests left
    /// waiting, which the caller answers as cancelled.
    pub fn end_turn(&mut self, end: TurnEnd) -> Vec<Json> {
        let Some(mut turn) = self.turn.take() else { return Vec::new() };
        let now = now_ms();
        let cancelled = turn.interrupted
            || matches!(end, TurnEnd::Stop(StopReason::Cancelled) | TurnEnd::Closed { shutdown: true });
        let (turn_status, message_status) = match &end {
            _ if cancelled => (TurnStatus::Cancelled, MessageStatus::Cancelled),
            TurnEnd::Stop(StopReason::EndTurn) => (TurnStatus::Done, MessageStatus::Done),
            // Truncated output is still a complete message; the turn records why it stopped.
            TurnEnd::Stop(_) => (TurnStatus::Error, MessageStatus::Done),
            TurnEnd::Failed(_) | TurnEnd::Closed { .. } => (TurnStatus::Error, MessageStatus::Error),
        };
        if let Some(message) = turn.message.take() {
            self.set(message.node, attr::STATUS, message_status.into());
        }
        for call in turn.calls.values() {
            if matches!(call.status, CallStatus::Pending | CallStatus::Running) {
                self.set(call.node, attr::STATUS, CallStatus::Cancelled.into());
                self.set(call.node, attr::ENDED, Value::Int(now));
            }
        }
        let waiting = self.cancel_decisions();
        match end {
            TurnEnd::Stop(StopReason::EndTurn) => {}
            TurnEnd::Stop(reason) => self.set(turn.node, X_STOP_REASON, reason.as_str().into()),
            TurnEnd::Failed(message) => self.ext(turn.node, "error", message),
            TurnEnd::Closed { shutdown: false } => self.ext(turn.node, "error", "the agent exited".to_owned()),
            TurnEnd::Closed { shutdown: true } => {}
        }
        self.set(turn.node, attr::STATUS, turn_status.into());
        self.set(turn.node, attr::ENDED, Value::Int(now));
        waiting
    }

    /// The agent's output closed. Ends the turn and marks the session closed.
    pub fn close(&mut self, shutdown: bool) -> Vec<Json> {
        let waiting = self.end_turn(TurnEnd::Closed { shutdown });
        self.closed = true;
        waiting
    }

    /// Marks every waiting decision cancelled and returns their JSON-RPC ids.
    pub fn cancel_decisions(&mut self) -> Vec<Json> {
        let now = now_ms();
        let pending: Vec<(NodeId, PendingDecision)> = self.decisions.drain().collect();
        let mut ids = Vec::with_capacity(pending.len());
        for (node, decision) in pending {
            self.set(node, attr::STATUS, DecisionStatus::Cancelled.into());
            self.set(node, attr::ENDED, Value::Int(now));
            ids.push(decision.rpc_id);
        }
        if !ids.is_empty()
            && let Some(turn) = &self.turn
        {
            self.set(turn.node, attr::STATUS, TurnStatus::Running.into());
        }
        ids
    }

    /// Applies one `session/update` payload.
    pub fn update(&mut self, mut update: Json) {
        if self.replay_request.is_some() {
            return;
        }
        let kind = match update.get("sessionUpdate").and_then(Json::as_str) {
            Some(kind) => kind.to_owned(),
            None => return self.ext_here("malformed", update.to_string()),
        };
        match kind.as_str() {
            "agent_message_chunk" => self.chunk(MessageKind::Text, &kind, update),
            "agent_thought_chunk" => self.chunk(MessageKind::Thinking, &kind, update),
            "tool_call" | "tool_call_update" => match parse::<ToolCallFields>(&update) {
                Ok(fields) if self.turn.is_some() => {
                    self.tool_call(fields);
                }
                // Malformed, or outside any turn: keep it raw.
                _ => self.ext_here(&kind, update.to_string()),
            },
            "plan" => match parse::<Plan>(&update) {
                Ok(plan) => self.plan(plan),
                Err(e) => self.ext_here(&kind, format!("{e}: {update}")),
            },
            "current_mode_update" => match parse::<CurrentModeUpdate>(&update) {
                Ok(mode) => self.mode_changed(&mode.current_mode_id),
                Err(e) => self.ext_here(&kind, format!("{e}: {update}")),
            },
            "session_info_update" => match parse::<SessionInfoUpdate>(&update) {
                Ok(SessionInfoUpdate { title: Some(title) }) => self.set(self.root, attr::TITLE, title.into()),
                _ => self.ext_here(&kind, update.to_string()),
            },
            _ => {
                if let Some(object) = update.as_object_mut() {
                    object.remove("sessionUpdate");
                }
                self.ext_here(&kind, update.to_string());
            }
        }
    }

    /// Records the agent's current mode on the session.
    pub fn mode_changed(&mut self, mode_id: &str) {
        if self.mode_id.as_deref() == Some(mode_id) {
            return;
        }
        self.mode_id = Some(mode_id.to_owned());
        let mode = if mode_id == "plan" { SessionMode::Plan } else { SessionMode::Normal };
        self.set(self.root, X_MODE, mode_id.into());
        self.set(self.root, attr::MODE, mode.into());
    }

    pub fn mode_id(&self) -> Option<&str> {
        self.mode_id.as_deref()
    }

    /// Adopts the mode id already recorded on a resumed session.
    pub fn adopt_mode(&mut self, mode_id: String) {
        self.mode_id = Some(mode_id);
    }

    fn chunk(&mut self, kind: MessageKind, update_kind: &str, mut update: Json) {
        let message_id = update.get("messageId").and_then(Json::as_str).map(str::to_owned);
        let text = match update.get_mut("content") {
            Some(content) if content.get("type").and_then(Json::as_str) == Some("text") => {
                match content.get_mut("text").map(Json::take) {
                    Some(Json::String(text)) => text,
                    _ => return self.ext_here(update_kind, update.to_string()),
                }
            }
            _ => return self.ext_here(update_kind, update.to_string()),
        };
        let Some(turn) = &mut self.turn else {
            return self.ext(self.root, update_kind, text);
        };
        let continued = turn.message.as_ref().filter(|open| {
            open.kind == kind && (message_id.is_none() || open.id.is_none() || open.id == message_id)
        });
        if let Some(open) = continued {
            let node = open.node;
            self.commit(Draft::append(node, text));
            return;
        }
        let turn_node = turn.node;
        let previous = turn.message.take();
        if let Some(previous) = previous {
            self.set(previous.node, attr::STATUS, MessageStatus::Done.into());
        }
        let mut attrs = vec![(attr::ROLE, Role::Assistant.into()), (attr::STATUS, MessageStatus::Streaming.into())];
        if kind == MessageKind::Thinking {
            attrs.push((attr::KIND, kind.into()));
        }
        if let Some(model) = &self.model {
            attrs.push((attr::MODEL, model.as_str().into()));
        }
        if let Some(id) = &message_id {
            attrs.push((attr::ID, id.as_str().into()));
        }
        let node = self.insert(turn_node, ElementKind::Message, attrs, text);
        if let (Some(node), Some(turn)) = (node, &mut self.turn) {
            turn.message = Some(OpenMessage { node, kind, id: message_id });
        }
    }

    fn close_message(&mut self) {
        if let Some(message) = self.turn.as_mut().and_then(|t| t.message.take()) {
            self.set(message.node, attr::STATUS, MessageStatus::Done.into());
        }
    }

    /// Inserts or updates the `<call>` for a `tool_call` or `tool_call_update`.
    /// Returns its node, or `None` outside a turn.
    fn tool_call(&mut self, fields: ToolCallFields) -> Option<NodeId> {
        let turn_node = self.turn.as_ref()?.node;
        let known = self.turn.as_ref().and_then(|t| t.calls.get(&fields.tool_call_id)).map(|c| c.node);
        let node = match known {
            Some(node) => node,
            None => {
                self.close_message();
                let tool = fields.name.as_deref().or(fields.kind.as_deref()).unwrap_or("other");
                let title = fields.title.as_deref().unwrap_or(&fields.tool_call_id);
                let attrs = vec![
                    (attr::TOOL, tool.into()),
                    (attr::CALL_ID, fields.tool_call_id.as_str().into()),
                    (attr::TITLE, title.into()),
                    (attr::STATUS, CallStatus::Pending.into()),
                    (attr::STARTED, Value::Int(now_ms())),
                ];
                let node = self.insert(turn_node, ElementKind::Call, attrs, String::new())?;
                let call = Call { node, input: None, status: CallStatus::Pending, content: Vec::new(), raw_output: None };
                self.turn.as_mut()?.calls.insert(fields.tool_call_id.clone(), call);
                node
            }
        };
        if known.is_some() {
            if let Some(title) = &fields.title {
                self.set(node, attr::TITLE, title.as_str().into());
            }
            if let Some(tool) = fields.name.as_deref().or(fields.kind.as_deref()) {
                self.set(node, attr::TOOL, tool.into());
            }
        }
        if let Some(input) = fields.raw_input.filter(|v| !v.is_null()) {
            let text = input.to_string();
            let existing = self.call(&fields.tool_call_id)?.input;
            match existing {
                Some(input) => self.commit(Draft::set_text(input, text)),
                None => {
                    let input = self.insert(node, ElementKind::Input, [(attr::FORMAT, "json".into())], text);
                    self.call_mut(&fields.tool_call_id)?.input = input;
                }
            }
        }
        let call = self.call_mut(&fields.tool_call_id)?;
        if let Some(content) = fields.content {
            call.content = content;
        }
        if let Some(output) = fields.raw_output.filter(|v| !v.is_null()) {
            call.raw_output = Some(output);
        }
        let status = match fields.status {
            None | Some(ToolCallStatus::Pending) => return Some(node),
            Some(ToolCallStatus::InProgress) => CallStatus::Running,
            Some(ToolCallStatus::Completed) => CallStatus::Done,
            Some(ToolCallStatus::Failed) => CallStatus::Error,
        };
        if call.status == status || matches!(call.status, CallStatus::Done | CallStatus::Error) {
            return Some(node);
        }
        call.status = status;
        let finished = (status != CallStatus::Running)
            .then(|| (std::mem::take(&mut call.content), call.raw_output.take()));
        match finished {
            Some((content, raw_output)) => self.finish_call(node, status, content, raw_output),
            None => self.set(node, attr::STATUS, status.into()),
        }
        Some(node)
    }

    /// Writes the `<result>` of a finished call and the content that has no
    /// text form (diffs, terminals, resources) as `<ext>` children.
    fn finish_call(&mut self, node: NodeId, status: CallStatus, content: Vec<Json>, raw_output: Option<Json>) {
        let mut text = String::new();
        let mut other = Vec::new();
        for item in content {
            let block_text = (item.get("type").and_then(Json::as_str) == Some("content"))
                .then(|| item.pointer("/content/text").and_then(Json::as_str))
                .flatten();
            match block_text {
                Some(block) => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(block);
                }
                None => other.push(item),
            }
        }
        let mut attrs = vec![(attr::IS_ERROR, Value::Bool(status == CallStatus::Error))];
        if text.is_empty()
            && let Some(output) = raw_output
        {
            text = match output {
                Json::String(s) => s,
                output => {
                    attrs.push((attr::FORMAT, "json".into()));
                    output.to_string()
                }
            };
        }
        self.insert(node, ElementKind::Result, attrs, text);
        for item in other {
            self.ext(node, "tool_call_content", item.to_string());
        }
        self.set(node, attr::STATUS, status.into());
        self.set(node, attr::ENDED, Value::Int(now_ms()));
    }

    fn call(&self, id: &str) -> Option<&Call> {
        self.turn.as_ref()?.calls.get(id)
    }

    fn call_mut(&mut self, id: &str) -> Option<&mut Call> {
        self.turn.as_mut()?.calls.get_mut(id)
    }

    /// Rewrites the `<todo>` list to match the agent's full plan.
    fn plan(&mut self, plan: Plan) {
        let mut entries = plan.entries.into_iter();
        let mut kept = 0;
        for (index, entry) in entries.by_ref().enumerate() {
            let state = match entry.status {
                PlanEntryStatus::Pending => TodoState::Pending,
                PlanEntryStatus::InProgress => TodoState::InProgress,
                PlanEntryStatus::Completed => TodoState::Done,
            };
            kept = index + 1;
            if let Some(todo) = self.todos.get(index) {
                let node = todo.node;
                let (same_state, same_text, same_priority) =
                    (todo.state == state, todo.text == entry.content, todo.priority == entry.priority);
                if !same_text {
                    self.commit(Draft::set_text(node, entry.content.as_str()));
                }
                if !same_state {
                    self.set(node, attr::STATE, state.into());
                }
                if !same_priority {
                    match &entry.priority {
                        Some(priority) => self.set(node, X_PRIORITY, priority.as_str().into()),
                        None => self.commit(Draft::clear(node, X_PRIORITY)),
                    }
                }
                self.todos[index] = Todo { node, text: entry.content, state, priority: entry.priority };
                continue;
            }
            let mut attrs = vec![(attr::STATE, state.into())];
            if let Some(priority) = &entry.priority {
                attrs.push((X_PRIORITY, priority.as_str().into()));
            }
            if let Some(node) = self.insert(self.root, ElementKind::Todo, attrs, entry.content.clone()) {
                self.todos.push(Todo { node, text: entry.content, state, priority: entry.priority });
            }
        }
        for gone in self.todos.split_off(kept.min(self.todos.len())) {
            self.commit(Draft::remove(gone.node));
        }
    }

    /// Inserts the approval `<decision>` for a `session/request_permission`.
    pub fn permission(&mut self, rpc_id: Json, request: RequestPermissionRequest) {
        let call_id = request.tool_call.tool_call_id.clone();
        let title = request.tool_call.title.clone();
        let input = request.tool_call.raw_input.as_ref().filter(|v| !v.is_null()).map(Json::to_string);
        let content: Vec<Json> = request.tool_call.content.clone().unwrap_or_default();
        // The request's tool call doubles as an update: it may be the first
        // the client hears of the call.
        let call = match self.call(&call_id) {
            Some(call) => Some(call.node),
            None => self.tool_call(request.tool_call),
        };
        let parent = call.or(self.turn.as_ref().map(|t| t.node)).unwrap_or(self.root);
        let names: Vec<String> = request.options.iter().map(|o| o.name.clone()).collect();
        let ids: Vec<String> = request.options.iter().map(|o| o.option_id.clone()).collect();
        let kinds: Vec<String> = request.options.iter().map(|o| kind_str(o.kind).to_owned()).collect();
        let mut attrs = vec![
            (attr::KIND, DecisionKind::Approval.into()),
            (attr::STATUS, DecisionStatus::Waiting.into()),
            (attr::OPTIONS, names.into()),
            (X_OPTION_IDS, ids.into()),
            (X_OPTION_KINDS, kinds.into()),
            (attr::REQUEST_ID, request_id_text(&rpc_id).into()),
            (attr::CREATED, Value::Int(now_ms())),
        ];
        if let Some(call) = call {
            attrs.push((attr::CALL, Value::Int(i64::try_from(call.0).unwrap_or(i64::MAX))));
        }
        if let Some(title) = title {
            attrs.push((attr::TITLE, title.into()));
        }
        let Some(node) = self.insert(parent, ElementKind::Decision, attrs, input.unwrap_or_default()) else {
            return;
        };
        for item in content {
            self.ext(node, "tool_call_content", item.to_string());
        }
        if let Some(turn) = &self.turn {
            self.set(turn.node, attr::STATUS, TurnStatus::Waiting.into());
        }
        self.decisions.insert(node, PendingDecision { rpc_id, options: request.options });
    }

    pub fn decision(&self, node: NodeId) -> Option<&PendingDecision> {
        self.decisions.get(&node)
    }

    /// Records the answer to decision `node` and returns its JSON-RPC id.
    pub fn answer(&mut self, node: NodeId, option: &PermissionOption, by: &str, reason: Option<&str>) -> Option<Json> {
        let pending = self.decisions.remove(&node)?;
        let canonical = match option.kind {
            PermissionOptionKind::AllowOnce => answer::ALLOW_ONCE,
            PermissionOptionKind::AllowAlways => answer::ALLOW_SESSION,
            PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways => answer::DENY,
        };
        self.set(node, attr::ANSWER, Value::List(vec![canonical.into()]));
        self.set(node, X_OPTION, option.option_id.as_str().into());
        if !by.is_empty() {
            self.set(node, attr::BY, by.into());
        }
        if let Some(reason) = reason {
            self.set(node, attr::REASON, reason.into());
        }
        self.set(node, attr::STATUS, DecisionStatus::Answered.into());
        self.set(node, attr::ENDED, Value::Int(now_ms()));
        if self.decisions.is_empty()
            && let Some(turn) = &self.turn
        {
            self.set(turn.node, attr::STATUS, TurnStatus::Running.into());
        }
        Some(pending.rpc_id)
    }

    fn ext_here(&mut self, kind: &str, raw: String) {
        let parent = self.turn.as_ref().map_or(self.root, |t| t.node);
        self.ext(parent, kind, raw);
    }

    fn ext(&self, parent: NodeId, kind: &str, raw: String) {
        self.insert(parent, ElementKind::Ext, [(attr::HARNESS, HARNESS.into()), (attr::TYPE, kind.into())], raw);
    }

    pub fn set(&self, node: NodeId, name: &str, value: Value) {
        self.commit(Draft::set(node, name, value));
    }

    fn insert<N: Into<String>>(
        &self,
        parent: NodeId,
        kind: ElementKind,
        attrs: impl IntoIterator<Item = (N, Value)>,
        text: impl Into<String>,
    ) -> Option<NodeId> {
        self.commit_patch(Draft::insert(parent, kind, attrs, text)).map(|p| p.node)
    }

    fn commit(&self, draft: Draft) {
        self.commit_patch(draft);
    }

    fn commit_patch(&self, draft: Draft) -> Option<loa_core::Patch> {
        match self.sink.commit(draft) {
            Ok(patch) => Some(patch),
            Err(e) => {
                tracing::warn!("acp: dropping a patch the session refused: {e}");
                None
            }
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(update: &Json) -> Result<T, serde_json::Error> {
    T::deserialize(update)
}

fn kind_str(kind: PermissionOptionKind) -> &'static str {
    match kind {
        PermissionOptionKind::AllowOnce => "allow_once",
        PermissionOptionKind::AllowAlways => "allow_always",
        PermissionOptionKind::RejectOnce => "reject_once",
        PermissionOptionKind::RejectAlways => "reject_always",
    }
}

/// `request_id` text of a JSON-RPC id: the number or string itself.
fn request_id_text(id: &Json) -> String {
    match id {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}
