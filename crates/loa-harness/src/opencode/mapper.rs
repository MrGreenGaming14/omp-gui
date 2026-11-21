//! The event mapper: a pure state machine that turns OpenCode server events
//! (and the same objects read from an export file) into session DOM patches.
//!
//! Shapes, following the DOM conventions of `docs/ARCHITECTURE.md`:
//! - one `<turn id=<user message id>>` per OpenCode user message, holding
//!   everything the assistant messages that answer it produced
//! - text and reasoning parts become `<message id=<part id>>` (`kind="thinking"`
//!   for reasoning), streamed with `append`
//! - tool parts become `<call>` with `<input>` and `<result>`, following the
//!   tool state machine `pending → running → completed | error`
//! - the task tool's child session becomes `<job kind="subagent">` under its call
//! - each assistant message's info is kept in `<ext type="message">`, every
//!   other part (step start and finish, files, patches, snapshots, compaction
//!   markers, …) in `<ext type="part">`, both as JSON text
//! - permission and question requests become `<decision>` elements
//! - the todo list becomes `<todo>` elements under the session
//! - a finished summary message (compaction) adds `<compaction>` to the session
//!
//! Native fields that no schema attribute holds are kept as compact JSON in
//! `x-oc`, so [`super::native::export`] rebuilds the native objects exactly.

use super::event::{Event, PartDelta, PermissionRequest, QuestionRequest, Todo, ToolRef};
use super::json;
use loa_contracts::harness::{AdapterError, Answer, Result, SessionState};
use loa_core::{
    CallStatus, DecisionKind, DecisionStatus, Dom, Draft, ElementKind as K, JobKind, JobStatus, MessageKind,
    MessageStatus, Node, NodeId, PatchSink, Role, SCHEMA_VERSION, SessionMode, TodoState, TurnStatus, Value, answer,
    attr,
};
use serde_json::Value as Json;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Harness id written to `<session harness>` and `<ext harness>`.
pub const HARNESS: &str = "opencode";

/// Extension attributes this adapter writes.
pub mod xattr {
    /// Compact JSON of the native object's fields that no other attribute holds.
    pub const OC: &str = "x-oc";
    /// Native id of the message an element belongs to (or is, for `<ext type="message">`).
    pub const MESSAGE: &str = "x-message";
    /// Native part id of a `<call>` or `<ext type="part">`.
    pub const PART: &str = "x-part";
    /// Native id of a subagent's child session, on its `<job>`.
    pub const NATIVE_SESSION: &str = "x-native-session";
    /// Todo priority (`high`, `medium`, `low`).
    pub const PRIORITY: &str = "x-priority";
    /// Todo status OpenCode reported when it isn't one of the schema states.
    pub const STATUS: &str = "x-status";
    /// Position of a question within its request.
    pub const INDEX: &str = "x-index";
    /// A question that accepts typed answers.
    pub const CUSTOM: &str = "x-custom";
    /// JSON of the session's pending revert (`{messageID, partID?, …}`).
    pub const REVERT: &str = "x-revert";
    /// Reasoning tokens of a turn, on its `<usage>`.
    pub const REASONING_TOKENS: &str = "x-reasoning-tokens";
    /// App session id of the parent of a subagent's mirrored child session.
    pub const PARENT_SESSION: &str = "x-parent-session";
}

/// `<ext type>` values this adapter writes.
pub mod ext {
    pub const MESSAGE: &str = "message";
    pub const PART: &str = "part";
    pub const ERROR: &str = "error";
}

/// Opens the app session that mirrors a subagent's child session (a port:
/// the app backs it with its session store).
pub trait ChildSessions: Send + Sync {
    /// Returns the app session id for native child `native_id` of the app
    /// session `parent_session_id`, and a sink holding an empty DOM, or `None`
    /// to not mirror this child.
    fn open_child(&self, parent_session_id: &str, native_id: &str) -> Option<(String, Arc<dyn PatchSink>)>;
}

/// What goes on the `<session>` root.
#[derive(Clone, Debug, Default)]
pub struct RootInfo {
    pub session_id: String,
    pub node: String,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub model: Option<String>,
    pub model_role: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<SessionMode>,
    /// The native session object (`Session` of the OpenCode API).
    pub native: Json,
    /// More `x-` attributes.
    pub extra: Vec<(String, Value)>,
}

/// How to answer a native request once a decision is answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// `POST /permission/{request_id}/reply` with `once`, `always` or `reject`.
    Permission { request_id: String, reply: &'static str, message: Option<String> },
    /// `POST /question/{request_id}/reply` with one answer list per question.
    Question { request_id: String, answers: Vec<Vec<String>> },
    /// `POST /question/{request_id}/reject`.
    RejectQuestion { request_id: String },
    /// Other questions of the same request still wait; nothing to send yet.
    Later,
}

/// Running FNV-1a hash and length of streamed text, so a full text update can
/// be checked against what was appended without keeping a copy.
#[derive(Clone, Copy, Debug)]
struct Tracked {
    len: usize,
    hash: u64,
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01b3;

fn fnv(mut hash: u64, text: &str) -> u64 {
    for b in text.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

impl Tracked {
    fn of(text: &str) -> Self {
        Tracked { len: text.len(), hash: fnv(FNV_OFFSET, text) }
    }

    fn push(&mut self, text: &str) {
        self.len += text.len();
        self.hash = fnv(self.hash, text);
    }

    fn is_prefix_of(&self, text: &str) -> bool {
        text.len() >= self.len && text.is_char_boundary(self.len) && fnv(FNV_OFFSET, &text[..self.len]) == self.hash
    }
}

#[derive(Debug)]
struct Msg {
    turn: NodeId,
    user: bool,
    /// `<ext type="message">` of an assistant message.
    ext: Option<NodeId>,
    /// Last `x-oc` (user) or ext text (assistant) written.
    oc: String,
    model: Option<String>,
    summary: bool,
    compacted: bool,
}

#[derive(Debug)]
enum PartKind {
    Text {
        node: NodeId,
        text: Tracked,
        status: MessageStatus,
        /// Full text, kept only for summary messages (the compaction summary).
        keep: Option<String>,
    },
    Call {
        node: NodeId,
        input: NodeId,
        input_text: Tracked,
        result: Option<(NodeId, Tracked)>,
        status: CallStatus,
        title: Option<String>,
        started: Option<i64>,
        ended: Option<i64>,
        job: Option<NodeId>,
    },
    Ext {
        node: NodeId,
        text: String,
    },
}

impl PartKind {
    fn node(&self) -> NodeId {
        match self {
            PartKind::Text { node, .. } | PartKind::Call { node, .. } | PartKind::Ext { node, .. } => *node,
        }
    }
}

#[derive(Debug)]
struct PartEntry {
    turn: NodeId,
    message: String,
    oc: String,
    kind: PartKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Usage {
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cache_write: i64,
    cost: f64,
}

#[derive(Debug, Default)]
struct TurnState {
    /// Usage of each step-finish part, by part id.
    steps: BTreeMap<String, Usage>,
    error: bool,
    aborted: bool,
    last_ts: i64,
    open: bool,
    model: Option<String>,
}

#[derive(Debug)]
enum RequestKind {
    Permission,
    Question { index: usize, count: usize },
}

#[derive(Debug)]
struct Pending {
    request_id: String,
    kind: RequestKind,
    turn: Option<NodeId>,
    waiting: bool,
    answer: Option<Vec<String>>,
}

#[derive(Debug)]
struct TodoNode {
    node: NodeId,
    todo: Todo,
}

struct Mirror {
    mapper: Box<EventMapper>,
    sink: Arc<dyn PatchSink>,
    session_id: String,
}

#[derive(Default)]
struct Child {
    job: Option<NodeId>,
    call: Option<NodeId>,
    turn: Option<NodeId>,
    mirror: Option<Mirror>,
}

/// Maps one native OpenCode session (and, through [`ChildSessions`], its
/// subagent children) onto one session DOM.
pub struct EventMapper {
    root: NodeId,
    session_id: String,
    node: String,
    cwd: Option<String>,
    native_id: String,
    session_oc: String,
    title: Option<String>,
    revert: Option<String>,
    turns: HashMap<String, NodeId>,
    turn_seq: Vec<NodeId>,
    /// Index into `turn_seq` of the first turn the next compaction covers.
    covered: usize,
    turn_state: HashMap<NodeId, TurnState>,
    active: Option<NodeId>,
    /// The user message `begin_turn` inserted, waiting for its native text part.
    bind: Option<(String, NodeId, Tracked)>,
    messages: HashMap<String, Msg>,
    parts: HashMap<String, PartEntry>,
    /// Native call id to (`<call>`, `<turn>`).
    calls: HashMap<String, (NodeId, NodeId)>,
    decisions: BTreeMap<NodeId, Pending>,
    todos: Vec<TodoNode>,
    children: HashMap<String, Child>,
    child_port: Option<Arc<dyn ChildSessions>>,
    model: Option<String>,
}

fn ins(
    sink: &dyn PatchSink,
    parent: NodeId,
    kind: K,
    attrs: Vec<(&str, Value)>,
    text: impl Into<String>,
    ts: i64,
) -> Result<NodeId> {
    Ok(sink.commit_at(Draft::insert(parent, kind, attrs, text), ts)?.node)
}

fn set(sink: &dyn PatchSink, node: NodeId, name: &str, value: impl Into<Value>, ts: i64) -> Result<()> {
    sink.commit_at(Draft::set(node, name, value), ts)?;
    Ok(())
}

/// `provider/model` of a native model reference.
pub fn model_name(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
}

/// Brings a node's text from what `tracked` describes to `full`, appending
/// when `full` extends it and replacing it otherwise.
fn sync_text(sink: &dyn PatchSink, node: NodeId, tracked: &mut Tracked, full: &str, ts: i64) -> Result<()> {
    if tracked.is_prefix_of(full) {
        if full.len() > tracked.len {
            let tail = &full[tracked.len..];
            sink.commit_at(Draft::append(node, tail), ts)?;
            tracked.push(tail);
        }
    } else {
        sink.commit_at(Draft::set_text(node, full), ts)?;
        *tracked = Tracked::of(full);
    }
    Ok(())
}

fn call_status(native: &str) -> CallStatus {
    match native {
        "pending" => CallStatus::Pending,
        "running" => CallStatus::Running,
        "error" => CallStatus::Error,
        _ => CallStatus::Done,
    }
}

const TEXT_FIELDS: &[&str] = &["/id", "/sessionID", "/messageID", "/type", "/text"];
const EXT_FIELDS: &[&str] = &["/id", "/sessionID", "/messageID"];
const TOOL_FIELDS: &[&str] = &[
    "/id",
    "/sessionID",
    "/messageID",
    "/type",
    "/callID",
    "/tool",
    "/state/raw",
    "/state/title",
    "/state/output",
    "/state/error",
];
const SESSION_FIELDS: &[&str] = &["/id", "/title", "/directory", "/time/created"];
const USER_FIELDS: &[&str] = &["/id", "/sessionID", "/role", "/time/created"];
const ASSISTANT_FIELDS: &[&str] = &["/id", "/sessionID"];

/// The `<input>` text of a tool state: the streamed raw arguments while
/// pending, the JSON arguments after.
fn input_text(state: &Json) -> String {
    match json::str_at(state, "/status") {
        Some("pending") => json::str_at(state, "/raw").unwrap_or_default().to_owned(),
        _ => state.get("input").map_or_else(|| "{}".to_owned(), Json::to_string),
    }
}

fn tool_remainder(part: &Json) -> Json {
    let mut rest = json::without(part, TOOL_FIELDS);
    if json::str_at(part, "/state/status") != Some("pending")
        && let Some(Json::Object(state)) = rest.get_mut("state")
    {
        state.remove("input");
    }
    rest
}

fn todo_state(status: &str) -> Option<TodoState> {
    match status {
        "completed" => Some(TodoState::Done),
        other => TodoState::parse(other),
    }
}

impl EventMapper {
    /// Inserts the `<session>` root into `sink` (an empty DOM).
    pub fn start(sink: &dyn PatchSink, info: RootInfo, ts: i64) -> Result<Self> {
        let native = &info.native;
        let native_id = json::str_at(native, "/id").unwrap_or_default().to_owned();
        let created = json::int_at(native, "/time/created").unwrap_or(ts);
        let title = json::str_at(native, "/title").map(str::to_owned);
        let cwd = info.cwd.clone().or_else(|| json::str_at(native, "/directory").map(str::to_owned));
        let session_oc = json::remainder_text(&json::without(native, SESSION_FIELDS));
        let mut attrs: Vec<(&str, Value)> = vec![
            (attr::ID, info.session_id.as_str().into()),
            (attr::HARNESS, HARNESS.into()),
            (attr::NODE, info.node.as_str().into()),
            (attr::CREATED, created.into()),
            (attr::SCHEMA, i64::from(SCHEMA_VERSION).into()),
        ];
        let optional = [
            (attr::CWD, cwd.clone()),
            (attr::PROJECT, info.project.clone()),
            (attr::MODEL, info.model.clone()),
            (attr::MODEL_ROLE, info.model_role.clone()),
            (attr::EFFORT, info.effort.clone()),
            (attr::TITLE, title.clone()),
        ];
        attrs.extend(optional.into_iter().filter_map(|(k, v)| Some((k, Value::Str(v?)))));
        if let Some(mode) = info.mode {
            attrs.push((attr::MODE, mode.into()));
        }
        if !native_id.is_empty() {
            attrs.push((attr::NATIVE_ID, native_id.as_str().into()));
        }
        if !session_oc.is_empty() {
            attrs.push((xattr::OC, session_oc.as_str().into()));
        }
        let revert = native.get("revert").filter(|r| !r.is_null()).map(Json::to_string);
        if let Some(revert) = &revert {
            attrs.push((xattr::REVERT, revert.as_str().into()));
        }
        let extra: Vec<(&str, Value)> = info.extra.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        attrs.extend(extra);
        let root = ins(sink, NodeId::DOCUMENT, K::Session, attrs, "", created)?;
        Ok(Self::empty(root, info.session_id, info.node, cwd, native_id, session_oc, title, revert, info.model))
    }

    #[allow(clippy::too_many_arguments)]
    fn empty(
        root: NodeId,
        session_id: String,
        node: String,
        cwd: Option<String>,
        native_id: String,
        session_oc: String,
        title: Option<String>,
        revert: Option<String>,
        model: Option<String>,
    ) -> Self {
        EventMapper {
            root,
            session_id,
            node,
            cwd,
            native_id,
            session_oc,
            title,
            revert,
            turns: HashMap::new(),
            turn_seq: Vec::new(),
            covered: 0,
            turn_state: HashMap::new(),
            active: None,
            bind: None,
            messages: HashMap::new(),
            parts: HashMap::new(),
            calls: HashMap::new(),
            decisions: BTreeMap::new(),
            todos: Vec::new(),
            children: HashMap::new(),
            child_port: None,
            model,
        }
    }

    /// Mirrors subagent child sessions through `port`.
    pub fn with_child_sessions(mut self, port: Option<Arc<dyn ChildSessions>>) -> Self {
        self.child_port = port;
        self
    }

    /// The `<session>` node.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// The native session id events are matched against.
    pub fn native_id(&self) -> &str {
        &self.native_id
    }

    /// Points the mapper at another native session (after a native fork) and
    /// records it as `<session native_id>`.
    pub fn set_native_id(&mut self, sink: &dyn PatchSink, native_id: &str, ts: i64) -> Result<()> {
        self.native_id = native_id.to_owned();
        set(sink, self.root, attr::NATIVE_ID, native_id, ts)
    }

    /// The model of the latest assistant message or selection, `provider/model`.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Records a model selection on the session.
    pub fn set_model(&mut self, sink: &dyn PatchSink, model: &str, ts: i64) -> Result<()> {
        self.model = Some(model.to_owned());
        set(sink, self.root, attr::MODEL, model, ts)
    }

    pub fn state(&self) -> SessionState {
        match self.active {
            None => SessionState::Idle,
            Some(turn) if self.decisions.values().any(|d| d.waiting && d.turn == Some(turn)) => SessionState::Waiting,
            Some(_) => SessionState::Running,
        }
    }

    /// Ids of the native messages the DOM holds, users and assistants.
    pub fn message_ids(&self) -> impl Iterator<Item = &str> {
        self.messages.keys().map(String::as_str)
    }

    /// The native user message id of a turn.
    pub fn turn_message(&self, turn: NodeId) -> Option<&str> {
        self.turns.iter().find(|(_, t)| **t == turn).map(|(id, _)| id.as_str())
    }

    /// Inserts the `<turn>` and user `<message>` of a prompt the app sends
    /// with native message id `message_id`.
    pub fn begin_turn(&mut self, sink: &dyn PatchSink, message_id: &str, text: &str, by: &str, ts: i64) -> Result<NodeId> {
        if self.active.is_some() {
            return Err(AdapterError::Busy);
        }
        let turn = ins(
            sink,
            self.root,
            K::Turn,
            vec![(attr::ID, message_id.into()), (attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, ts.into())],
            "",
            ts,
        )?;
        let mut attrs: Vec<(&str, Value)> = vec![
            (attr::ROLE, Role::User.into()),
            (attr::STATUS, MessageStatus::Done.into()),
            (xattr::MESSAGE, message_id.into()),
        ];
        if !by.is_empty() {
            attrs.push((attr::BY, by.into()));
        }
        let message = ins(sink, turn, K::Message, attrs, text, ts)?;
        self.open_turn(message_id, turn, ts);
        self.bind = Some((message_id.to_owned(), message, Tracked::of(text)));
        Ok(turn)
    }

    fn open_turn(&mut self, message_id: &str, turn: NodeId, ts: i64) {
        self.turns.insert(message_id.to_owned(), turn);
        self.turn_seq.push(turn);
        self.turn_state.insert(turn, TurnState { open: true, last_ts: ts, model: self.model.clone(), ..Default::default() });
        self.active = Some(turn);
    }

    /// Ends `turn` with an error the harness never reported (the prompt
    /// request failed), keeping the message in `<ext type="error">`.
    pub fn fail_turn(&mut self, sink: &dyn PatchSink, turn: NodeId, message: &str, ts: i64) -> Result<()> {
        ins(sink, turn, K::Ext, vec![(attr::HARNESS, HARNESS.into()), (attr::TYPE, ext::ERROR.into())], message, ts)?;
        if self.bind.as_ref().is_some_and(|b| self.turns.get(&b.0) == Some(&turn)) {
            self.bind = None;
        }
        self.close_turn(sink, turn, Some(TurnStatus::Error), ts)
    }

    /// The interrupt convention: streaming messages, running calls and jobs
    /// and waiting decisions become `cancelled`, then the turn does.
    pub fn cancel_turn(&mut self, sink: &dyn PatchSink, ts: i64) -> Result<()> {
        match self.active {
            Some(turn) => self.close_turn(sink, turn, Some(TurnStatus::Cancelled), ts),
            None => Ok(()),
        }
    }

    /// Ends the open turn as the harness reported it (import: the file ended).
    pub fn finish(&mut self, sink: &dyn PatchSink, ts: i64) -> Result<()> {
        match self.active {
            Some(turn) => {
                let ts = self.turn_state.get(&turn).map_or(ts, |s| s.last_ts.max(ts));
                self.close_turn(sink, turn, None, ts)
            }
            None => Ok(()),
        }
    }

    /// Feeds one server event.
    pub fn apply(&mut self, sink: &dyn PatchSink, event: Event, ts: i64) -> Result<()> {
        let Some(sid) = event.session_id() else { return Ok(()) };
        if sid != self.native_id {
            if let Event::SessionCreated { info } = &event
                && json::str_at(info, "/parentID") == Some(self.native_id.as_str())
            {
                return self.register_child(sink, info, ts);
            }
            if self.children.contains_key(sid) {
                let sid = sid.to_owned();
                return self.child_event(sink, &sid, event, ts);
            }
            return Ok(());
        }
        match event {
            Event::Connected
            | Event::Heartbeat
            | Event::SessionCreated { .. }
            | Event::SessionDeleted { .. }
            | Event::SessionCompacted { .. } => Ok(()),
            Event::SessionUpdated { info } => self.session_updated(sink, &info, ts),
            Event::SessionStatus { status, raw, .. } => match status.as_str() {
                "idle" => self.idle(sink, ts),
                "retry" => {
                    let parent = self.active.unwrap_or(self.root);
                    let attrs = vec![(attr::HARNESS, HARNESS.into()), (attr::TYPE, "session.status".into())];
                    ins(sink, parent, K::Ext, attrs, raw.to_string(), ts).map(drop)
                }
                _ => Ok(()),
            },
            Event::SessionIdle { .. } => self.idle(sink, ts),
            Event::SessionError { error, .. } => self.session_error(sink, error, ts),
            Event::MessageUpdated { info, .. } => self.message_updated(sink, &info, ts),
            Event::MessageRemoved { message_id, .. } => self.message_removed(sink, &message_id, ts),
            Event::PartUpdated { part, .. } => self.part_updated(sink, &part, ts),
            Event::PartDelta(delta) => self.part_delta(sink, &delta, ts),
            Event::PartRemoved { part_id, .. } => self.part_removed(sink, &part_id, ts),
            Event::TodoUpdated { todos, .. } => self.sync_todos(sink, &todos, ts),
            Event::PermissionAsked(request) => self.permission_asked(sink, &request, None, ts),
            Event::PermissionReplied { request_id, reply, .. } => self.permission_replied(sink, &request_id, &reply, ts),
            Event::QuestionAsked(request) => self.question_asked(sink, &request, None, ts),
            Event::QuestionReplied { request_id, answers, .. } => self.question_replied(sink, &request_id, &answers, ts),
            Event::QuestionRejected { request_id, .. } => self.request_closed(sink, &request_id, ts),
            Event::Other { kind, properties } => {
                // `session.next.*` events repeat what the classic events above
                // already carry; `session.diff` and `command.executed` are
                // derived from messages the DOM holds.
                if kind.starts_with("session.next.") || matches!(kind.as_str(), "session.diff" | "command.executed") {
                    return Ok(());
                }
                let attrs = vec![(attr::HARNESS, HARNESS.into()), (attr::TYPE, kind.into())];
                ins(sink, self.root, K::Ext, attrs, properties.to_string(), ts).map(drop)
            }
        }
    }

    fn idle(&mut self, sink: &dyn PatchSink, ts: i64) -> Result<()> {
        match self.active {
            Some(turn) => self.close_turn(sink, turn, None, ts),
            None => Ok(()),
        }
    }

    fn session_updated(&mut self, sink: &dyn PatchSink, info: &Json, ts: i64) -> Result<()> {
        let title = json::str_at(info, "/title");
        if title.is_some() && title != self.title.as_deref() {
            let title = title.unwrap_or_default();
            set(sink, self.root, attr::TITLE, title, ts)?;
            self.title = Some(title.to_owned());
        }
        let revert = info.get("revert").filter(|r| !r.is_null()).map(Json::to_string);
        if revert != self.revert {
            match &revert {
                Some(r) => set(sink, self.root, xattr::REVERT, r.as_str(), ts)?,
                None => {
                    sink.commit_at(Draft::clear(self.root, xattr::REVERT), ts)?;
                }
            }
            self.revert = revert;
        }
        let oc = json::remainder_text(&json::without(info, SESSION_FIELDS));
        if oc != self.session_oc {
            set(sink, self.root, xattr::OC, oc.as_str(), ts)?;
            self.session_oc = oc;
        }
        Ok(())
    }

    fn session_error(&mut self, sink: &dyn PatchSink, error: Option<Json>, ts: i64) -> Result<()> {
        let Some(error) = error else { return Ok(()) };
        let aborted = json::str_at(&error, "/name") == Some("MessageAbortedError");
        if let Some(st) = self.active.and_then(|t| self.turn_state.get_mut(&t)) {
            if aborted {
                st.aborted = true;
            } else {
                st.error = true;
            }
        }
        if aborted {
            return Ok(());
        }
        let parent = self.active.unwrap_or(self.root);
        let attrs = vec![(attr::HARNESS, HARNESS.into()), (attr::TYPE, "session.error".into())];
        ins(sink, parent, K::Ext, attrs, error.to_string(), ts).map(drop)
    }

    /// A user or assistant message was created or changed.
    pub(crate) fn message_updated(&mut self, sink: &dyn PatchSink, info: &Json, ts: i64) -> Result<()> {
        let id = json::str_at(info, "/id").ok_or_else(|| AdapterError::Protocol("message without id".into()))?;
        if json::str_at(info, "/role") == Some("user") {
            self.user_message(sink, id, info, ts)
        } else {
            self.assistant_message(sink, id, info, ts)
        }
    }

    fn user_message(&mut self, sink: &dyn PatchSink, id: &str, info: &Json, ts: i64) -> Result<()> {
        let oc = json::remainder_text(&json::without(info, USER_FIELDS));
        if let Some(msg) = self.messages.get_mut(id) {
            if msg.oc != oc {
                set(sink, msg.turn, xattr::OC, oc.as_str(), ts)?;
                msg.oc = oc;
            }
            return Ok(());
        }
        let turn = match self.turns.get(id) {
            Some(turn) => {
                let turn = *turn;
                // The app stamped the turn when it sent the prompt; the
                // harness's own time wins so exports match the native file.
                if let Some(created) = json::int_at(info, "/time/created") {
                    set(sink, turn, attr::STARTED, created, ts)?;
                }
                turn
            }
            None => {
                // A prompt the app didn't send: the TUI, a command, compaction.
                if let Some(open) = self.active {
                    let last = self.turn_state.get(&open).map_or(ts, |s| s.last_ts);
                    self.close_turn(sink, open, None, last)?;
                }
                let started = json::int_at(info, "/time/created").unwrap_or(ts);
                let attrs = vec![
                    (attr::ID, id.into()),
                    (attr::STATUS, TurnStatus::Running.into()),
                    (attr::STARTED, started.into()),
                ];
                let turn = ins(sink, self.root, K::Turn, attrs, "", started)?;
                self.open_turn(id, turn, started);
                turn
            }
        };
        if !oc.is_empty() {
            set(sink, turn, xattr::OC, oc.as_str(), ts)?;
        }
        if let (Some(provider), Some(model)) = (json::str_at(info, "/model/providerID"), json::str_at(info, "/model/modelID"))
            && let Some(st) = self.turn_state.get_mut(&turn)
        {
            st.model = Some(model_name(provider, model));
        }
        self.messages.insert(
            id.to_owned(),
            Msg { turn, user: true, ext: None, oc, model: None, summary: false, compacted: false },
        );
        Ok(())
    }

    fn assistant_message(&mut self, sink: &dyn PatchSink, id: &str, info: &Json, ts: i64) -> Result<()> {
        let model = match (json::str_at(info, "/providerID"), json::str_at(info, "/modelID")) {
            (Some(p), Some(m)) => Some(model_name(p, m)),
            _ => None,
        };
        if model.is_some() {
            self.model.clone_from(&model);
        }
        let text = json::remainder_text(&json::without(info, ASSISTANT_FIELDS));
        let summary = json::bool_at(info, "/summary") == Some(true);
        let turn = match self.messages.get_mut(id) {
            Some(msg) => {
                if msg.oc != text
                    && let Some(ext) = msg.ext
                {
                    sink.commit_at(Draft::set_text(ext, text.as_str()), ts)?;
                    msg.oc = text;
                }
                msg.summary = summary;
                if model.is_some() {
                    msg.model = model;
                }
                msg.turn
            }
            None => {
                let parent = json::str_at(info, "/parentID").and_then(|p| self.turns.get(p)).copied();
                let turn = match parent.or(self.active) {
                    Some(turn) => turn,
                    None => {
                        // An assistant message with no prompt in view (the
                        // user message was removed or predates a fork).
                        let attrs = vec![(attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, ts.into())];
                        let turn = ins(sink, self.root, K::Turn, attrs, "", ts)?;
                        self.turn_seq.push(turn);
                        self.turn_state.insert(turn, TurnState { open: true, last_ts: ts, ..Default::default() });
                        self.active = Some(turn);
                        turn
                    }
                };
                let attrs = vec![
                    (attr::HARNESS, HARNESS.into()),
                    (attr::TYPE, ext::MESSAGE.into()),
                    (xattr::MESSAGE, id.into()),
                ];
                let created = json::int_at(info, "/time/created").unwrap_or(ts);
                let ext_node = ins(sink, turn, K::Ext, attrs, text.as_str(), created)?;
                self.messages.insert(
                    id.to_owned(),
                    Msg { turn, user: false, ext: Some(ext_node), oc: text, model, summary, compacted: false },
                );
                turn
            }
        };
        if let Some(st) = self.turn_state.get_mut(&turn) {
            if let Some(model) = self.messages.get(id).and_then(|m| m.model.clone()) {
                st.model = Some(model);
            }
            if let Some(done) = json::int_at(info, "/time/completed") {
                st.last_ts = st.last_ts.max(done);
            }
            match json::str_at(info, "/error/name") {
                Some("MessageAbortedError") => st.aborted = true,
                Some(_) => st.error = true,
                None => {}
            }
        }
        if summary && json::int_at(info, "/time/completed").is_some() {
            self.record_compaction(sink, id, turn, ts)?;
        }
        Ok(())
    }

    /// Adds `<compaction>` for a finished summary message, covering the turns
    /// since the previous compaction.
    fn record_compaction(&mut self, sink: &dyn PatchSink, message_id: &str, turn: NodeId, ts: i64) -> Result<()> {
        let Some(msg) = self.messages.get_mut(message_id) else { return Ok(()) };
        if msg.compacted {
            return Ok(());
        }
        msg.compacted = true;
        let model = msg.model.clone();
        let summary: String = {
            let mut texts: Vec<(NodeId, &str)> = self
                .parts
                .values()
                .filter(|p| p.message == message_id)
                .filter_map(|p| match &p.kind {
                    PartKind::Text { node, keep: Some(text), .. } => Some((*node, text.as_str())),
                    _ => None,
                })
                .collect();
            texts.sort_by_key(|(node, _)| *node);
            texts.into_iter().map(|(_, t)| t).collect::<Vec<_>>().join("\n\n")
        };
        let at = self.turn_seq.iter().position(|t| *t == turn).unwrap_or(self.turn_seq.len());
        let covers: Vec<Value> =
            self.turn_seq[self.covered.min(at)..at].iter().map(|t| Value::Int(i64::try_from(t.0).unwrap_or(i64::MAX))).collect();
        self.covered = at;
        let mut attrs = vec![(attr::COVERS, Value::List(covers)), (attr::CREATED, ts.into())];
        if let Some(model) = model {
            attrs.push((attr::MODEL, model.into()));
        }
        ins(sink, self.root, K::Compaction, attrs, summary, ts).map(drop)
    }

    fn message_removed(&mut self, sink: &dyn PatchSink, message_id: &str, ts: i64) -> Result<()> {
        let Some(msg) = self.messages.remove(message_id) else { return Ok(()) };
        if msg.user {
            // Reverting drops the whole exchange: the turn and all it holds.
            sink.commit_at(Draft::remove(msg.turn), ts)?;
            self.turns.remove(message_id);
            self.turn_seq.retain(|t| *t != msg.turn);
            self.covered = self.covered.min(self.turn_seq.len());
            self.turn_state.remove(&msg.turn);
            self.messages.retain(|_, m| m.turn != msg.turn);
            self.parts.retain(|_, p| p.turn != msg.turn);
            self.calls.retain(|_, (_, turn)| *turn != msg.turn);
            self.decisions.retain(|_, d| d.turn != Some(msg.turn));
            if self.active == Some(msg.turn) {
                self.active = None;
            }
            return Ok(());
        }
        let mut doomed: Vec<NodeId> = msg.ext.into_iter().collect();
        let part_ids: Vec<String> =
            self.parts.iter().filter(|(_, p)| p.message == message_id).map(|(id, _)| id.clone()).collect();
        for id in part_ids {
            if let Some(part) = self.parts.remove(&id) {
                doomed.push(part.kind.node());
            }
        }
        doomed.sort();
        for node in doomed {
            sink.commit_at(Draft::remove(node), ts)?;
        }
        Ok(())
    }

    fn part_removed(&mut self, sink: &dyn PatchSink, part_id: &str, ts: i64) -> Result<()> {
        if let Some(part) = self.parts.remove(part_id) {
            sink.commit_at(Draft::remove(part.kind.node()), ts)?;
        }
        Ok(())
    }

    fn part_delta(&mut self, sink: &dyn PatchSink, delta: &PartDelta, ts: i64) -> Result<()> {
        if delta.field != "text" {
            return Ok(());
        }
        let Some(PartEntry { kind: PartKind::Text { node, text, keep, .. }, .. }) = self.parts.get_mut(&delta.part_id)
        else {
            return Ok(());
        };
        sink.commit_at(Draft::append(*node, delta.delta.as_str()), ts)?;
        text.push(&delta.delta);
        if let Some(keep) = keep {
            keep.push_str(&delta.delta);
        }
        Ok(())
    }

    /// A part was created or changed (always the full part).
    pub(crate) fn part_updated(&mut self, sink: &dyn PatchSink, part: &Json, ts: i64) -> Result<()> {
        let id = json::str_at(part, "/id").ok_or_else(|| AdapterError::Protocol("part without id".into()))?;
        let message_id =
            json::str_at(part, "/messageID").ok_or_else(|| AdapterError::Protocol(format!("part {id} has no message")))?;
        let Some(msg) = self.messages.get(message_id) else {
            return Err(AdapterError::Protocol(format!("part {id} of unknown message {message_id}")));
        };
        let (turn, user, summary, model) = (msg.turn, msg.user, msg.summary, msg.model.clone());
        let synthetic = json::bool_at(part, "/synthetic") == Some(true);
        match json::str_at(part, "/type") {
            Some("text") if !synthetic => {
                self.text_part(sink, id, message_id, part, turn, user, false, summary, model, ts)
            }
            Some("reasoning") => self.text_part(sink, id, message_id, part, turn, user, true, summary, model, ts),
            Some("tool") => self.tool_part(sink, id, message_id, part, turn, ts),
            _ => self.ext_part(sink, id, message_id, part, turn, ts),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn text_part(
        &mut self,
        sink: &dyn PatchSink,
        id: &str,
        message_id: &str,
        part: &Json,
        turn: NodeId,
        user: bool,
        thinking: bool,
        summary: bool,
        model: Option<String>,
        ts: i64,
    ) -> Result<()> {
        let full = json::str_at(part, "/text").unwrap_or_default();
        let oc = json::remainder_text(&json::without(part, TEXT_FIELDS));
        let finished = user || json::int_at(part, "/time/end").is_some();
        let status = if finished { MessageStatus::Done } else { MessageStatus::Streaming };
        let at = json::int_at(part, "/time/start").unwrap_or(ts);
        if let Some(entry) = self.parts.get_mut(id) {
            let PartKind::Text { node, text, status: current, keep } = &mut entry.kind else {
                return Ok(());
            };
            sync_text(sink, *node, text, full, ts)?;
            if let Some(keep) = keep
                && keep != full
            {
                full.clone_into(keep);
            }
            if entry.oc != oc {
                set(sink, *node, xattr::OC, oc.as_str(), ts)?;
                entry.oc = oc;
            }
            if *current == MessageStatus::Streaming && status == MessageStatus::Done {
                set(sink, *node, attr::STATUS, status, json::int_at(part, "/time/end").unwrap_or(ts))?;
                *current = status;
            }
            return Ok(());
        }
        let bound = match &self.bind {
            Some((mid, _, _)) if user && !thinking && mid == message_id => self.bind.take(),
            _ => None,
        };
        let (node, tracked) = match bound {
            Some((_, node, mut tracked)) => {
                set(sink, node, attr::ID, id, ts)?;
                if !oc.is_empty() {
                    set(sink, node, xattr::OC, oc.as_str(), ts)?;
                }
                sync_text(sink, node, &mut tracked, full, ts)?;
                (node, tracked)
            }
            None => {
                let role = if user { Role::User } else { Role::Assistant };
                let mut attrs: Vec<(&str, Value)> = vec![
                    (attr::ROLE, role.into()),
                    (attr::ID, id.into()),
                    (attr::STATUS, status.into()),
                    (xattr::MESSAGE, message_id.into()),
                ];
                if thinking {
                    attrs.push((attr::KIND, MessageKind::Thinking.into()));
                }
                if let Some(model) = model.filter(|_| !user) {
                    attrs.push((attr::MODEL, model.into()));
                }
                if !oc.is_empty() {
                    attrs.push((xattr::OC, oc.as_str().into()));
                }
                (ins(sink, turn, K::Message, attrs, full, at)?, Tracked::of(full))
            }
        };
        let keep = summary.then(|| full.to_owned());
        self.parts.insert(
            id.to_owned(),
            PartEntry {
                turn,
                message: message_id.to_owned(),
                oc,
                kind: PartKind::Text { node, text: tracked, status, keep },
            },
        );
        Ok(())
    }

    fn tool_part(
        &mut self,
        sink: &dyn PatchSink,
        id: &str,
        message_id: &str,
        part: &Json,
        turn: NodeId,
        ts: i64,
    ) -> Result<()> {
        let state = part.get("state").cloned().unwrap_or(Json::Null);
        let native_status = json::str_at(&state, "/status").unwrap_or("pending");
        let status = call_status(native_status);
        let tool = json::str_at(part, "/tool").unwrap_or_default();
        let call_id = json::str_at(part, "/callID").unwrap_or_default();
        let title = json::str_at(&state, "/title");
        let started = json::int_at(&state, "/time/start");
        let ended = json::int_at(&state, "/time/end");
        let input = input_text(&state);
        let oc = json::remainder_text(&tool_remainder(part));
        let output = match native_status {
            "completed" => Some((json::str_at(&state, "/output").unwrap_or_default(), false)),
            "error" => Some((json::str_at(&state, "/error").unwrap_or_default(), true)),
            _ => None,
        };
        if !self.parts.contains_key(id) {
            let at = started.unwrap_or(ts);
            let mut attrs: Vec<(&str, Value)> = vec![
                (attr::TOOL, tool.into()),
                (attr::CALL_ID, call_id.into()),
                (attr::STATUS, status.into()),
                (attr::STARTED, at.into()),
                (xattr::PART, id.into()),
                (xattr::MESSAGE, message_id.into()),
            ];
            if let Some(title) = title {
                attrs.push((attr::TITLE, title.into()));
            }
            if let Some(ended) = ended {
                attrs.push((attr::ENDED, ended.into()));
            }
            if !oc.is_empty() {
                attrs.push((xattr::OC, oc.as_str().into()));
            }
            let node = ins(sink, turn, K::Call, attrs, "", at)?;
            let input_node = ins(sink, node, K::Input, vec![], input.as_str(), at)?;
            let result = match output {
                Some((text, is_error)) => {
                    let at = ended.unwrap_or(at);
                    let attrs = if is_error { vec![(attr::IS_ERROR, Value::Bool(true))] } else { vec![] };
                    Some((ins(sink, node, K::Result, attrs, text, at)?, Tracked::of(text)))
                }
                None => None,
            };
            self.calls.insert(call_id.to_owned(), (node, turn));
            self.parts.insert(
                id.to_owned(),
                PartEntry {
                    turn,
                    message: message_id.to_owned(),
                    oc,
                    kind: PartKind::Call {
                        node,
                        input: input_node,
                        input_text: Tracked::of(&input),
                        result,
                        status,
                        title: title.map(str::to_owned),
                        started: Some(at),
                        ended,
                        job: None,
                    },
                },
            );
        } else if let Some(entry) = self.parts.get_mut(id) {
            let PartKind::Call {
                node,
                input: input_node,
                input_text,
                result,
                status: current,
                title: current_title,
                started: current_started,
                ended: current_ended,
                ..
            } = &mut entry.kind
            else {
                return Ok(());
            };
            let node = *node;
            sync_text(sink, *input_node, input_text, &input, ts)?;
            if title.is_some() && title != current_title.as_deref() {
                let title = title.unwrap_or_default();
                set(sink, node, attr::TITLE, title, ts)?;
                *current_title = Some(title.to_owned());
            }
            if started.is_some() && started != *current_started {
                set(sink, node, attr::STARTED, started.unwrap_or_default(), ts)?;
                *current_started = started;
            }
            if let Some((text, is_error)) = output {
                match result {
                    Some((result_node, tracked)) => sync_text(sink, *result_node, tracked, text, ts)?,
                    None => {
                        let attrs = if is_error { vec![(attr::IS_ERROR, Value::Bool(true))] } else { vec![] };
                        *result = Some((ins(sink, node, K::Result, attrs, text, ended.unwrap_or(ts))?, Tracked::of(text)));
                    }
                }
            }
            if ended.is_some() && ended != *current_ended {
                set(sink, node, attr::ENDED, ended.unwrap_or_default(), ts)?;
                *current_ended = ended;
            }
            // A cancelled call stays cancelled: the interrupt decided it.
            if status != *current && *current != CallStatus::Cancelled {
                set(sink, node, attr::STATUS, status, ts)?;
                *current = status;
            }
            if entry.oc != oc {
                set(sink, node, xattr::OC, oc.as_str(), ts)?;
                entry.oc = oc;
            }
        }
        match tool {
            "task" => self.task_job(sink, id, turn, &state, ts)?,
            "todowrite" if native_status == "completed" => {
                if let Some(todos) = state.pointer("/input/todos")
                    && let Ok(todos) = serde_json::from_value::<Vec<Todo>>(todos.clone())
                {
                    self.sync_todos(sink, &todos, ended.unwrap_or(ts))?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Keeps the `<job kind="subagent">` of a task tool call in step with it.
    fn task_job(&mut self, sink: &dyn PatchSink, part_id: &str, turn: NodeId, state: &Json, ts: i64) -> Result<()> {
        let Some(PartEntry { kind: PartKind::Call { node, job, status, started, ended, .. }, .. }) =
            self.parts.get_mut(part_id)
        else {
            return Ok(());
        };
        let (call, status, started, ended) = (*node, *status, *started, *ended);
        let job_node = match *job {
            Some(job) => job,
            None => {
                let Some(child) = json::str_at(state, "/metadata/sessionId") else { return Ok(()) };
                let mut attrs: Vec<(&str, Value)> = vec![
                    (attr::KIND, JobKind::Subagent.into()),
                    (attr::STATUS, JobStatus::Running.into()),
                    (attr::CALL, Value::Int(i64::try_from(call.0).unwrap_or(i64::MAX))),
                    (attr::STARTED, started.unwrap_or(ts).into()),
                    (xattr::NATIVE_SESSION, child.into()),
                ];
                let title = json::str_at(state, "/title").or_else(|| json::str_at(state, "/input/description"));
                if let Some(title) = title {
                    attrs.push((attr::TITLE, title.into()));
                }
                if let Some(agent) = json::str_at(state, "/input/subagent_type") {
                    attrs.push((attr::AGENT, agent.into()));
                }
                let entry = self.children.entry(child.to_owned()).or_default();
                if let Some(mirror) = &entry.mirror {
                    attrs.push((attr::SESSION, mirror.session_id.as_str().into()));
                }
                let job_node = ins(sink, call, K::Job, attrs, "", started.unwrap_or(ts))?;
                entry.job = Some(job_node);
                entry.call = Some(call);
                entry.turn = Some(turn);
                if let Some(PartEntry { kind: PartKind::Call { job, .. }, .. }) = self.parts.get_mut(part_id) {
                    *job = Some(job_node);
                }
                job_node
            }
        };
        let job_status = match status {
            CallStatus::Done => JobStatus::Done,
            CallStatus::Error => JobStatus::Error,
            CallStatus::Cancelled => JobStatus::Cancelled,
            CallStatus::Pending | CallStatus::Running => return Ok(()),
        };
        if let Some(child) = self.children.values_mut().find(|c| c.job == Some(job_node) && c.turn.is_some()) {
            // `turn` is set while the job runs; it routes the child's decisions.
            child.turn = None;
            set(sink, job_node, attr::STATUS, job_status, ts)?;
            set(sink, job_node, attr::ENDED, ended.unwrap_or(ts), ts)?;
        }
        Ok(())
    }

    fn ext_part(
        &mut self,
        sink: &dyn PatchSink,
        id: &str,
        message_id: &str,
        part: &Json,
        turn: NodeId,
        ts: i64,
    ) -> Result<()> {
        let text = json::without(part, EXT_FIELDS).to_string();
        if json::str_at(part, "/type") == Some("step-finish")
            && let Some(st) = self.turn_state.get_mut(&turn)
        {
            let n = |ptr| json::int_at(part, ptr).unwrap_or(0);
            st.steps.insert(
                id.to_owned(),
                Usage {
                    input: n("/tokens/input"),
                    output: n("/tokens/output"),
                    reasoning: n("/tokens/reasoning"),
                    cache_read: n("/tokens/cache/read"),
                    cache_write: n("/tokens/cache/write"),
                    cost: json::float_at(part, "/cost").unwrap_or(0.0),
                },
            );
        }
        if let Some(entry) = self.parts.get_mut(id) {
            if let PartKind::Ext { node, text: current } = &mut entry.kind
                && *current != text
            {
                sink.commit_at(Draft::set_text(*node, text.as_str()), ts)?;
                *current = text;
            }
            return Ok(());
        }
        let attrs = vec![
            (attr::HARNESS, HARNESS.into()),
            (attr::TYPE, ext::PART.into()),
            (xattr::PART, id.into()),
            (xattr::MESSAGE, message_id.into()),
        ];
        let node = ins(sink, turn, K::Ext, attrs, text.as_str(), ts)?;
        self.parts.insert(
            id.to_owned(),
            PartEntry { turn, message: message_id.to_owned(), oc: String::new(), kind: PartKind::Ext { node, text } },
        );
        Ok(())
    }

    /// Ends a turn. `forced` is the interrupt or a failed prompt; `None`
    /// derives the status from what the harness reported.
    fn close_turn(&mut self, sink: &dyn PatchSink, turn: NodeId, forced: Option<TurnStatus>, ts: i64) -> Result<()> {
        let Some(st) = self.turn_state.get_mut(&turn) else { return Ok(()) };
        if !st.open {
            return Ok(());
        }
        st.open = false;
        let status = forced.unwrap_or(if st.error {
            TurnStatus::Error
        } else if st.aborted {
            TurnStatus::Cancelled
        } else {
            TurnStatus::Done
        });
        let total = st.steps.values().fold(None::<Usage>, |sum, u| {
            let mut sum = sum.unwrap_or_default();
            sum.input += u.input;
            sum.output += u.output;
            sum.reasoning += u.reasoning;
            sum.cache_read += u.cache_read;
            sum.cache_write += u.cache_write;
            sum.cost += u.cost;
            Some(sum)
        });
        let model = st.model.clone();
        let cancelled = status == TurnStatus::Cancelled;
        let mut ids: Vec<&String> = self.parts.iter().filter(|(_, p)| p.turn == turn).map(|(id, _)| id).collect();
        ids.sort_by_key(|id| self.parts[*id].kind.node());
        let ids: Vec<String> = ids.into_iter().cloned().collect();
        for id in ids {
            let Some(entry) = self.parts.get_mut(&id) else { continue };
            match &mut entry.kind {
                PartKind::Text { node, status: current, .. } if *current == MessageStatus::Streaming => {
                    let next = if cancelled { MessageStatus::Cancelled } else { MessageStatus::Done };
                    set(sink, *node, attr::STATUS, next, ts)?;
                    *current = next;
                }
                PartKind::Call { node, status: current, ended, job, .. }
                    if matches!(*current, CallStatus::Pending | CallStatus::Running) =>
                {
                    set(sink, *node, attr::STATUS, CallStatus::Cancelled, ts)?;
                    *current = CallStatus::Cancelled;
                    if ended.is_none() {
                        set(sink, *node, attr::ENDED, ts, ts)?;
                        *ended = Some(ts);
                    }
                    if let Some(job) = *job
                        && let Some(child) = self.children.values_mut().find(|c| c.job == Some(job) && c.turn.is_some())
                    {
                        child.turn = None;
                        set(sink, job, attr::STATUS, JobStatus::Cancelled, ts)?;
                        set(sink, job, attr::ENDED, ts, ts)?;
                    }
                }
                _ => {}
            }
        }
        for (node, pending) in self.decisions.iter_mut().filter(|(_, d)| d.waiting && d.turn == Some(turn)) {
            set(sink, *node, attr::STATUS, DecisionStatus::Cancelled, ts)?;
            set(sink, *node, attr::ENDED, ts, ts)?;
            pending.waiting = false;
        }
        set(sink, turn, attr::STATUS, status, ts)?;
        set(sink, turn, attr::ENDED, ts, ts)?;
        if let Some(u) = total {
            let mut attrs: Vec<(&str, Value)> = vec![
                (attr::INPUT_TOKENS, u.input.into()),
                (attr::OUTPUT_TOKENS, u.output.into()),
                (attr::CACHE_READ_TOKENS, u.cache_read.into()),
                (attr::CACHE_WRITE_TOKENS, u.cache_write.into()),
                (attr::COST, u.cost.into()),
                (xattr::REASONING_TOKENS, u.reasoning.into()),
            ];
            if let Some(model) = model {
                attrs.push((attr::MODEL, model.into()));
            }
            ins(sink, turn, K::Usage, attrs, "", ts)?;
        }
        if self.active == Some(turn) {
            self.active = None;
        }
        if self.bind.as_ref().is_some_and(|b| self.turns.get(&b.0) == Some(&turn)) {
            self.bind = None;
        }
        Ok(())
    }

    /// Replaces the todo list: changed entries get `set`, new ones `insert`,
    /// dropped ones `remove`.
    fn sync_todos(&mut self, sink: &dyn PatchSink, todos: &[Todo], ts: i64) -> Result<()> {
        for (i, todo) in todos.iter().enumerate() {
            let state = todo_state(&todo.status);
            match self.todos.get_mut(i) {
                Some(current) => {
                    let node = current.node;
                    if current.todo.content != todo.content {
                        sink.commit_at(Draft::set_text(node, todo.content.as_str()), ts)?;
                    }
                    if current.todo.status != todo.status {
                        set(sink, node, attr::STATE, state.unwrap_or(TodoState::Pending), ts)?;
                        match state {
                            Some(_) => {
                                if todo_state(&current.todo.status).is_none() {
                                    sink.commit_at(Draft::clear(node, xattr::STATUS), ts)?;
                                }
                            }
                            None => set(sink, node, xattr::STATUS, todo.status.as_str(), ts)?,
                        }
                    }
                    if current.todo.priority != todo.priority {
                        set(sink, node, xattr::PRIORITY, todo.priority.as_str(), ts)?;
                    }
                    current.todo = todo.clone();
                }
                None => {
                    let mut attrs: Vec<(&str, Value)> = vec![(attr::STATE, state.unwrap_or(TodoState::Pending).into())];
                    if state.is_none() {
                        attrs.push((xattr::STATUS, todo.status.as_str().into()));
                    }
                    if !todo.priority.is_empty() {
                        attrs.push((xattr::PRIORITY, todo.priority.as_str().into()));
                    }
                    let node = ins(sink, self.root, K::Todo, attrs, todo.content.as_str(), ts)?;
                    self.todos.push(TodoNode { node, todo: todo.clone() });
                }
            }
        }
        for gone in self.todos.drain(todos.len().min(self.todos.len())..) {
            sink.commit_at(Draft::remove(gone.node), ts)?;
        }
        Ok(())
    }

    /// Where a request about `tool` (of this session or of child `child`) goes:
    /// (parent node, `<call>` it is about, turn it blocks).
    fn request_place(&self, tool: Option<&ToolRef>, child: Option<&str>) -> (NodeId, Option<NodeId>, Option<NodeId>) {
        if let Some(c) = child.and_then(|c| self.children.get(c))
            && let (Some(call), Some(turn)) = (c.call, c.turn)
        {
            return (call, Some(call), Some(turn));
        }
        if let Some((call, turn)) = tool.and_then(|t| self.calls.get(&t.call_id)) {
            return (*call, Some(*call), Some(*turn));
        }
        match self.active {
            Some(turn) => (turn, None, Some(turn)),
            None => (self.root, None, None),
        }
    }

    fn set_turn_waiting(&self, sink: &dyn PatchSink, turn: Option<NodeId>, ts: i64) -> Result<()> {
        match turn {
            Some(turn) if self.turn_state.get(&turn).is_some_and(|s| s.open) => {
                set(sink, turn, attr::STATUS, TurnStatus::Waiting, ts)
            }
            _ => Ok(()),
        }
    }

    fn permission_asked(
        &mut self,
        sink: &dyn PatchSink,
        request: &PermissionRequest,
        child: Option<&str>,
        ts: i64,
    ) -> Result<()> {
        if self.decisions.values().any(|d| d.request_id == request.id) {
            return Ok(());
        }
        let (parent, call, turn) = self.request_place(request.tool.as_ref(), child);
        let title = if request.patterns.is_empty() {
            request.permission.clone()
        } else {
            format!("{}: {}", request.permission, request.patterns.join(", "))
        };
        let body = ["/command", "/diff", "/filepath"]
            .iter()
            .find_map(|p| json::str_at(&request.metadata, p))
            .map_or_else(|| request.patterns.join("\n"), str::to_owned);
        let oc = serde_json::json!({
            "permission": request.permission,
            "patterns": request.patterns,
            "always": request.always,
            "metadata": request.metadata,
        });
        let mut attrs: Vec<(&str, Value)> = vec![
            (attr::KIND, DecisionKind::Approval.into()),
            (attr::STATUS, DecisionStatus::Waiting.into()),
            (attr::REQUEST_ID, request.id.as_str().into()),
            (attr::TITLE, title.into()),
            (attr::CREATED, ts.into()),
            (xattr::OC, oc.to_string().into()),
        ];
        if let Some(call) = call {
            attrs.push((attr::CALL, Value::Int(i64::try_from(call.0).unwrap_or(i64::MAX))));
        }
        let node = ins(sink, parent, K::Decision, attrs, body, ts)?;
        self.decisions.insert(
            node,
            Pending { request_id: request.id.clone(), kind: RequestKind::Permission, turn, waiting: true, answer: None },
        );
        self.set_turn_waiting(sink, turn, ts)
    }

    fn question_asked(
        &mut self,
        sink: &dyn PatchSink,
        request: &QuestionRequest,
        child: Option<&str>,
        ts: i64,
    ) -> Result<()> {
        if self.decisions.values().any(|d| d.request_id == request.id) {
            return Ok(());
        }
        let (parent, call, turn) = self.request_place(request.tool.as_ref(), child);
        let count = request.questions.len();
        for (index, q) in request.questions.iter().enumerate() {
            let options: Vec<Value> = q.options.iter().map(|o| Value::Str(o.label.clone())).collect();
            let mut attrs: Vec<(&str, Value)> = vec![
                (attr::KIND, DecisionKind::Question.into()),
                (attr::STATUS, DecisionStatus::Waiting.into()),
                (attr::REQUEST_ID, request.id.as_str().into()),
                (attr::OPTIONS, Value::List(options)),
                (attr::MULTI, q.multiple.unwrap_or(false).into()),
                (attr::CREATED, ts.into()),
                (xattr::INDEX, i64::try_from(index).unwrap_or(i64::MAX).into()),
            ];
            if !q.header.is_empty() {
                attrs.push((attr::TITLE, q.header.as_str().into()));
            }
            if let Some(custom) = q.custom {
                attrs.push((xattr::CUSTOM, custom.into()));
            }
            if let Some(call) = call {
                attrs.push((attr::CALL, Value::Int(i64::try_from(call.0).unwrap_or(i64::MAX))));
            }
            let node = ins(sink, parent, K::Decision, attrs, q.question.as_str(), ts)?;
            self.decisions.insert(
                node,
                Pending {
                    request_id: request.id.clone(),
                    kind: RequestKind::Question { index, count },
                    turn,
                    waiting: true,
                    answer: None,
                },
            );
        }
        self.set_turn_waiting(sink, turn, ts)
    }

    /// What to send OpenCode for `answer` on `decision`, checked against the
    /// decision's kind. Doesn't change the DOM.
    pub fn plan_reply(&self, decision: NodeId, answer: &Answer) -> Result<Reply> {
        let pending = self.decisions.get(&decision).filter(|d| d.waiting).ok_or(AdapterError::UnknownDecision(decision))?;
        let request_id = pending.request_id.clone();
        match pending.kind {
            RequestKind::Permission => {
                let reply = match answer.values.first().map(String::as_str) {
                    Some(answer::ALLOW_ONCE) => "once",
                    Some(answer::ALLOW_SESSION) => "always",
                    Some(answer::DENY) => "reject",
                    other => {
                        return Err(AdapterError::Protocol(format!(
                            "an approval takes allow_once, allow_session or deny, not {other:?}"
                        )));
                    }
                };
                Ok(Reply::Permission { request_id, reply, message: answer.reason.clone().filter(|_| reply == "reject") })
            }
            RequestKind::Question { index, count } => {
                if answer.values.is_empty() {
                    return Ok(Reply::RejectQuestion { request_id });
                }
                let mut answers = vec![Vec::new(); count];
                for d in self.decisions.values().filter(|d| d.request_id == request_id) {
                    if let RequestKind::Question { index: i, .. } = d.kind
                        && let Some(slot) = answers.get_mut(i)
                    {
                        match (&d.answer, i == index) {
                            (_, true) => slot.clone_from(&answer.values),
                            (Some(a), false) => slot.clone_from(a),
                            (None, false) => return Ok(Reply::Later),
                        }
                    }
                }
                Ok(Reply::Question { request_id, answers })
            }
        }
    }

    /// Records `answer` on `decision`; the turn runs again once nothing waits.
    pub fn record_answer(&mut self, sink: &dyn PatchSink, decision: NodeId, answer: &Answer, ts: i64) -> Result<()> {
        let pending = self.decisions.get_mut(&decision).filter(|d| d.waiting).ok_or(AdapterError::UnknownDecision(decision))?;
        let rejected = answer.values.is_empty() && matches!(pending.kind, RequestKind::Question { .. });
        pending.waiting = false;
        pending.answer = Some(answer.values.clone());
        let (turn, request_id) = (pending.turn, pending.request_id.clone());
        if rejected {
            // Dismissing one question rejects the whole request.
            set(sink, decision, attr::STATUS, DecisionStatus::Cancelled, ts)?;
            if !answer.by.is_empty() {
                set(sink, decision, attr::BY, answer.by.as_str(), ts)?;
            }
            set(sink, decision, attr::ENDED, ts, ts)?;
            return self.request_closed(sink, &request_id, ts);
        }
        set(sink, decision, attr::ANSWER, answer.values.clone(), ts)?;
        if !answer.by.is_empty() {
            set(sink, decision, attr::BY, answer.by.as_str(), ts)?;
        }
        if let Some(reason) = &answer.reason {
            set(sink, decision, attr::REASON, reason.as_str(), ts)?;
        }
        set(sink, decision, attr::STATUS, DecisionStatus::Answered, ts)?;
        set(sink, decision, attr::ENDED, ts, ts)?;
        self.resume_turn(sink, turn, ts)
    }

    fn resume_turn(&self, sink: &dyn PatchSink, turn: Option<NodeId>, ts: i64) -> Result<()> {
        let Some(turn) = turn else { return Ok(()) };
        let still_waiting = self.decisions.values().any(|d| d.waiting && d.turn == Some(turn));
        if !still_waiting && self.turn_state.get(&turn).is_some_and(|s| s.open) {
            set(sink, turn, attr::STATUS, TurnStatus::Running, ts)?;
        }
        Ok(())
    }

    /// A reply made outside the app (another client) for a permission.
    fn permission_replied(&mut self, sink: &dyn PatchSink, request_id: &str, reply: &str, ts: i64) -> Result<()> {
        let value = match reply {
            "once" => answer::ALLOW_ONCE,
            "always" => answer::ALLOW_SESSION,
            _ => answer::DENY,
        };
        let nodes: Vec<NodeId> =
            self.decisions.iter().filter(|(_, d)| d.waiting && d.request_id == request_id).map(|(n, _)| *n).collect();
        for node in nodes {
            let answer = Answer { values: vec![value.to_owned()], reason: None, by: String::new() };
            self.record_answer(sink, node, &answer, ts)?;
        }
        Ok(())
    }

    fn question_replied(&mut self, sink: &dyn PatchSink, request_id: &str, answers: &[Vec<String>], ts: i64) -> Result<()> {
        let nodes: Vec<(NodeId, usize)> = self
            .decisions
            .iter()
            .filter(|(_, d)| d.waiting && d.request_id == request_id)
            .filter_map(|(n, d)| match d.kind {
                RequestKind::Question { index, .. } => Some((*n, index)),
                RequestKind::Permission => None,
            })
            .collect();
        for (node, index) in nodes {
            let values = answers.get(index).cloned().unwrap_or_default();
            if values.is_empty() {
                continue;
            }
            self.record_answer(sink, node, &Answer { values, reason: None, by: String::new() }, ts)?;
        }
        Ok(())
    }

    /// The request is gone without an answer: its waiting decisions are cancelled.
    fn request_closed(&mut self, sink: &dyn PatchSink, request_id: &str, ts: i64) -> Result<()> {
        let mut turns = Vec::new();
        for (node, pending) in self.decisions.iter_mut().filter(|(_, d)| d.waiting && d.request_id == request_id) {
            set(sink, *node, attr::STATUS, DecisionStatus::Cancelled, ts)?;
            set(sink, *node, attr::ENDED, ts, ts)?;
            pending.waiting = false;
            turns.push(pending.turn);
        }
        for turn in turns {
            self.resume_turn(sink, turn, ts)?;
        }
        Ok(())
    }

    fn register_child(&mut self, sink: &dyn PatchSink, info: &Json, ts: i64) -> Result<()> {
        let Some(id) = json::str_at(info, "/id") else { return Ok(()) };
        if self.children.get(id).is_some_and(|c| c.mirror.is_some()) {
            return Ok(());
        }
        let mirror = match &self.child_port {
            Some(port) => match port.open_child(&self.session_id, id) {
                Some((session_id, sink)) => {
                    let root = RootInfo {
                        session_id: session_id.clone(),
                        node: self.node.clone(),
                        cwd: self.cwd.clone(),
                        native: info.clone(),
                        extra: vec![(xattr::PARENT_SESSION.to_owned(), Value::Str(self.session_id.clone()))],
                        ..Default::default()
                    };
                    let mapper = EventMapper::start(&*sink, root, ts)?.with_child_sessions(Some(Arc::clone(port)));
                    Some(Mirror { mapper: Box::new(mapper), sink, session_id })
                }
                None => None,
            },
            None => None,
        };
        let child = self.children.entry(id.to_owned()).or_default();
        if let (Some(m), Some(job)) = (&mirror, child.job) {
            // The task tool reported the child before its creation event.
            set(sink, job, attr::SESSION, m.session_id.as_str(), ts)?;
        }
        child.mirror = mirror;
        Ok(())
    }

    fn child_event(&mut self, sink: &dyn PatchSink, child: &str, event: Event, ts: i64) -> Result<()> {
        match event {
            Event::PermissionAsked(request) => self.permission_asked(sink, &request, Some(child), ts),
            Event::QuestionAsked(request) => self.question_asked(sink, &request, Some(child), ts),
            Event::PermissionReplied { request_id, reply, .. } => self.permission_replied(sink, &request_id, &reply, ts),
            Event::QuestionReplied { request_id, answers, .. } => self.question_replied(sink, &request_id, &answers, ts),
            Event::QuestionRejected { request_id, .. } => self.request_closed(sink, &request_id, ts),
            event => match self.children.get_mut(child).and_then(|c| c.mirror.as_mut()) {
                Some(mirror) => mirror.mapper.apply(&*mirror.sink, event, ts),
                None => Ok(()),
            },
        }
    }

    /// Rebuilds the mapper's view of a session from its DOM (resume, fork).
    pub fn from_dom(dom: &Dom) -> Result<Self> {
        let root = dom.root().ok_or_else(|| AdapterError::Format("the session DOM is empty".into()))?;
        let s = |n: &Node, a: &str| n.str_attr(a).map(str::to_owned);
        let mut m = EventMapper::empty(
            root.id,
            s(root, attr::ID).unwrap_or_default(),
            s(root, attr::NODE).unwrap_or_default(),
            s(root, attr::CWD),
            s(root, attr::NATIVE_ID).unwrap_or_default(),
            s(root, xattr::OC).unwrap_or_default(),
            s(root, attr::TITLE),
            s(root, xattr::REVERT),
            s(root, attr::MODEL),
        );
        for child in dom.children(root.id) {
            match child.kind {
                K::Turn => m.index_turn(dom, child),
                K::Todo => m.todos.push(TodoNode {
                    node: child.id,
                    todo: Todo {
                        content: child.text.clone(),
                        status: child.str_attr(xattr::STATUS).map_or_else(
                            || match child.str_attr(attr::STATE) {
                                Some("done") => "completed".to_owned(),
                                other => other.unwrap_or("pending").to_owned(),
                            },
                            str::to_owned,
                        ),
                        priority: child.str_attr(xattr::PRIORITY).unwrap_or_default().to_owned(),
                    },
                }),
                K::Compaction => {
                    let last = child
                        .attr(attr::COVERS)
                        .and_then(Value::as_list)
                        .and_then(|l| l.iter().filter_map(Value::as_int).max());
                    if let Some(at) = last.and_then(|n| m.turn_seq.iter().position(|t| i64::try_from(t.0).ok() == Some(n))) {
                        m.covered = at + 1;
                    }
                }
                _ => {}
            }
        }
        Ok(m)
    }

    fn index_turn(&mut self, dom: &Dom, turn: &Node) {
        self.turn_seq.push(turn.id);
        let open = matches!(turn.str_attr(attr::STATUS), Some("running" | "waiting"));
        self.turn_state.insert(turn.id, TurnState { open, last_ts: turn.int_attr(attr::STARTED).unwrap_or(0), ..Default::default() });
        if open {
            self.active = Some(turn.id);
        }
        if let Some(id) = turn.str_attr(attr::ID) {
            self.turns.insert(id.to_owned(), turn.id);
            let oc = turn.str_attr(xattr::OC).unwrap_or_default().to_owned();
            self.messages.insert(
                id.to_owned(),
                Msg { turn: turn.id, user: true, ext: None, oc, model: None, summary: false, compacted: true },
            );
        }
        for node in dom.children(turn.id) {
            let message = node.str_attr(xattr::MESSAGE).unwrap_or_default().to_owned();
            match node.kind {
                K::Ext if node.str_attr(attr::TYPE) == Some(ext::MESSAGE) => {
                    let info = json::parse_object(&node.text).unwrap_or(Json::Null);
                    let model = match (json::str_at(&info, "/providerID"), json::str_at(&info, "/modelID")) {
                        (Some(p), Some(mo)) => Some(model_name(p, mo)),
                        _ => None,
                    };
                    let summary = json::bool_at(&info, "/summary") == Some(true);
                    self.messages.insert(
                        message,
                        Msg { turn: turn.id, user: false, ext: Some(node.id), oc: node.text.clone(), model, summary, compacted: true },
                    );
                }
                K::Ext if node.str_attr(attr::TYPE) == Some(ext::PART) => {
                    if let Some(id) = node.str_attr(xattr::PART) {
                        self.parts.insert(
                            id.to_owned(),
                            PartEntry {
                                turn: turn.id,
                                message,
                                oc: String::new(),
                                kind: PartKind::Ext { node: node.id, text: node.text.clone() },
                            },
                        );
                    }
                }
                K::Message => {
                    let Some(id) = node.str_attr(attr::ID) else { continue };
                    let status = node.str_attr(attr::STATUS).and_then(MessageStatus::parse).unwrap_or(MessageStatus::Done);
                    self.parts.insert(
                        id.to_owned(),
                        PartEntry {
                            turn: turn.id,
                            message,
                            oc: node.str_attr(xattr::OC).unwrap_or_default().to_owned(),
                            kind: PartKind::Text { node: node.id, text: Tracked::of(&node.text), status, keep: None },
                        },
                    );
                }
                K::Call => {
                    let Some(id) = node.str_attr(xattr::PART) else { continue };
                    let input = dom.children_of_kind(node.id, K::Input).next();
                    let result = dom.children_of_kind(node.id, K::Result).next();
                    let job = dom.children_of_kind(node.id, K::Job).next();
                    let Some(input) = input else { continue };
                    if let Some(call_id) = node.str_attr(attr::CALL_ID) {
                        self.calls.insert(call_id.to_owned(), (node.id, turn.id));
                    }
                    if let Some(job) = job
                        && let Some(child) = job.str_attr(xattr::NATIVE_SESSION)
                    {
                        let running = job.str_attr(attr::STATUS) == Some("running");
                        let entry = self.children.entry(child.to_owned()).or_default();
                        entry.job = Some(job.id);
                        entry.call = Some(node.id);
                        entry.turn = running.then_some(turn.id);
                    }
                    for decision in dom.children_of_kind(node.id, K::Decision) {
                        self.index_decision(decision, turn.id);
                    }
                    self.parts.insert(
                        id.to_owned(),
                        PartEntry {
                            turn: turn.id,
                            message,
                            oc: node.str_attr(xattr::OC).unwrap_or_default().to_owned(),
                            kind: PartKind::Call {
                                node: node.id,
                                input: input.id,
                                input_text: Tracked::of(&input.text),
                                result: result.map(|r| (r.id, Tracked::of(&r.text))),
                                status: node.str_attr(attr::STATUS).and_then(CallStatus::parse).unwrap_or(CallStatus::Done),
                                title: s_opt(node, attr::TITLE),
                                started: node.int_attr(attr::STARTED),
                                ended: node.int_attr(attr::ENDED),
                                job: job.map(|j| j.id),
                            },
                        },
                    );
                }
                K::Decision => self.index_decision(node, turn.id),
                _ => {}
            }
        }
    }

    fn index_decision(&mut self, node: &Node, turn: NodeId) {
        let Some(request_id) = node.str_attr(attr::REQUEST_ID) else { return };
        let kind = match node.str_attr(attr::KIND) {
            Some("question") => {
                let index = node.int_attr(xattr::INDEX).and_then(|i| usize::try_from(i).ok()).unwrap_or(0);
                RequestKind::Question { index, count: 0 }
            }
            _ => RequestKind::Permission,
        };
        let answer = node.attr(attr::ANSWER).map(|a| a.str_items().map(str::to_owned).collect());
        self.decisions.insert(
            node.id,
            Pending {
                request_id: request_id.to_owned(),
                kind,
                turn: Some(turn),
                waiting: node.str_attr(attr::STATUS) == Some("waiting"),
                answer,
            },
        );
        let count = self.decisions.values().filter(|d| d.request_id == request_id).count();
        for d in self.decisions.values_mut().filter(|d| d.request_id == request_id) {
            if let RequestKind::Question { count: c, .. } = &mut d.kind {
                *c = count;
            }
        }
    }

    /// After a restart nothing runs in the harness any more: an open turn is
    /// closed as interrupted.
    pub fn close_stale(&mut self, sink: &dyn PatchSink, ts: i64) -> Result<()> {
        self.cancel_turn(sink, ts)
    }
}

fn s_opt(node: &Node, name: &str) -> Option<String> {
    node.str_attr(name).map(str::to_owned)
}
