//! OpenCode server events (and export files) onto the session DOM: one
//! `<turn id=<user message id>>` per user message; text and reasoning parts
//! as `<message id=<part id>>`; tool parts as `<call>` (the task tool's child
//! session as `<job kind="subagent">`); assistant message info and every other
//! part as `<ext>`; permission and question requests as `<decision>`; the todo
//! list as `<todo>`; a finished summary message as `<compaction>`. Native
//! fields no attribute holds are kept as JSON in `x-oc` for the export.

use super::event::{Event, Known, PermissionRequest, QuestionRequest, Todo, ToolRef};
use super::json::{self, int_at, is_true, str_at};
use crate::kit::{EndEvent, EventMapper, TurnLedger};
use loa_contracts::harness::{AdapterError, Answer, Result, SessionState};
use loa_core::{
    CallStatus, DecisionKind, DecisionStatus, Dom, DomWriter, ElementKind as K, JobKind, JobStatus, MessageKind, MessageStatus, Node, NodeId,
    PatchSink, Role, SCHEMA_VERSION, SessionMode, TodoState, TurnStatus, Value, answer, attr,
};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Harness id written to `<session harness>` and `<ext harness>`.
pub const HARNESS: &str = "opencode";

/// Extension attributes this adapter writes.
pub mod xattr {
    /// Compact JSON of the native object's fields no other attribute holds.
    pub const OC: &str = "x-oc";
    /// Native id of the message an element belongs to.
    pub const MESSAGE: &str = "x-message";
    /// Native part id of a `<call>` or `<ext type="part">`.
    pub const PART: &str = "x-part";
    /// Native id of a subagent's child session, on its `<job>`.
    pub const NATIVE_SESSION: &str = "x-native-session";
    pub const PRIORITY: &str = "x-priority";
    /// Todo status that isn't one of the schema states.
    pub const STATUS: &str = "x-status";
    /// Position of a question within its request.
    pub const INDEX: &str = "x-index";
    /// A question that accepts typed answers.
    pub const CUSTOM: &str = "x-custom";
    /// JSON of the session's pending revert.
    pub const REVERT: &str = "x-revert";
    pub const REASONING_TOKENS: &str = "x-reasoning-tokens";
    /// App session id of the parent of a mirrored child session.
    pub const PARENT_SESSION: &str = "x-parent-session";
}

/// `<ext type>` values this adapter writes.
pub mod ext {
    pub const MESSAGE: &str = "message";
    pub const PART: &str = "part";
    pub const ERROR: &str = "error";
}

/// The app's session store for subagent children: an app session id and an empty sink for
/// native child `native_id` of `parent_session_id`, or `None` to not mirror it.
pub trait ChildSessions: Send + Sync {
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
    /// The native session object.
    pub native: Json,
    /// More `x-` attributes.
    pub extra: Vec<(String, Value)>,
}

/// The request answering a decision: `POST path` with `body`.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub path: String,
    pub body: Option<Json>,
}

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

struct Call {
    node: NodeId,
    input: NodeId,
    input_text: String,
    result: Option<(NodeId, String)>,
    status: CallStatus,
    title: Option<String>,
    started: Option<i64>,
    ended: Option<i64>,
    job: Option<NodeId>,
}

enum PartKind {
    Text { node: NodeId, text: String, status: MessageStatus },
    Call(Call),
    Ext { node: NodeId, text: String },
}

impl PartKind {
    fn node(&self) -> NodeId {
        match self {
            PartKind::Text { node, .. } | PartKind::Ext { node, .. } | PartKind::Call(Call { node, .. }) => *node,
        }
    }
}

struct Part {
    turn: NodeId,
    message: String,
    oc: String,
    kind: PartKind,
}

/// The message a part belongs to.
struct Owner {
    turn: NodeId,
    user: bool,
    model: Option<String>,
}

/// One tool part's state, as the DOM needs it.
struct ToolState<'a> {
    status: CallStatus,
    tool: &'a str,
    call_id: &'a str,
    title: Option<&'a str>,
    started: Option<i64>,
    ended: Option<i64>,
    input: String,
    oc: String,
    /// Output or error text, and whether it is an error.
    output: Option<(&'a str, bool)>,
}

#[derive(Clone, Copy, Default)]
struct Usage {
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cache_write: i64,
    cost: f64,
}

/// What closing the open turn needs.
#[derive(Default)]
struct TurnState {
    /// Usage of each step-finish part, by part id.
    steps: BTreeMap<String, Usage>,
    error: bool,
    aborted: bool,
    last_ts: i64,
    model: Option<String>,
}

enum RequestKind {
    Permission,
    Question { index: usize, count: usize },
}

struct Pending {
    request_id: String,
    kind: RequestKind,
    turn: Option<NodeId>,
    waiting: bool,
    answer: Option<Vec<String>>,
}

struct Mirror {
    mapper: Box<Mapper>,
    sink: Arc<dyn PatchSink>,
    session_id: String,
}

#[derive(Default)]
struct Child {
    job: Option<NodeId>,
    call: Option<NodeId>,
    /// Set while the job runs; routes the child's decisions.
    turn: Option<NodeId>,
    mirror: Option<Mirror>,
}

/// Maps one native session (and, through [`ChildSessions`], its subagent children) onto one DOM.
pub struct Mapper {
    root: NodeId,
    session_id: String,
    node: String,
    cwd: Option<String>,
    native_id: String,
    session_oc: String,
    title: Option<String>,
    revert: Option<String>,
    ledger: TurnLedger<String>,
    /// State of the ledger's open turn.
    current: TurnState,
    turns: HashMap<String, NodeId>,
    turn_seq: Vec<NodeId>,
    /// Index into `turn_seq` of the first turn the next compaction covers.
    covered: usize,
    /// The user message `begin_turn` inserted, waiting for its native text part.
    bind: Option<(String, NodeId, String)>,
    messages: HashMap<String, Msg>,
    parts: HashMap<String, Part>,
    /// Native call id to (`<call>`, `<turn>`).
    calls: HashMap<String, (NodeId, NodeId)>,
    decisions: BTreeMap<NodeId, Pending>,
    todos: Vec<(NodeId, Todo)>,
    children: HashMap<String, Child>,
    child_port: Option<Arc<dyn ChildSessions>>,
    model: Option<String>,
}

const TEXT_FIELDS: &[&str] = &["/id", "/sessionID", "/messageID", "/type", "/text"];
const EXT_FIELDS: &[&str] = &["/id", "/sessionID", "/messageID"];
const TOOL_FIELDS: &[&str] = &["/id", "/sessionID", "/messageID", "/type", "/callID", "/tool", "/state/raw", "/state/title", "/state/output", "/state/error"];
pub(super) const SESSION_FIELDS: &[&str] = &["/id", "/title", "/directory", "/time/created"];
const USER_FIELDS: &[&str] = &["/id", "/sessionID", "/role", "/time/created"];
const ASSISTANT_FIELDS: &[&str] = &["/id", "/sessionID"];

fn node_value(node: NodeId) -> Value {
    Value::Int(i64::try_from(node.0).unwrap_or(i64::MAX))
}

/// `provider/model` of the provider and model ids at `provider` and `model` in `v`.
fn model_at(v: &Json, provider: &str, model: &str) -> Option<String> {
    Some(format!("{}/{}", str_at(v, provider)?, str_at(v, model)?))
}

/// Brings a node's text from `current` to `full`: appends when `full` extends it, else replaces it.
fn sync_text(w: &DomWriter, node: NodeId, current: &mut String, full: &str) -> Result<()> {
    match full.strip_prefix(current.as_str()) {
        Some("") => {}
        Some(tail) => {
            w.append(node, tail)?;
            current.push_str(tail);
        }
        None => {
            w.set_text(node, full)?;
            full.clone_into(current);
        }
    }
    Ok(())
}

fn todo_state(status: &str) -> Option<TodoState> {
    if status == "completed" { Some(TodoState::Done) } else { TodoState::parse(status) }
}

impl<'a> ToolState<'a> {
    fn of(part: &'a Json) -> Self {
        let state = part.get("state").unwrap_or(&Json::Null);
        let native = str_at(state, "/status").unwrap_or("pending");
        let status = match native {
            "pending" => CallStatus::Pending,
            "running" => CallStatus::Running,
            "error" => CallStatus::Error,
            _ => CallStatus::Done,
        };
        // Pending tools stream raw arguments; afterwards `input` holds them, so `x-oc` drops it.
        let mut rest = json::without(part, TOOL_FIELDS);
        let input = if native == "pending" {
            str_at(state, "/raw").unwrap_or_default().to_owned()
        } else {
            if let Some(Json::Object(state)) = rest.get_mut("state") {
                state.remove("input");
            }
            state.get("input").map_or_else(|| "{}".to_owned(), Json::to_string)
        };
        let output = match native {
            "completed" => Some((str_at(state, "/output").unwrap_or_default(), false)),
            "error" => Some((str_at(state, "/error").unwrap_or_default(), true)),
            _ => None,
        };
        ToolState {
            status,
            tool: str_at(part, "/tool").unwrap_or_default(),
            call_id: str_at(part, "/callID").unwrap_or_default(),
            title: str_at(state, "/title"),
            started: int_at(state, "/time/start"),
            ended: int_at(state, "/time/end"),
            input,
            oc: json::rest(&rest, &[]),
            output,
        }
    }
}

impl Mapper {
    fn empty(root: NodeId, session_id: String, node: String, model: Option<String>) -> Self {
        Mapper {
            root,
            session_id,
            node,
            cwd: None,
            native_id: String::new(),
            session_oc: String::new(),
            title: None,
            revert: None,
            ledger: TurnLedger::new(root),
            current: TurnState::default(),
            turns: HashMap::new(),
            turn_seq: Vec::new(),
            covered: 0,
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

    /// Inserts the `<session>` root into an empty DOM.
    pub fn start(w: &DomWriter, info: RootInfo) -> Result<Self> {
        let native = &info.native;
        let native_id = str_at(native, "/id").unwrap_or_default().to_owned();
        let created = int_at(native, "/time/created").unwrap_or_else(|| w.now());
        let title = str_at(native, "/title").map(str::to_owned);
        let cwd = info.cwd.clone().or_else(|| str_at(native, "/directory").map(str::to_owned));
        let session_oc = json::rest(native, SESSION_FIELDS);
        let revert = native.get("revert").filter(|r| !r.is_null()).map(Json::to_string);
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
            (attr::NATIVE_ID, Some(native_id.clone()).filter(|id| !id.is_empty())),
            (xattr::OC, Some(session_oc.clone()).filter(|oc| !oc.is_empty())),
            (xattr::REVERT, revert.clone()),
        ];
        attrs.extend(optional.into_iter().filter_map(|(k, v)| Some((k, Value::Str(v?)))));
        attrs.extend(info.mode.map(|mode| (attr::MODE, mode.into())));
        attrs.extend(info.extra.iter().map(|(k, v)| (k.as_str(), v.clone())));
        let root = w.at(created).insert(NodeId::DOCUMENT, K::Session, attrs, "")?;
        let mut mapper = Self::empty(root, info.session_id, info.node, info.model);
        (mapper.cwd, mapper.native_id, mapper.session_oc, mapper.title, mapper.revert) = (cwd, native_id, session_oc, title, revert);
        Ok(mapper)
    }

    /// Mirrors subagent child sessions through `port`.
    pub fn with_child_sessions(mut self, port: Option<Arc<dyn ChildSessions>>) -> Self {
        self.child_port = port;
        self
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    /// The native session id events are matched against.
    pub fn native_id(&self) -> &str {
        &self.native_id
    }

    /// Points the mapper at another native session (after a native fork).
    pub fn set_native_id(&mut self, w: &DomWriter, native_id: &str) -> Result<()> {
        self.native_id = native_id.to_owned();
        Ok(w.set(self.root, attr::NATIVE_ID, native_id)?)
    }

    /// The model of the latest assistant message or selection, `provider/model`.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn set_model(&mut self, w: &DomWriter, model: &str) -> Result<()> {
        self.model = Some(model.to_owned());
        Ok(w.set(self.root, attr::MODEL, model)?)
    }

    pub fn state(&self) -> SessionState {
        self.ledger.state()
    }

    fn active(&self) -> Option<NodeId> {
        self.ledger.turn().map(|turn| turn.node)
    }

    /// Inserts the `<turn>` and user `<message>` of a prompt the app sends as native message `message_id`.
    pub fn begin_turn(&mut self, w: &DomWriter, message_id: &str, text: &str, by: &str) -> Result<NodeId> {
        if self.active().is_some() {
            return Err(AdapterError::Busy);
        }
        let ts = w.now();
        let attrs: [(&str, Value); 3] = [(attr::ID, message_id.into()), (attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, ts.into())];
        let turn = w.insert(self.root, K::Turn, attrs, "")?;
        let mut attrs: Vec<(&str, Value)> = vec![(attr::ROLE, Role::User.into()), (attr::STATUS, MessageStatus::Done.into()), (xattr::MESSAGE, message_id.into())];
        attrs.extend((!by.is_empty()).then(|| (attr::BY, by.into())));
        let message = w.insert(turn, K::Message, attrs, text)?;
        self.open_turn(message_id, turn, ts);
        self.bind = Some((message_id.to_owned(), message, text.to_owned()));
        Ok(turn)
    }

    fn open_turn(&mut self, message_id: &str, turn: NodeId, ts: i64) {
        self.turns.insert(message_id.to_owned(), turn);
        self.adopt(turn, ts, self.model.clone());
    }

    fn adopt(&mut self, turn: NodeId, ts: i64, model: Option<String>) {
        self.turn_seq.push(turn);
        self.current = TurnState { last_ts: ts, model, ..TurnState::default() };
        self.ledger.adopt(turn);
    }

    /// Ends the open turn as failed (the prompt request failed), keeping `message` in `<ext type="error">`.
    pub fn fail_turn(&mut self, w: &DomWriter, turn: NodeId, message: &str) -> Result<()> {
        w.ext(turn, HARNESS, ext::ERROR, message)?;
        self.close_active(w, Some(TurnStatus::Error))
    }

    /// The interrupt: streaming messages, running calls and jobs and waiting decisions are cancelled, then the turn.
    pub fn cancel_turn(&mut self, w: &DomWriter) -> Result<()> {
        self.close_active(w, Some(TurnStatus::Cancelled))
    }

    /// Ends the open turn as the harness reported it (an import reached the end of the file).
    pub fn finish(&mut self, w: &DomWriter) -> Result<()> {
        self.close_active(&w.at(self.current.last_ts.max(w.now())), None)
    }

    /// Feeds one server event.
    pub fn apply_event(&mut self, w: &DomWriter, event: Event) -> Result<()> {
        let Some(sid) = event.session_id() else { return Ok(()) };
        if sid != self.native_id {
            if let Event::Known(Known::SessionCreated { info }) = &event
                && str_at(info, "/parentID") == Some(self.native_id.as_str())
            {
                return self.register_child(w, info);
            }
            if self.children.contains_key(sid) {
                let sid = sid.to_owned();
                return self.child_event(w, &sid, event);
            }
            return Ok(());
        }
        let known = match event {
            Event::Known(known) => known,
            // `session.next.*` repeat the classic events; the others derive from messages the DOM holds.
            Event::Other { kind, .. } if kind.starts_with("session.next.") || matches!(kind.as_str(), "session.diff" | "command.executed" | "session.compacted") => return Ok(()),
            Event::Other { kind, properties } => return Ok(w.ext(self.root, HARNESS, &kind, properties).map(drop)?),
        };
        match known {
            Known::SessionCreated { .. } => Ok(()),
            Known::SessionUpdated { info } => self.session_updated(w, &info),
            Known::SessionStatus { status, .. } => match str_at(&status, "/type") {
                Some("idle") => self.close_active(w, None),
                Some("retry") => Ok(w.ext(self.active().unwrap_or(self.root), HARNESS, "session.status", status).map(drop)?),
                _ => Ok(()),
            },
            Known::SessionIdle(_) => self.close_active(w, None),
            Known::SessionError { error, .. } => self.session_error(w, error),
            Known::MessageUpdated { info, .. } => self.message_updated(w, &info),
            Known::MessageRemoved(at) => self.message_removed(w, &at.message),
            Known::PartUpdated { part, .. } => self.part_updated(w, &part),
            Known::PartDelta { at, field, delta } => self.part_delta(w, &at.part, &field, &delta),
            Known::PartRemoved(at) => match self.parts.remove(&at.part) {
                Some(part) => Ok(w.remove(part.kind.node())?),
                None => Ok(()),
            },
            Known::TodoUpdated { todos, .. } => self.sync_todos(w, &todos),
            Known::PermissionAsked(request) => self.permission_asked(w, &request, None),
            Known::PermissionReplied { at, reply } => self.permission_replied(w, &at.request, &reply),
            Known::QuestionAsked(request) => self.question_asked(w, &request, None),
            Known::QuestionReplied { at, answers } => self.question_replied(w, &at.request, &answers),
            Known::QuestionRejected(at) => self.request_closed(w, &at.request),
        }
    }

    fn session_updated(&mut self, w: &DomWriter, info: &Json) -> Result<()> {
        if let Some(title) = str_at(info, "/title").filter(|t| Some(*t) != self.title.as_deref()) {
            w.set(self.root, attr::TITLE, title)?;
            self.title = Some(title.to_owned());
        }
        let revert = info.get("revert").filter(|r| !r.is_null()).map(Json::to_string);
        if revert != self.revert {
            match &revert {
                Some(r) => w.set(self.root, xattr::REVERT, r.as_str())?,
                None => w.clear(self.root, xattr::REVERT)?,
            }
            self.revert = revert;
        }
        let oc = json::rest(info, SESSION_FIELDS);
        if oc != self.session_oc {
            w.set(self.root, xattr::OC, oc.as_str())?;
            self.session_oc = oc;
        }
        Ok(())
    }

    fn session_error(&mut self, w: &DomWriter, error: Option<Json>) -> Result<()> {
        let Some(error) = error else { return Ok(()) };
        let aborted = str_at(&error, "/name") == Some("MessageAbortedError");
        if self.active().is_some() {
            if aborted { self.current.aborted = true } else { self.current.error = true }
        }
        if aborted {
            return Ok(());
        }
        Ok(w.ext(self.active().unwrap_or(self.root), HARNESS, "session.error", error).map(drop)?)
    }

    /// A user or assistant message was created or changed.
    pub(super) fn message_updated(&mut self, w: &DomWriter, info: &Json) -> Result<()> {
        let id = str_at(info, "/id").ok_or_else(|| AdapterError::Protocol("message without id".into()))?;
        if str_at(info, "/role") == Some("user") { self.user_message(w, id, info) } else { self.assistant_message(w, id, info) }
    }

    fn user_message(&mut self, w: &DomWriter, id: &str, info: &Json) -> Result<()> {
        let oc = json::rest(info, USER_FIELDS);
        if let Some(msg) = self.messages.get_mut(id) {
            if msg.oc != oc {
                w.set(msg.turn, xattr::OC, oc.as_str())?;
                msg.oc = oc;
            }
            return Ok(());
        }
        let created = int_at(info, "/time/created");
        let turn = match self.turns.get(id).copied() {
            Some(turn) => {
                // The harness's own time wins over the app's, so exports match the native file.
                if let Some(created) = created {
                    w.set(turn, attr::STARTED, created)?;
                }
                turn
            }
            None => {
                // A prompt the app didn't send: the TUI, a command, compaction.
                self.close_active(&w.at(self.current.last_ts), None)?;
                let started = created.unwrap_or_else(|| w.now());
                let attrs: [(&str, Value); 3] = [(attr::ID, id.into()), (attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, started.into())];
                let turn = w.at(started).insert(self.root, K::Turn, attrs, "")?;
                self.open_turn(id, turn, started);
                turn
            }
        };
        if !oc.is_empty() {
            w.set(turn, xattr::OC, oc.as_str())?;
        }
        if let Some(model) = model_at(info, "/model/providerID", "/model/modelID").filter(|_| self.active() == Some(turn)) {
            self.current.model = Some(model);
        }
        self.messages.insert(id.to_owned(), Msg { turn, user: true, ext: None, oc, model: None, summary: false, compacted: false });
        Ok(())
    }

    fn assistant_message(&mut self, w: &DomWriter, id: &str, info: &Json) -> Result<()> {
        let model = model_at(info, "/providerID", "/modelID");
        if model.is_some() {
            self.model.clone_from(&model);
        }
        let text = json::rest(info, ASSISTANT_FIELDS);
        let summary = is_true(info, "/summary");
        let turn = match self.messages.get_mut(id) {
            Some(msg) => {
                if msg.oc != text
                    && let Some(ext) = msg.ext
                {
                    w.set_text(ext, text.as_str())?;
                    msg.oc = text;
                }
                msg.summary = summary;
                if model.is_some() {
                    msg.model = model;
                }
                msg.turn
            }
            None => {
                let parent = str_at(info, "/parentID").and_then(|p| self.turns.get(p)).copied();
                let turn = match parent.or(self.active()) {
                    Some(turn) => turn,
                    None => {
                        // No prompt in view: the user message was removed or predates a fork.
                        let attrs: [(&str, Value); 2] = [(attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, w.now().into())];
                        let turn = w.insert(self.root, K::Turn, attrs, "")?;
                        self.adopt(turn, w.now(), None);
                        turn
                    }
                };
                let attrs = [(attr::HARNESS, HARNESS), (attr::TYPE, ext::MESSAGE), (xattr::MESSAGE, id)];
                let ext_node = w.at(int_at(info, "/time/created").unwrap_or_else(|| w.now())).insert(turn, K::Ext, attrs, text.as_str())?;
                self.messages.insert(id.to_owned(), Msg { turn, user: false, ext: Some(ext_node), oc: text, model, summary, compacted: false });
                turn
            }
        };
        let completed = int_at(info, "/time/completed");
        if self.active() == Some(turn) {
            let st = &mut self.current;
            if let Some(model) = self.messages.get(id).and_then(|m| m.model.clone()) {
                st.model = Some(model);
            }
            st.last_ts = st.last_ts.max(completed.unwrap_or(i64::MIN));
            match str_at(info, "/error/name") {
                Some("MessageAbortedError") => st.aborted = true,
                Some(_) => st.error = true,
                None => {}
            }
        }
        if summary && completed.is_some() {
            self.record_compaction(w, id, turn)?;
        }
        Ok(())
    }

    /// Adds `<compaction>` for a finished summary message, covering the turns since the previous one.
    fn record_compaction(&mut self, w: &DomWriter, message_id: &str, turn: NodeId) -> Result<()> {
        let Some(msg) = self.messages.get_mut(message_id).filter(|m| !m.compacted) else { return Ok(()) };
        msg.compacted = true;
        let model = msg.model.clone();
        let mut texts: Vec<(NodeId, &str)> = self.parts.values().filter(|p| p.message == message_id).filter_map(|p| match &p.kind {
            PartKind::Text { node, text, .. } => Some((*node, text.as_str())),
            _ => None,
        }).collect();
        texts.sort_by_key(|(node, _)| *node);
        let summary = texts.into_iter().map(|(_, t)| t).collect::<Vec<_>>().join("\n\n");
        let at = self.turn_seq.iter().position(|t| *t == turn).unwrap_or(self.turn_seq.len());
        let covers = self.turn_seq[self.covered.min(at)..at].iter().map(|t| node_value(*t)).collect();
        self.covered = at;
        let mut attrs = vec![(attr::COVERS, Value::List(covers)), (attr::CREATED, w.now().into())];
        attrs.extend(model.map(|model| (attr::MODEL, model.into())));
        Ok(w.insert(self.root, K::Compaction, attrs, summary).map(drop)?)
    }

    fn message_removed(&mut self, w: &DomWriter, message_id: &str) -> Result<()> {
        let Some(msg) = self.messages.remove(message_id) else { return Ok(()) };
        if msg.user {
            // Reverting drops the whole exchange: the turn and all it holds.
            w.remove(msg.turn)?;
            self.turns.remove(message_id);
            self.turn_seq.retain(|t| *t != msg.turn);
            self.covered = self.covered.min(self.turn_seq.len());
            self.messages.retain(|_, m| m.turn != msg.turn);
            self.parts.retain(|_, p| p.turn != msg.turn);
            self.calls.retain(|_, (_, turn)| *turn != msg.turn);
            self.decisions.retain(|_, d| d.turn != Some(msg.turn));
            if self.active() == Some(msg.turn) {
                self.ledger = TurnLedger::new(self.root);
            }
            return Ok(());
        }
        let ids: Vec<String> = self.parts.iter().filter(|(_, p)| p.message == message_id).map(|(id, _)| id.clone()).collect();
        let mut doomed: Vec<NodeId> = msg.ext.into_iter().chain(ids.iter().filter_map(|id| self.parts.remove(id)).map(|p| p.kind.node())).collect();
        doomed.sort();
        doomed.into_iter().try_for_each(|node| w.remove(node))?;
        Ok(())
    }

    fn part_delta(&mut self, w: &DomWriter, part: &str, field: &str, delta: &str) -> Result<()> {
        if field == "text"
            && let Some(Part { kind: PartKind::Text { node, text, .. }, .. }) = self.parts.get_mut(part)
        {
            w.append(*node, delta)?;
            text.push_str(delta);
        }
        Ok(())
    }

    /// A part was created or changed (always the full part).
    pub(super) fn part_updated(&mut self, w: &DomWriter, part: &Json) -> Result<()> {
        let id = str_at(part, "/id").ok_or_else(|| AdapterError::Protocol("part without id".into()))?;
        let message = str_at(part, "/messageID").ok_or_else(|| AdapterError::Protocol(format!("part {id} has no message")))?;
        let msg = self.messages.get(message).ok_or_else(|| AdapterError::Protocol(format!("part {id} of unknown message {message}")))?;
        let owner = Owner { turn: msg.turn, user: msg.user, model: msg.model.clone() };
        match str_at(part, "/type") {
            Some("text") if !is_true(part, "/synthetic") => self.text_part(w, id, message, part, owner, false),
            Some("reasoning") => self.text_part(w, id, message, part, owner, true),
            Some("tool") => self.tool_part(w, id, message, part, owner.turn),
            _ => self.ext_part(w, id, message, part, owner.turn),
        }
    }

    fn text_part(&mut self, w: &DomWriter, id: &str, message: &str, part: &Json, owner: Owner, thinking: bool) -> Result<()> {
        let full = str_at(part, "/text").unwrap_or_default();
        let oc = json::rest(part, TEXT_FIELDS);
        let status = if owner.user || int_at(part, "/time/end").is_some() { MessageStatus::Done } else { MessageStatus::Streaming };
        match self.parts.get_mut(id) {
            Some(entry) => update_text(w, entry, full, oc, status),
            None => {
                let node = self.new_text(w, message, part, &oc, &owner, thinking, status)?;
                let kind = PartKind::Text { node, text: full.to_owned(), status };
                self.parts.insert(id.to_owned(), Part { turn: owner.turn, message: message.to_owned(), oc, kind });
                Ok(())
            }
        }
    }

    /// The node of a new text part: the user message `begin_turn` inserted, or a new `<message>`.
    #[allow(clippy::too_many_arguments)]
    fn new_text(&mut self, w: &DomWriter, message: &str, part: &Json, oc: &str, owner: &Owner, thinking: bool, status: MessageStatus) -> Result<NodeId> {
        let full = str_at(part, "/text").unwrap_or_default();
        if owner.user && !thinking && self.bind.as_ref().is_some_and(|(mid, _, _)| mid == message) {
            let (_, node, mut text) = self.bind.take().expect("checked");
            w.set(node, attr::ID, str_at(part, "/id").unwrap_or_default())?;
            if !oc.is_empty() {
                w.set(node, xattr::OC, oc)?;
            }
            sync_text(w, node, &mut text, full)?;
            return Ok(node);
        }
        let role = if owner.user { Role::User } else { Role::Assistant };
        let mut attrs: Vec<(&str, Value)> = vec![(attr::ROLE, role.into()), (attr::ID, str_at(part, "/id").unwrap_or_default().into()), (attr::STATUS, status.into()), (xattr::MESSAGE, message.into())];
        attrs.extend(thinking.then(|| (attr::KIND, MessageKind::Thinking.into())));
        attrs.extend(owner.model.clone().filter(|_| !owner.user).map(|model| (attr::MODEL, model.into())));
        attrs.extend((!oc.is_empty()).then(|| (xattr::OC, oc.into())));
        Ok(w.at(int_at(part, "/time/start").unwrap_or_else(|| w.now())).insert(owner.turn, K::Message, attrs, full)?)
    }

    fn tool_part(&mut self, w: &DomWriter, id: &str, message: &str, part: &Json, turn: NodeId) -> Result<()> {
        let tool = ToolState::of(part);
        match self.parts.get_mut(id) {
            Some(Part { kind: PartKind::Call(call), oc, .. }) => update_call(w, call, oc, &tool)?,
            Some(_) => {}
            None => {
                let call = new_call(w, turn, id, message, &tool)?;
                self.calls.insert(tool.call_id.to_owned(), (call.node, turn));
                self.parts.insert(id.to_owned(), Part { turn, message: message.to_owned(), oc: tool.oc.clone(), kind: PartKind::Call(call) });
            }
        }
        let state = part.get("state").unwrap_or(&Json::Null);
        match tool.tool {
            "task" => self.task_job(w, id, turn, state),
            "todowrite" if tool.status == CallStatus::Done && tool.output.is_some_and(|(_, error)| !error) => {
                match state.pointer("/input/todos").map(|t| serde_json::from_value::<Vec<Todo>>(t.clone())) {
                    Some(Ok(todos)) => self.sync_todos(&w.at(tool.ended.unwrap_or_else(|| w.now())), &todos),
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    /// Keeps the `<job kind="subagent">` of a task call in step with it.
    fn task_job(&mut self, w: &DomWriter, part: &str, turn: NodeId, state: &Json) -> Result<()> {
        let Some(Part { kind: PartKind::Call(call), .. }) = self.parts.get_mut(part) else { return Ok(()) };
        let job = match call.job {
            Some(job) => job,
            None => {
                let Some(child) = str_at(state, "/metadata/sessionId") else { return Ok(()) };
                let started = call.started.unwrap_or_else(|| w.now());
                let mut attrs: Vec<(&str, Value)> = vec![
                    (attr::KIND, JobKind::Subagent.into()),
                    (attr::STATUS, JobStatus::Running.into()),
                    (attr::CALL, node_value(call.node)),
                    (attr::STARTED, started.into()),
                    (xattr::NATIVE_SESSION, child.into()),
                ];
                attrs.extend(str_at(state, "/title").or_else(|| str_at(state, "/input/description")).map(|title| (attr::TITLE, title.into())));
                attrs.extend(str_at(state, "/input/subagent_type").map(|agent| (attr::AGENT, agent.into())));
                let entry = self.children.entry(child.to_owned()).or_default();
                attrs.extend(entry.mirror.as_ref().map(|m| (attr::SESSION, m.session_id.as_str().into())));
                let job = w.at(started).insert(call.node, K::Job, attrs, "")?;
                (entry.job, entry.call, entry.turn, call.job) = (Some(job), Some(call.node), Some(turn), Some(job));
                job
            }
        };
        let status = match call.status {
            CallStatus::Done => JobStatus::Done,
            CallStatus::Error => JobStatus::Error,
            CallStatus::Cancelled => JobStatus::Cancelled,
            CallStatus::Pending | CallStatus::Running => return Ok(()),
        };
        let ended = call.ended;
        if let Some(child) = self.children.values_mut().find(|c| c.job == Some(job) && c.turn.is_some()) {
            child.turn = None;
            w.set(job, attr::STATUS, status)?;
            w.set(job, attr::ENDED, ended.unwrap_or_else(|| w.now()))?;
        }
        Ok(())
    }

    fn ext_part(&mut self, w: &DomWriter, id: &str, message: &str, part: &Json, turn: NodeId) -> Result<()> {
        let text = json::without(part, EXT_FIELDS).to_string();
        if str_at(part, "/type") == Some("step-finish") && self.active() == Some(turn) {
            let n = |ptr| int_at(part, ptr).unwrap_or(0);
            let usage = Usage {
                input: n("/tokens/input"),
                output: n("/tokens/output"),
                reasoning: n("/tokens/reasoning"),
                cache_read: n("/tokens/cache/read"),
                cache_write: n("/tokens/cache/write"),
                cost: part.pointer("/cost").and_then(Json::as_f64).unwrap_or(0.0),
            };
            self.current.steps.insert(id.to_owned(), usage);
        }
        match self.parts.get_mut(id) {
            Some(Part { kind: PartKind::Ext { node, text: current }, .. }) if *current != text => {
                w.set_text(*node, text.as_str())?;
                *current = text;
            }
            Some(_) => {}
            None => {
                let attrs = [(attr::HARNESS, HARNESS), (attr::TYPE, ext::PART), (xattr::PART, id), (xattr::MESSAGE, message)];
                let node = w.insert(turn, K::Ext, attrs, text.as_str())?;
                self.parts.insert(id.to_owned(), Part { turn, message: message.to_owned(), oc: String::new(), kind: PartKind::Ext { node, text } });
            }
        }
        Ok(())
    }

    /// Ends the open turn: `forced` (interrupt, failed prompt) or the status the harness reported.
    fn close_active(&mut self, w: &DomWriter, forced: Option<TurnStatus>) -> Result<()> {
        let Some(turn) = self.active() else { return Ok(()) };
        let st = std::mem::take(&mut self.current);
        let derived = if st.error { TurnStatus::Error } else if st.aborted { TurnStatus::Cancelled } else { TurnStatus::Done };
        let status = forced.unwrap_or(derived);
        let mut ids: Vec<&String> = self.parts.iter().filter(|(_, p)| p.turn == turn).map(|(id, _)| id).collect();
        ids.sort_by_key(|id| self.parts[*id].kind.node());
        let ids: Vec<String> = ids.into_iter().cloned().collect();
        for id in ids {
            match &mut self.parts.get_mut(&id).expect("listed").kind {
                PartKind::Text { node, status: current, .. } if *current == MessageStatus::Streaming => {
                    *current = if status == TurnStatus::Cancelled { MessageStatus::Cancelled } else { MessageStatus::Done };
                    w.set(*node, attr::STATUS, *current)?;
                }
                PartKind::Call(call) if matches!(call.status, CallStatus::Pending | CallStatus::Running) => {
                    call.status = CallStatus::Cancelled;
                    w.set(call.node, attr::STATUS, CallStatus::Cancelled)?;
                    if call.ended.is_none() {
                        w.set(call.node, attr::ENDED, w.now())?;
                        call.ended = Some(w.now());
                    }
                    if let Some(job) = call.job
                        && let Some(child) = self.children.values_mut().find(|c| c.job == Some(job) && c.turn.is_some())
                    {
                        child.turn = None;
                        w.close(job, JobStatus::Cancelled)?;
                    }
                }
                _ => {}
            }
        }
        let decisions = &mut self.decisions;
        self.ledger.end_turn(w, status, |event| {
            if let EndEvent::Decision(node) = event
                && let Some(pending) = decisions.get_mut(&node)
            {
                pending.waiting = false;
            }
        })?;
        let total = st.steps.values().fold(None::<Usage>, |sum, u| {
            let s = sum.unwrap_or_default();
            Some(Usage { input: s.input + u.input, output: s.output + u.output, reasoning: s.reasoning + u.reasoning, cache_read: s.cache_read + u.cache_read, cache_write: s.cache_write + u.cache_write, cost: s.cost + u.cost })
        });
        if let Some(u) = total {
            let mut attrs: Vec<(&str, Value)> = vec![
                (attr::INPUT_TOKENS, u.input.into()),
                (attr::OUTPUT_TOKENS, u.output.into()),
                (attr::CACHE_READ_TOKENS, u.cache_read.into()),
                (attr::CACHE_WRITE_TOKENS, u.cache_write.into()),
                (attr::COST, u.cost.into()),
                (xattr::REASONING_TOKENS, u.reasoning.into()),
            ];
            attrs.extend(st.model.map(|model| (attr::MODEL, model.into())));
            w.insert(turn, K::Usage, attrs, "")?;
        }
        if self.bind.as_ref().is_some_and(|b| self.turns.get(&b.0) == Some(&turn)) {
            self.bind = None;
        }
        Ok(())
    }

    /// Replaces the todo list: changed entries are set, new ones inserted, dropped ones removed.
    fn sync_todos(&mut self, w: &DomWriter, todos: &[Todo]) -> Result<()> {
        for (i, todo) in todos.iter().enumerate() {
            let state = todo_state(&todo.status);
            let Some((node, current)) = self.todos.get_mut(i) else {
                let mut attrs: Vec<(&str, Value)> = vec![(attr::STATE, state.unwrap_or(TodoState::Pending).into())];
                attrs.extend(state.is_none().then(|| (xattr::STATUS, todo.status.as_str().into())));
                attrs.extend((!todo.priority.is_empty()).then(|| (xattr::PRIORITY, todo.priority.as_str().into())));
                let node = w.insert(self.root, K::Todo, attrs, todo.content.as_str())?;
                self.todos.push((node, todo.clone()));
                continue;
            };
            if current.content != todo.content {
                w.set_text(*node, todo.content.as_str())?;
            }
            if current.status != todo.status {
                w.set(*node, attr::STATE, state.unwrap_or(TodoState::Pending))?;
                match state {
                    Some(_) if todo_state(&current.status).is_none() => w.clear(*node, xattr::STATUS)?,
                    Some(_) => {}
                    None => w.set(*node, xattr::STATUS, todo.status.as_str())?,
                }
            }
            if current.priority != todo.priority {
                w.set(*node, xattr::PRIORITY, todo.priority.as_str())?;
            }
            *current = todo.clone();
        }
        for (node, _) in self.todos.drain(todos.len().min(self.todos.len())..) {
            w.remove(node)?;
        }
        Ok(())
    }

    /// Where a request about `tool` (of this session or of child `child`) goes: parent node, `<call>`, turn it blocks.
    fn request_place(&self, tool: Option<&ToolRef>, child: Option<&str>) -> (NodeId, Option<NodeId>, Option<NodeId>) {
        if let Some(c) = child.and_then(|c| self.children.get(c))
            && let (Some(call), Some(turn)) = (c.call, c.turn)
        {
            return (call, Some(call), Some(turn));
        }
        if let Some((call, turn)) = tool.and_then(|t| self.calls.get(&t.call_id)) {
            return (*call, Some(*call), Some(*turn));
        }
        match self.active() {
            Some(turn) => (turn, None, Some(turn)),
            None => (self.root, None, None),
        }
    }

    /// Records a new waiting decision on the turn it blocks.
    fn waiting(&mut self, w: &DomWriter, node: NodeId, request_id: &str, kind: RequestKind, turn: Option<NodeId>) -> Result<()> {
        self.decisions.insert(node, Pending { request_id: request_id.to_owned(), kind, turn, waiting: true, answer: None });
        if turn.is_some() && turn == self.active() {
            self.ledger.wait(node);
            w.set(self.ledger.turn().expect("active").node, attr::STATUS, TurnStatus::Waiting)?;
        }
        Ok(())
    }

    fn permission_asked(&mut self, w: &DomWriter, request: &PermissionRequest, child: Option<&str>) -> Result<()> {
        if self.decisions.values().any(|d| d.request_id == request.id) {
            return Ok(());
        }
        let (parent, call, turn) = self.request_place(request.tool.as_ref(), child);
        let title = if request.patterns.is_empty() { request.permission.clone() } else { format!("{}: {}", request.permission, request.patterns.join(", ")) };
        let body = ["/command", "/diff", "/filepath"].iter().find_map(|p| str_at(&request.metadata, p)).map_or_else(|| request.patterns.join("\n"), str::to_owned);
        let oc = json!({ "permission": request.permission, "patterns": request.patterns, "always": request.always, "metadata": request.metadata });
        let mut attrs: Vec<(&str, Value)> = vec![
            (attr::KIND, DecisionKind::Approval.into()),
            (attr::STATUS, DecisionStatus::Waiting.into()),
            (attr::REQUEST_ID, request.id.as_str().into()),
            (attr::TITLE, title.into()),
            (attr::CREATED, w.now().into()),
            (xattr::OC, oc.to_string().into()),
        ];
        attrs.extend(call.map(|call| (attr::CALL, node_value(call))));
        let node = w.insert(parent, K::Decision, attrs, body)?;
        self.waiting(w, node, &request.id, RequestKind::Permission, turn)
    }

    fn question_asked(&mut self, w: &DomWriter, request: &QuestionRequest, child: Option<&str>) -> Result<()> {
        if self.decisions.values().any(|d| d.request_id == request.id) {
            return Ok(());
        }
        let (parent, call, turn) = self.request_place(request.tool.as_ref(), child);
        let count = request.questions.len();
        for (index, q) in request.questions.iter().enumerate() {
            let mut attrs: Vec<(&str, Value)> = vec![
                (attr::KIND, DecisionKind::Question.into()),
                (attr::STATUS, DecisionStatus::Waiting.into()),
                (attr::REQUEST_ID, request.id.as_str().into()),
                (attr::OPTIONS, Value::List(q.options.iter().map(|o| Value::Str(o.label.clone())).collect())),
                (attr::MULTI, q.multiple.unwrap_or(false).into()),
                (attr::CREATED, w.now().into()),
                (xattr::INDEX, i64::try_from(index).unwrap_or(i64::MAX).into()),
            ];
            attrs.extend((!q.header.is_empty()).then(|| (attr::TITLE, q.header.as_str().into())));
            attrs.extend(q.custom.map(|custom| (xattr::CUSTOM, custom.into())));
            attrs.extend(call.map(|call| (attr::CALL, node_value(call))));
            let node = w.insert(parent, K::Decision, attrs, q.question.as_str())?;
            self.waiting(w, node, &request.id, RequestKind::Question { index, count }, turn)?;
        }
        Ok(())
    }

    /// The request answering `decision` with `answer`, checked against its kind; `None` while
    /// other questions of the same request still wait. Doesn't change the DOM.
    pub fn plan_reply(&self, decision: NodeId, answer: &Answer) -> Result<Option<Reply>> {
        let pending = self.decisions.get(&decision).filter(|d| d.waiting).ok_or(AdapterError::UnknownDecision(decision))?;
        let id = &pending.request_id;
        let RequestKind::Question { index, count } = pending.kind else {
            let reply = match answer.values.first().map(String::as_str) {
                Some(answer::ALLOW_ONCE) => "once",
                Some(answer::ALLOW_SESSION) => "always",
                Some(answer::DENY) => "reject",
                other => return Err(AdapterError::Protocol(format!("an approval takes allow_once, allow_session or deny, not {other:?}"))),
            };
            let mut body = json!({ "reply": reply });
            if let Some(message) = answer.reason.as_ref().filter(|_| reply == "reject") {
                body["message"] = json!(message);
            }
            return Ok(Some(Reply { path: format!("/permission/{id}/reply"), body: Some(body) }));
        };
        if answer.values.is_empty() {
            return Ok(Some(Reply { path: format!("/question/{id}/reject"), body: None }));
        }
        let mut answers = vec![Vec::new(); count];
        for d in self.decisions.values().filter(|d| &d.request_id == id) {
            if let RequestKind::Question { index: i, .. } = d.kind
                && let Some(slot) = answers.get_mut(i)
            {
                match (&d.answer, i == index) {
                    (_, true) => slot.clone_from(&answer.values),
                    (Some(a), false) => slot.clone_from(a),
                    (None, false) => return Ok(None),
                }
            }
        }
        Ok(Some(Reply { path: format!("/question/{id}/reply"), body: Some(json!({ "answers": answers })) }))
    }

    /// Records `answer` on `decision`; the turn runs again once nothing waits.
    pub fn record_answer(&mut self, w: &DomWriter, decision: NodeId, answer: &Answer) -> Result<()> {
        let pending = self.decisions.get_mut(&decision).filter(|d| d.waiting).ok_or(AdapterError::UnknownDecision(decision))?;
        let rejected = answer.values.is_empty() && matches!(pending.kind, RequestKind::Question { .. });
        pending.waiting = false;
        pending.answer = Some(answer.values.clone());
        let (turn, request_id) = (pending.turn, pending.request_id.clone());
        self.ledger.answered(decision);
        if !rejected {
            w.set(decision, attr::ANSWER, answer.values.clone())?;
        }
        if !answer.by.is_empty() {
            w.set(decision, attr::BY, answer.by.as_str())?;
        }
        if rejected {
            // Dismissing one question rejects the whole request.
            w.close(decision, DecisionStatus::Cancelled)?;
            return self.request_closed(w, &request_id);
        }
        if let Some(reason) = &answer.reason {
            w.set(decision, attr::REASON, reason.as_str())?;
        }
        w.close(decision, DecisionStatus::Answered)?;
        self.resume_turn(w, turn)
    }

    fn resume_turn(&self, w: &DomWriter, turn: Option<NodeId>) -> Result<()> {
        if turn.is_some() && turn == self.active() && !self.decisions.values().any(|d| d.waiting && d.turn == turn) {
            w.set(turn.expect("checked"), attr::STATUS, TurnStatus::Running)?;
        }
        Ok(())
    }

    fn waiting_on(&self, request_id: &str) -> Vec<NodeId> {
        self.decisions.iter().filter(|(_, d)| d.waiting && d.request_id == request_id).map(|(n, _)| *n).collect()
    }

    /// A permission reply made by another client.
    fn permission_replied(&mut self, w: &DomWriter, request_id: &str, reply: &str) -> Result<()> {
        let value = match reply {
            "once" => answer::ALLOW_ONCE,
            "always" => answer::ALLOW_SESSION,
            _ => answer::DENY,
        };
        for node in self.waiting_on(request_id) {
            self.record_answer(w, node, &Answer { values: vec![value.to_owned()], reason: None, by: String::new() })?;
        }
        Ok(())
    }

    fn question_replied(&mut self, w: &DomWriter, request_id: &str, answers: &[Vec<String>]) -> Result<()> {
        for node in self.waiting_on(request_id) {
            let RequestKind::Question { index, .. } = self.decisions[&node].kind else { continue };
            let values = answers.get(index).cloned().unwrap_or_default();
            if !values.is_empty() {
                self.record_answer(w, node, &Answer { values, reason: None, by: String::new() })?;
            }
        }
        Ok(())
    }

    /// The request is gone without an answer: its waiting decisions are cancelled.
    fn request_closed(&mut self, w: &DomWriter, request_id: &str) -> Result<()> {
        let mut turns = Vec::new();
        for node in self.waiting_on(request_id) {
            w.close(node, DecisionStatus::Cancelled)?;
            let pending = self.decisions.get_mut(&node).expect("listed");
            pending.waiting = false;
            turns.push(pending.turn);
            self.ledger.answered(node);
        }
        turns.into_iter().try_for_each(|turn| self.resume_turn(w, turn))
    }

    fn register_child(&mut self, w: &DomWriter, info: &Json) -> Result<()> {
        let Some(id) = str_at(info, "/id") else { return Ok(()) };
        if self.children.get(id).is_some_and(|c| c.mirror.is_some()) {
            return Ok(());
        }
        let opened = self.child_port.as_ref().and_then(|port| Some((Arc::clone(port), port.open_child(&self.session_id, id)?)));
        let mirror = match opened {
            Some((port, (session_id, sink))) => {
                let extra = vec![(xattr::PARENT_SESSION.to_owned(), Value::Str(self.session_id.clone()))];
                let root = RootInfo { session_id: session_id.clone(), node: self.node.clone(), cwd: self.cwd.clone(), native: info.clone(), extra, ..Default::default() };
                let mapper = Mapper::start(&DomWriter::new(&*sink).at(w.now()), root)?.with_child_sessions(Some(port));
                Some(Mirror { mapper: Box::new(mapper), sink, session_id })
            }
            None => None,
        };
        let child = self.children.entry(id.to_owned()).or_default();
        if let (Some(m), Some(job)) = (&mirror, child.job) {
            // The task tool reported the child before its creation event.
            w.set(job, attr::SESSION, m.session_id.as_str())?;
        }
        child.mirror = mirror;
        Ok(())
    }

    fn child_event(&mut self, w: &DomWriter, child: &str, event: Event) -> Result<()> {
        match event {
            Event::Known(Known::PermissionAsked(request)) => self.permission_asked(w, &request, Some(child)),
            Event::Known(Known::QuestionAsked(request)) => self.question_asked(w, &request, Some(child)),
            Event::Known(Known::PermissionReplied { at, reply }) => self.permission_replied(w, &at.request, &reply),
            Event::Known(Known::QuestionReplied { at, answers }) => self.question_replied(w, &at.request, &answers),
            Event::Known(Known::QuestionRejected(at)) => self.request_closed(w, &at.request),
            event => match self.children.get_mut(child).and_then(|c| c.mirror.as_mut()) {
                Some(mirror) => mirror.mapper.apply_event(&DomWriter::new(&*mirror.sink).at(w.now()), event),
                None => Ok(()),
            },
        }
    }

    /// Rebuilds the mapper's view of a session from its DOM (resume, fork).
    pub fn from_dom(dom: &Dom) -> Result<Self> {
        let root = dom.root().ok_or_else(|| AdapterError::Format("the session DOM is empty".into()))?;
        let s = |a: &str| root.str_attr(a).map(str::to_owned);
        let mut m = Mapper::empty(root.id, s(attr::ID).unwrap_or_default(), s(attr::NODE).unwrap_or_default(), s(attr::MODEL));
        (m.cwd, m.native_id, m.session_oc, m.title, m.revert) = (s(attr::CWD), s(attr::NATIVE_ID).unwrap_or_default(), s(xattr::OC).unwrap_or_default(), s(attr::TITLE), s(xattr::REVERT));
        m.ledger = TurnLedger::from_dom(dom);
        for child in dom.children(root.id) {
            match child.kind {
                K::Turn => m.index_turn(dom, child),
                K::Todo => {
                    let status = child.str_attr(xattr::STATUS).map(str::to_owned).unwrap_or_else(|| match child.str_attr(attr::STATE) {
                        Some("done") => "completed".to_owned(),
                        other => other.unwrap_or("pending").to_owned(),
                    });
                    let priority = child.str_attr(xattr::PRIORITY).unwrap_or_default().to_owned();
                    m.todos.push((child.id, Todo { content: child.text.clone(), status, priority }));
                }
                K::Compaction => {
                    let last = child.attr(attr::COVERS).and_then(Value::as_list).and_then(|l| l.iter().filter_map(Value::as_int).max());
                    if let Some(at) = last.and_then(|n| m.turn_seq.iter().position(|t| node_value(*t) == Value::Int(n))) {
                        m.covered = at + 1;
                    }
                }
                _ => {}
            }
        }
        if let Some(turn) = m.active().and_then(|turn| dom.get(turn)) {
            m.current.last_ts = turn.int_attr(attr::STARTED).unwrap_or(0);
        }
        Ok(m)
    }

    fn index_turn(&mut self, dom: &Dom, turn: &Node) {
        self.turn_seq.push(turn.id);
        if let Some(id) = turn.str_attr(attr::ID) {
            self.turns.insert(id.to_owned(), turn.id);
            let oc = turn.str_attr(xattr::OC).unwrap_or_default().to_owned();
            self.messages.insert(id.to_owned(), Msg { turn: turn.id, user: true, ext: None, oc, model: None, summary: false, compacted: true });
        }
        for node in dom.children(turn.id) {
            let message = node.str_attr(xattr::MESSAGE).unwrap_or_default().to_owned();
            let oc = node.str_attr(xattr::OC).unwrap_or_default().to_owned();
            let kind = match node.kind {
                K::Ext if node.str_attr(attr::TYPE) == Some(ext::MESSAGE) => {
                    let info = json::parse_object(&node.text).unwrap_or(Json::Null);
                    let (model, summary) = (model_at(&info, "/providerID", "/modelID"), is_true(&info, "/summary"));
                    self.messages.insert(message, Msg { turn: turn.id, user: false, ext: Some(node.id), oc: node.text.clone(), model, summary, compacted: true });
                    continue;
                }
                K::Ext if node.str_attr(attr::TYPE) == Some(ext::PART) => (node.str_attr(xattr::PART), PartKind::Ext { node: node.id, text: node.text.clone() }),
                K::Message => {
                    let status = node.str_attr(attr::STATUS).and_then(MessageStatus::parse).unwrap_or(MessageStatus::Done);
                    (node.str_attr(attr::ID), PartKind::Text { node: node.id, text: node.text.clone(), status })
                }
                K::Call => {
                    let Some(input) = dom.children_of_kind(node.id, K::Input).next() else { continue };
                    let job = dom.children_of_kind(node.id, K::Job).next();
                    if let Some(call_id) = node.str_attr(attr::CALL_ID) {
                        self.calls.insert(call_id.to_owned(), (node.id, turn.id));
                    }
                    if let Some(job) = job
                        && let Some(child) = job.str_attr(xattr::NATIVE_SESSION)
                    {
                        let running = job.str_attr(attr::STATUS) == Some("running");
                        let entry = self.children.entry(child.to_owned()).or_default();
                        (entry.job, entry.call, entry.turn) = (Some(job.id), Some(node.id), running.then_some(turn.id));
                    }
                    for decision in dom.children_of_kind(node.id, K::Decision) {
                        self.index_decision(decision, turn.id);
                    }
                    let call = Call {
                        node: node.id,
                        input: input.id,
                        input_text: input.text.clone(),
                        result: dom.children_of_kind(node.id, K::Result).next().map(|r| (r.id, r.text.clone())),
                        status: node.str_attr(attr::STATUS).and_then(CallStatus::parse).unwrap_or(CallStatus::Done),
                        title: node.str_attr(attr::TITLE).map(str::to_owned),
                        started: node.int_attr(attr::STARTED),
                        ended: node.int_attr(attr::ENDED),
                        job: job.map(|j| j.id),
                    };
                    (node.str_attr(xattr::PART), PartKind::Call(call))
                }
                K::Decision => {
                    self.index_decision(node, turn.id);
                    continue;
                }
                _ => continue,
            };
            if let (Some(id), kind) = kind {
                let oc = if matches!(kind, PartKind::Ext { .. }) { String::new() } else { oc };
                self.parts.insert(id.to_owned(), Part { turn: turn.id, message, oc, kind });
            }
        }
    }

    fn index_decision(&mut self, node: &Node, turn: NodeId) {
        let Some(request_id) = node.str_attr(attr::REQUEST_ID) else { return };
        let kind = match node.str_attr(attr::KIND) {
            Some("question") => RequestKind::Question { index: node.int_attr(xattr::INDEX).and_then(|i| usize::try_from(i).ok()).unwrap_or(0), count: 0 },
            _ => RequestKind::Permission,
        };
        let answer = node.attr(attr::ANSWER).map(|a| a.str_items().map(str::to_owned).collect());
        let waiting = node.str_attr(attr::STATUS) == Some("waiting");
        self.decisions.insert(node.id, Pending { request_id: request_id.to_owned(), kind, turn: Some(turn), waiting, answer });
        let count = self.decisions.values().filter(|d| d.request_id == request_id).count();
        for d in self.decisions.values_mut().filter(|d| d.request_id == request_id) {
            if let RequestKind::Question { count: c, .. } = &mut d.kind {
                *c = count;
            }
        }
    }
}

fn update_text(w: &DomWriter, entry: &mut Part, full: &str, oc: String, status: MessageStatus) -> Result<()> {
    let PartKind::Text { node, text, status: current } = &mut entry.kind else { return Ok(()) };
    sync_text(w, *node, text, full)?;
    if entry.oc != oc {
        w.set(*node, xattr::OC, oc.as_str())?;
        entry.oc = oc;
    }
    if *current == MessageStatus::Streaming && status == MessageStatus::Done {
        w.set(*node, attr::STATUS, status)?;
        *current = status;
    }
    Ok(())
}

fn new_call(w: &DomWriter, turn: NodeId, id: &str, message: &str, tool: &ToolState) -> Result<Call> {
    let at = tool.started.unwrap_or_else(|| w.now());
    let mut attrs: Vec<(&str, Value)> = vec![
        (attr::TOOL, tool.tool.into()),
        (attr::CALL_ID, tool.call_id.into()),
        (attr::STATUS, tool.status.into()),
        (attr::STARTED, at.into()),
        (xattr::PART, id.into()),
        (xattr::MESSAGE, message.into()),
    ];
    attrs.extend(tool.title.map(|title| (attr::TITLE, title.into())));
    attrs.extend(tool.ended.map(|ended| (attr::ENDED, ended.into())));
    attrs.extend((!tool.oc.is_empty()).then(|| (xattr::OC, tool.oc.as_str().into())));
    let w = w.at(at);
    let node = w.insert(turn, K::Call, attrs, "")?;
    let input = w.insert(node, K::Input, Vec::<(&str, Value)>::new(), tool.input.as_str())?;
    let result = match tool.output {
        Some((text, is_error)) => Some((insert_result(&w.at(tool.ended.unwrap_or(at)), node, text, is_error)?, text.to_owned())),
        None => None,
    };
    let title = tool.title.map(str::to_owned);
    Ok(Call { node, input, input_text: tool.input.clone(), result, status: tool.status, title, started: Some(at), ended: tool.ended, job: None })
}

fn insert_result(w: &DomWriter, call: NodeId, text: &str, is_error: bool) -> Result<NodeId> {
    let attrs: Vec<(&str, Value)> = if is_error { vec![(attr::IS_ERROR, Value::Bool(true))] } else { vec![] };
    Ok(w.insert(call, K::Result, attrs, text)?)
}

fn update_call(w: &DomWriter, call: &mut Call, oc: &mut String, tool: &ToolState) -> Result<()> {
    sync_text(w, call.input, &mut call.input_text, &tool.input)?;
    if let Some(title) = tool.title.filter(|t| Some(*t) != call.title.as_deref()) {
        w.set(call.node, attr::TITLE, title)?;
        call.title = Some(title.to_owned());
    }
    if let Some(started) = tool.started.filter(|s| Some(*s) != call.started) {
        w.set(call.node, attr::STARTED, started)?;
        call.started = Some(started);
    }
    match (tool.output, &mut call.result) {
        (Some((text, _)), Some((node, current))) => sync_text(w, *node, current, text)?,
        (Some((text, is_error)), result @ None) => *result = Some((insert_result(&w.at(tool.ended.unwrap_or_else(|| w.now())), call.node, text, is_error)?, text.to_owned())),
        (None, _) => {}
    }
    if let Some(ended) = tool.ended.filter(|e| Some(*e) != call.ended) {
        w.set(call.node, attr::ENDED, ended)?;
        call.ended = Some(ended);
    }
    // A cancelled call stays cancelled: the interrupt decided it.
    if tool.status != call.status && call.status != CallStatus::Cancelled {
        w.set(call.node, attr::STATUS, tool.status)?;
        call.status = tool.status;
    }
    if *oc != tool.oc {
        w.set(call.node, xattr::OC, tool.oc.as_str())?;
        oc.clone_from(&tool.oc);
    }
    Ok(())
}

impl EventMapper for Mapper {
    type Event = Event;
    type Reply = Reply;

    fn apply(&mut self, ev: Event, w: &DomWriter) -> Result<Vec<Reply>> {
        self.apply_event(w, ev).map(|()| Vec::new())
    }

    fn answer(&mut self, decision: NodeId, answer: &Answer, w: &DomWriter) -> Result<Option<Reply>> {
        let reply = self.plan_reply(decision, answer)?;
        self.record_answer(w, decision, answer)?;
        Ok(reply)
    }

    fn ledger(&self) -> &TurnLedger<String> {
        &self.ledger
    }

    fn ledger_mut(&mut self) -> &mut TurnLedger<String> {
        &mut self.ledger
    }
}
