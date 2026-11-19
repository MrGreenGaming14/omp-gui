//! `check_dom` and `check_history` on hand-built DOMs: each test builds what a
//! buggy adapter would write and expects the rule it breaks.

use loa_adapter_testkit::{Phase, Rule, check_dom, check_dom_in, check_history};
use loa_core::{
    CallStatus, DecisionKind, DecisionStatus, Draft, ElementKind, MemorySink, MessageStatus, NodeId, PatchSink, Role,
    SCHEMA_VERSION, TurnStatus, Value, answer, attr,
};

/// Builds a session with one running turn and its user message.
struct Builder {
    sink: MemorySink,
    session: NodeId,
    turn: NodeId,
}

impl Builder {
    fn new() -> Self {
        let sink = MemorySink::new();
        let session = sink
            .commit_at(
                Draft::insert(
                    NodeId::DOCUMENT,
                    ElementKind::Session,
                    [
                        (attr::ID, Value::from("s")),
                        (attr::HARNESS, Value::from("scripted")),
                        (attr::NODE, Value::from("n")),
                        (attr::CREATED, Value::Int(10)),
                        (attr::SCHEMA, Value::Int(i64::from(SCHEMA_VERSION))),
                    ],
                    "",
                ),
                10,
            )
            .unwrap()
            .node;
        let mut b = Self { sink, session, turn: NodeId(0) };
        b.turn = b.new_turn();
        b
    }

    fn new_turn(&mut self) -> NodeId {
        let turn = self.insert(self.session, ElementKind::Turn, vec![(attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, Value::Int(20))], "");
        self.insert(turn, ElementKind::Message, vec![(attr::ROLE, Role::User.into()), (attr::STATUS, MessageStatus::Done.into())], "hi");
        turn
    }

    fn insert(&self, parent: NodeId, kind: ElementKind, attrs: Vec<(&str, Value)>, text: &str) -> NodeId {
        self.sink.commit_at(Draft::insert(parent, kind, attrs, text), 30).unwrap().node
    }

    fn set(&self, node: NodeId, name: &str, value: impl Into<Value>) {
        self.sink.commit_at(Draft::set(node, name, value), 40).unwrap();
    }

    fn call(&self, status: CallStatus) -> NodeId {
        let call = self.insert(
            self.turn,
            ElementKind::Call,
            vec![(attr::TOOL, "bash".into()), (attr::CALL_ID, "c1".into()), (attr::STATUS, status.into())],
            "",
        );
        self.insert(call, ElementKind::Input, vec![], "{\"command\":\"ls\"}");
        call
    }

    fn end_turn(&self) {
        self.set(self.turn, attr::STATUS, TurnStatus::Done);
        self.set(self.turn, attr::ENDED, Value::Int(50));
    }

    fn rules(&self) -> Vec<Rule> {
        self.sink.with_dom(check_dom).into_iter().map(|v| v.rule).collect()
    }

    fn history_rules(&self) -> Vec<Rule> {
        check_history(&self.sink.patches()).into_iter().map(|v| v.rule).collect()
    }
}

#[test]
fn a_well_formed_session_has_no_violations() {
    let b = Builder::new();
    let call = b.call(CallStatus::Running);
    b.insert(call, ElementKind::Result, vec![(attr::IS_ERROR, false.into())], "ok");
    b.set(call, attr::STATUS, CallStatus::Done);
    let reply = b.insert(b.turn, ElementKind::Message, vec![(attr::ROLE, Role::Assistant.into()), (attr::STATUS, MessageStatus::Streaming.into())], "");
    b.sink.commit(Draft::append(reply, "done")).unwrap();
    b.set(reply, attr::STATUS, MessageStatus::Done);
    b.end_turn();
    assert_eq!(b.sink.with_dom(check_dom), vec![]);
    assert_eq!(check_history(&b.sink.patches()), vec![]);
}

#[test]
fn an_empty_document_has_no_session() {
    let sink = MemorySink::new();
    assert_eq!(sink.with_dom(check_dom).into_iter().map(|v| v.rule).collect::<Vec<_>>(), vec![Rule::Root]);
}

#[test]
fn a_session_without_schema_version_is_reported() {
    let sink = MemorySink::new();
    let attrs = [
        (attr::ID, Value::from("s")),
        (attr::HARNESS, Value::from("x")),
        (attr::NODE, Value::from("")),
        (attr::CREATED, Value::Int(1)),
    ];
    sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "")).unwrap();
    let rules: Vec<Rule> = sink.with_dom(check_dom).into_iter().map(|v| v.rule).collect();
    assert_eq!(rules, vec![Rule::SessionAttrs, Rule::SchemaVersion]);
}

#[test]
fn a_turn_left_running_is_not_settled() {
    let b = Builder::new();
    assert_eq!(b.rules(), vec![Rule::NotTerminal]);
    // While live, a running last turn is expected.
    assert_eq!(b.sink.with_dom(|d| check_dom_in(d, Phase::Live)), vec![]);
}

#[test]
fn unknown_status_is_rejected_before_a_patch_is_committed() {
    let b = Builder::new();
    let before = b.sink.with_dom(|dom| dom.seq());
    assert_eq!(
        b.sink.commit(Draft::set(b.turn, attr::STATUS, "finished")),
        Err(loa_core::CommitError::Apply(loa_core::ApplyError::Schema(
            loa_core::SchemaError::EnumValue {
                kind: ElementKind::Turn,
                name: attr::STATUS.to_owned(),
                value: "finished".to_owned(),
            },
        )))
    );
    assert_eq!(b.sink.with_dom(|dom| dom.seq()), before);
    assert_eq!(b.sink.with_dom(|dom| dom.get(b.turn).unwrap().str_attr(attr::STATUS).map(str::to_owned)), Some("running".into()));
}

#[test]
fn a_call_without_input_is_reported() {
    let b = Builder::new();
    let call = b.insert(b.turn, ElementKind::Call, vec![(attr::TOOL, "bash".into()), (attr::STATUS, CallStatus::Running.into())], "");
    // Live and still running: the `<input>` may come with the next patch.
    assert_eq!(b.sink.with_dom(|d| check_dom_in(d, Phase::Live)), vec![]);
    b.insert(call, ElementKind::Result, vec![(attr::IS_ERROR, true.into())], "boom");
    b.set(call, attr::STATUS, CallStatus::Error);
    b.end_turn();
    assert_eq!(b.rules(), vec![Rule::CallInput]);
}

#[test]
fn call_arguments_must_be_json() {
    let b = Builder::new();
    let call = b.insert(b.turn, ElementKind::Call, vec![(attr::TOOL, "bash".into()), (attr::STATUS, CallStatus::Cancelled.into())], "");
    b.insert(call, ElementKind::Input, vec![], "{\"command\": \"ls");
    b.end_turn();
    assert_eq!(b.rules(), vec![Rule::CallInput]);
}

#[test]
fn a_finished_call_needs_a_matching_result() {
    let b = Builder::new();
    let call = b.call(CallStatus::Running);
    b.set(call, attr::STATUS, CallStatus::Done);
    b.end_turn();
    assert_eq!(b.rules(), vec![Rule::CallResult]);

    let b = Builder::new();
    let call = b.call(CallStatus::Running);
    b.insert(call, ElementKind::Result, vec![(attr::IS_ERROR, true.into())], "failed");
    b.set(call, attr::STATUS, CallStatus::Done);
    b.end_turn();
    assert_eq!(b.rules(), vec![Rule::CallResult]);
}

#[test]
fn a_waiting_decision_needs_a_waiting_turn() {
    let b = Builder::new();
    let call = b.call(CallStatus::Running);
    b.insert(
        call,
        ElementKind::Decision,
        vec![(attr::KIND, DecisionKind::Approval.into()), (attr::STATUS, DecisionStatus::Waiting.into())],
        "ls",
    );
    // A paused session (imported mid decision) is settled with the decision
    // waiting; the turn must say so.
    let rules = b.rules();
    assert!(rules.contains(&Rule::Waiting), "{rules:?}");

    b.set(b.turn, attr::STATUS, TurnStatus::Waiting);
    let rules = b.rules();
    assert!(!rules.contains(&Rule::Waiting), "{rules:?}");
}

#[test]
fn a_session_level_waiting_decision_has_no_waiting_turn() {
    let b = Builder::new();
    b.insert(
        b.session,
        ElementKind::Decision,
        vec![(attr::KIND, DecisionKind::Approval.into()), (attr::STATUS, DecisionStatus::Waiting.into())],
        "approve?",
    );
    let rules = b.rules();
    assert!(rules.contains(&Rule::Waiting), "{rules:?}");
}

#[test]
fn a_waiting_turn_needs_a_waiting_decision() {
    let b = Builder::new();
    b.set(b.turn, attr::STATUS, TurnStatus::Waiting);
    assert!(b.rules().contains(&Rule::Waiting));
}

#[test]
fn a_decision_call_attribute_must_name_a_call() {
    let b = Builder::new();
    let message = NodeId(3);
    b.insert(
        b.turn,
        ElementKind::Decision,
        vec![
            (attr::KIND, DecisionKind::Approval.into()),
            (attr::STATUS, DecisionStatus::Cancelled.into()),
            (attr::CALL, Value::Int(99)),
        ],
        "",
    );
    b.insert(
        b.turn,
        ElementKind::Decision,
        vec![
            (attr::KIND, DecisionKind::Approval.into()),
            (attr::STATUS, DecisionStatus::Cancelled.into()),
            (attr::CALL, Value::Int(i64::try_from(message.0).unwrap())),
        ],
        "",
    );
    b.end_turn();
    assert_eq!(b.rules(), vec![Rule::DanglingRef, Rule::DanglingRef]);
}

#[test]
fn compaction_covers_must_name_turns() {
    let b = Builder::new();
    b.end_turn();
    let turn = i64::try_from(b.turn.0).unwrap();
    b.insert(b.session, ElementKind::Compaction, vec![(attr::COVERS, Value::List(vec![Value::Int(turn), Value::Int(1000)]))], "summary");
    assert_eq!(b.rules(), vec![Rule::DanglingRef]);
}

#[test]
fn answers_must_fit_the_decision_kind() {
    let b = Builder::new();
    let approval = b.insert(b.turn, ElementKind::Decision, vec![(attr::KIND, DecisionKind::Approval.into()), (attr::STATUS, DecisionStatus::Waiting.into())], "");
    b.set(approval, attr::ANSWER, Value::from(vec!["yes".to_owned()]));
    b.set(approval, attr::STATUS, DecisionStatus::Answered);
    let form = b.insert(b.turn, ElementKind::Decision, vec![(attr::KIND, DecisionKind::Form.into()), (attr::STATUS, DecisionStatus::Waiting.into())], "");
    b.set(form, attr::ANSWER, Value::from(vec!["name=x".to_owned()]));
    b.set(form, attr::STATUS, DecisionStatus::Answered);
    let plan = b.insert(b.turn, ElementKind::Decision, vec![(attr::KIND, DecisionKind::Plan.into()), (attr::STATUS, DecisionStatus::Waiting.into())], "");
    b.set(plan, attr::ANSWER, Value::from(vec![answer::APPROVE.to_owned()]));
    b.set(plan, attr::STATUS, DecisionStatus::Answered);
    b.end_turn();
    let found = b.sink.with_dom(check_dom);
    let nodes: Vec<_> = found.iter().map(|v| (v.rule, v.node)).collect();
    assert_eq!(nodes, vec![(Rule::DecisionAnswer, Some(approval)), (Rule::DecisionAnswer, Some(form))]);
}

#[test]
fn harness_ids_must_be_unique() {
    let b = Builder::new();
    for _ in 0..2 {
        let call = b.call(CallStatus::Running);
        b.insert(call, ElementKind::Result, vec![], "");
        b.set(call, attr::STATUS, CallStatus::Done);
    }
    b.end_turn();
    assert_eq!(b.rules(), vec![Rule::DuplicateId]);
}

#[test]
fn a_turn_must_start_with_the_user_message() {
    let b = Builder::new();
    let turn = b.insert(b.session, ElementKind::Turn, vec![(attr::STATUS, TurnStatus::Done.into())], "");
    b.insert(turn, ElementKind::Message, vec![(attr::ROLE, Role::Assistant.into()), (attr::STATUS, MessageStatus::Done.into())], "hi");
    b.end_turn();
    assert_eq!(b.rules(), vec![Rule::TurnShape]);
}

#[test]
fn live_check_catches_a_second_open_turn() {
    let mut b = Builder::new();
    b.new_turn();
    let rules: Vec<Rule> = b.sink.with_dom(|d| check_dom_in(d, Phase::Live)).into_iter().map(|v| v.rule).collect();
    assert_eq!(rules, vec![Rule::OpenTurn]);
}

#[test]
fn ending_a_turn_before_its_call_is_out_of_order() {
    let b = Builder::new();
    let call = b.call(CallStatus::Running);
    b.end_turn();
    b.insert(call, ElementKind::Result, vec![], "late");
    b.set(call, attr::STATUS, CallStatus::Done);
    // The final DOM looks fine; only the history shows the turn ended first.
    assert_eq!(b.rules(), vec![]);
    assert_eq!(b.history_rules(), vec![Rule::CloseOrder]);
}

#[test]
fn a_finished_message_cannot_stream_again() {
    let b = Builder::new();
    let reply = b.insert(b.turn, ElementKind::Message, vec![(attr::ROLE, Role::Assistant.into()), (attr::STATUS, MessageStatus::Done.into())], "x");
    b.set(reply, attr::STATUS, MessageStatus::Streaming);
    b.set(reply, attr::STATUS, MessageStatus::Done);
    b.end_turn();
    assert_eq!(b.history_rules(), vec![Rule::Transition]);
}

#[test]
fn answered_before_the_answer_is_out_of_order() {
    let b = Builder::new();
    let d = b.insert(b.turn, ElementKind::Decision, vec![(attr::KIND, DecisionKind::Approval.into()), (attr::STATUS, DecisionStatus::Waiting.into())], "");
    b.set(b.turn, attr::STATUS, TurnStatus::Waiting);
    b.set(d, attr::STATUS, DecisionStatus::Answered);
    b.set(d, attr::ANSWER, Value::from(vec![answer::DENY.to_owned()]));
    b.set(b.turn, attr::STATUS, TurnStatus::Running);
    b.end_turn();
    assert_eq!(b.rules(), vec![]);
    assert_eq!(b.history_rules(), vec![Rule::CloseOrder]);
}

#[test]
fn ended_before_started_is_reported() {
    let b = Builder::new();
    b.set(b.turn, attr::STATUS, TurnStatus::Done);
    b.set(b.turn, attr::ENDED, Value::Int(5));
    assert_eq!(b.rules(), vec![Rule::Timestamps]);
}
