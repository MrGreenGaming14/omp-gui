//! The `testing` checks accept a well-formed session and flag a broken one.

use loa_harness::testing::{Phase, Rule, WatchSink, check_dom, check_dom_in, check_history};
use loa_core::{CallStatus, Draft, ElementKind, MessageStatus, NodeId, PatchSink, Role, SCHEMA_VERSION, TurnStatus, Value, attr};

fn insert(sink: &WatchSink, parent: NodeId, kind: ElementKind, attrs: Vec<(&str, Value)>, text: &str) -> NodeId {
    sink.commit_at(Draft::insert(parent, kind, attrs, text), 30).unwrap().node
}

fn session_with_turn(sink: &WatchSink) -> NodeId {
    let attrs = vec![
        (attr::ID, "s".into()),
        (attr::HARNESS, "h".into()),
        (attr::NODE, "n".into()),
        (attr::CREATED, Value::Int(10)),
        (attr::SCHEMA, Value::Int(i64::from(SCHEMA_VERSION))),
    ];
    let session = insert(sink, NodeId::DOCUMENT, ElementKind::Session, attrs, "");
    let turn = insert(sink, session, ElementKind::Turn, vec![(attr::STATUS, TurnStatus::Running.into())], "");
    let user = vec![(attr::ROLE, Role::User.into()), (attr::STATUS, MessageStatus::Done.into())];
    insert(sink, turn, ElementKind::Message, user, "hi");
    turn
}

fn rules(sink: &WatchSink) -> Vec<Rule> {
    sink.inner.with_dom(check_dom).into_iter().map(|v| v.rule).collect()
}

#[test]
fn a_well_formed_session_passes_and_a_running_turn_is_only_live() {
    let sink = WatchSink::default();
    let turn = session_with_turn(&sink);
    assert_eq!(rules(&sink), vec![Rule::NotTerminal]);
    assert_eq!(sink.inner.with_dom(|d| check_dom_in(d, Phase::Live)), vec![]);
    let call = insert(&sink, turn, ElementKind::Call, vec![(attr::TOOL, "bash".into()), (attr::STATUS, CallStatus::Running.into())], "");
    insert(&sink, call, ElementKind::Input, vec![], "{}");
    insert(&sink, call, ElementKind::Result, vec![(attr::IS_ERROR, false.into())], "ok");
    sink.commit_at(Draft::set(call, attr::STATUS, CallStatus::Done), 40).unwrap();
    sink.commit_at(Draft::set(turn, attr::STATUS, TurnStatus::Done), 50).unwrap();
    assert_eq!(*sink.seq.borrow(), sink.inner.patches().last().unwrap().seq);
    assert_eq!(rules(&sink), vec![]);
    assert_eq!(check_history(&sink.inner.patches()), vec![]);
}

#[test]
fn a_done_call_without_input_or_result_is_flagged() {
    let sink = WatchSink::default();
    let turn = session_with_turn(&sink);
    insert(&sink, turn, ElementKind::Call, vec![(attr::TOOL, "bash".into()), (attr::STATUS, CallStatus::Done.into())], "");
    sink.commit_at(Draft::set(turn, attr::STATUS, TurnStatus::Done), 50).unwrap();
    assert_eq!(rules(&sink), vec![Rule::CallInput, Rule::CallResult]);
}
