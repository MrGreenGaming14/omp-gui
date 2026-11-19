//! The patches of each event in "DOM conventions for adapters", written once
//! so the live player and the native import produce the same shapes.

use loa_core::{
    CallStatus, CommitError, DecisionKind, DecisionStatus, Draft, ElementKind, MessageKind, MessageStatus, NodeId,
    PatchSink, Role, SCHEMA_VERSION, SessionMode, TurnStatus, Value, attr,
};

pub(crate) type Result<T, E = CommitError> = std::result::Result<T, E>;

/// Attributes of the `<session>` root.
#[derive(Default)]
pub(crate) struct SessionHeader<'a> {
    pub id: &'a str,
    pub harness: &'a str,
    pub node: &'a str,
    pub created: i64,
    pub cwd: Option<&'a str>,
    pub project: Option<&'a str>,
    pub model: Option<&'a str>,
    pub model_role: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub mode: Option<SessionMode>,
}

/// A `<decision>` to raise.
pub(crate) struct AskSpec<'a> {
    pub kind: DecisionKind,
    pub request_id: &'a str,
    pub title: Option<&'a str>,
    pub options: &'a [String],
    pub body: &'a str,
}

/// Commits convention-shaped drafts to one session.
pub(crate) struct Writer<'a> {
    sink: &'a dyn PatchSink,
}

type Attrs = Vec<(&'static str, Value)>;

fn push_opt(attrs: &mut Attrs, name: &'static str, value: Option<&str>) {
    if let Some(v) = value {
        attrs.push((name, Value::from(v)));
    }
}

impl<'a> Writer<'a> {
    pub fn new(sink: &'a dyn PatchSink) -> Self {
        Self { sink }
    }

    fn insert(&self, parent: NodeId, kind: ElementKind, attrs: Attrs, text: &str, ts: i64) -> Result<NodeId> {
        Ok(self.sink.commit_at(Draft::insert(parent, kind, attrs, text), ts)?.node)
    }

    fn set(&self, node: NodeId, name: &str, value: impl Into<Value>, ts: i64) -> Result<()> {
        self.sink.commit_at(Draft::set(node, name, value), ts).map(drop)
    }

    pub fn session(&self, h: &SessionHeader<'_>) -> Result<NodeId> {
        let mut attrs: Attrs = vec![
            (attr::ID, h.id.into()),
            (attr::HARNESS, h.harness.into()),
            (attr::NODE, h.node.into()),
            (attr::CREATED, Value::Int(h.created)),
            (attr::SCHEMA, Value::Int(i64::from(SCHEMA_VERSION))),
        ];
        push_opt(&mut attrs, attr::CWD, h.cwd);
        push_opt(&mut attrs, attr::PROJECT, h.project);
        push_opt(&mut attrs, attr::MODEL, h.model);
        push_opt(&mut attrs, attr::MODEL_ROLE, h.model_role);
        push_opt(&mut attrs, attr::EFFORT, h.effort);
        if let Some(mode) = h.mode {
            attrs.push((attr::MODE, mode.into()));
        }
        self.insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "", h.created)
    }

    /// Prompt sent: the running turn, then the user message. Returns the turn.
    pub fn turn(&self, session: NodeId, id: Option<&str>, text: &str, by: Option<&str>, ts: i64) -> Result<NodeId> {
        let mut attrs: Attrs = vec![(attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, Value::Int(ts))];
        push_opt(&mut attrs, attr::ID, id);
        let turn = self.insert(session, ElementKind::Turn, attrs, "", ts)?;
        let mut attrs: Attrs = vec![(attr::ROLE, Role::User.into()), (attr::STATUS, MessageStatus::Done.into())];
        push_opt(&mut attrs, attr::BY, by.filter(|b| !b.is_empty()));
        self.insert(turn, ElementKind::Message, attrs, text, ts)?;
        Ok(turn)
    }

    /// An assistant message, streaming when `status` is `Streaming`.
    pub fn message(
        &self,
        turn: NodeId,
        thinking: bool,
        model: Option<&str>,
        status: MessageStatus,
        text: &str,
        ts: i64,
    ) -> Result<NodeId> {
        let mut attrs: Attrs = vec![(attr::ROLE, Role::Assistant.into()), (attr::STATUS, status.into())];
        if thinking {
            attrs.push((attr::KIND, MessageKind::Thinking.into()));
        }
        push_opt(&mut attrs, attr::MODEL, model);
        self.insert(turn, ElementKind::Message, attrs, text, ts)
    }

    pub fn append(&self, node: NodeId, text: &str, ts: i64) -> Result<()> {
        self.sink.commit_at(Draft::append(node, text), ts).map(drop)
    }

    pub fn status(&self, node: NodeId, status: impl Into<Value>, ts: i64) -> Result<()> {
        self.set(node, attr::STATUS, status, ts)
    }

    /// Tool call starts: the running call and its `<input>`. Returns (call, input).
    pub fn call(
        &self,
        turn: NodeId,
        tool: &str,
        call_id: &str,
        title: Option<&str>,
        args: &str,
        ts: i64,
    ) -> Result<(NodeId, NodeId)> {
        let mut attrs: Attrs = vec![
            (attr::TOOL, tool.into()),
            (attr::CALL_ID, call_id.into()),
            (attr::STATUS, CallStatus::Running.into()),
            (attr::STARTED, Value::Int(ts)),
        ];
        push_opt(&mut attrs, attr::TITLE, title);
        let call = self.insert(turn, ElementKind::Call, attrs, "", ts)?;
        let input = self.insert(call, ElementKind::Input, Vec::new(), args, ts)?;
        Ok((call, input))
    }

    /// Tool call ends: the `<result>`, then the status and `ended`.
    pub fn call_end(&self, call: NodeId, output: &str, is_error: bool, ts: i64) -> Result<()> {
        self.insert(call, ElementKind::Result, vec![(attr::IS_ERROR, Value::Bool(is_error))], output, ts)?;
        let status = if is_error { CallStatus::Error } else { CallStatus::Done };
        self.status(call, status, ts)?;
        self.set(call, attr::ENDED, ts, ts)
    }

    /// Harness needs input: the waiting decision under `parent` (a call or the
    /// turn), then the turn waits. `call` is set when `parent` is a call.
    pub fn ask(&self, parent: NodeId, turn: NodeId, call: Option<NodeId>, spec: &AskSpec<'_>, ts: i64) -> Result<NodeId> {
        let mut attrs: Attrs = vec![
            (attr::KIND, spec.kind.into()),
            (attr::STATUS, DecisionStatus::Waiting.into()),
            (attr::REQUEST_ID, spec.request_id.into()),
            (attr::CREATED, Value::Int(ts)),
        ];
        push_opt(&mut attrs, attr::TITLE, spec.title);
        if !spec.options.is_empty() {
            attrs.push((attr::OPTIONS, spec.options.to_vec().into()));
        }
        if let Some(call) = call {
            // Node ids are allocated from 1 upwards and stay far below i64::MAX.
            attrs.push((attr::CALL, Value::Int(i64::try_from(call.0).unwrap_or(i64::MAX))));
        }
        let decision = self.insert(parent, ElementKind::Decision, attrs, spec.body, ts)?;
        self.status(turn, TurnStatus::Waiting, ts)?;
        Ok(decision)
    }

    /// Decision answered: answer, by, reason, status, ended, then the turn runs again.
    pub fn answered(
        &self,
        decision: NodeId,
        turn: NodeId,
        values: &[String],
        by: Option<&str>,
        reason: Option<&str>,
        ts: i64,
    ) -> Result<()> {
        self.set(decision, attr::ANSWER, values.to_vec(), ts)?;
        if let Some(by) = by.filter(|b| !b.is_empty()) {
            self.set(decision, attr::BY, by, ts)?;
        }
        if let Some(reason) = reason {
            self.set(decision, attr::REASON, reason, ts)?;
        }
        self.status(decision, DecisionStatus::Answered, ts)?;
        self.set(decision, attr::ENDED, ts, ts)?;
        self.status(turn, TurnStatus::Running, ts)
    }

    pub fn usage(&self, parent: NodeId, input_tokens: i64, output_tokens: i64, ts: i64) -> Result<NodeId> {
        let attrs: Attrs =
            vec![(attr::INPUT_TOKENS, Value::Int(input_tokens)), (attr::OUTPUT_TOKENS, Value::Int(output_tokens))];
        self.insert(parent, ElementKind::Usage, attrs, "", ts)
    }

    pub fn ext(&self, parent: NodeId, harness: &str, kind: &str, raw: &str, ts: i64) -> Result<NodeId> {
        let attrs: Attrs = vec![(attr::HARNESS, harness.into()), (attr::TYPE, kind.into())];
        self.insert(parent, ElementKind::Ext, attrs, raw, ts)
    }

    /// Turn ends: status, then `ended`.
    pub fn end_turn(&self, turn: NodeId, status: TurnStatus, ts: i64) -> Result<()> {
        self.status(turn, status, ts)?;
        self.set(turn, attr::ENDED, ts, ts)
    }

    /// Marks a call or decision cancelled and ended.
    pub fn cancel_with_end(&self, node: NodeId, ts: i64) -> Result<()> {
        self.status(node, CallStatus::Cancelled, ts)?;
        self.set(node, attr::ENDED, ts, ts)
    }
}
