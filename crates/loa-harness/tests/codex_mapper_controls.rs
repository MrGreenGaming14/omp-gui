use loa_contracts::harness::{Answer, Prompt, SessionConfig, SessionState};
use loa_harness::codex::mapper::{Mapper, Reply};
use loa_core::{ElementKind, MemorySink, PatchSink, attr};
use serde_json::json;
use std::sync::Arc;

fn mapper() -> (Mapper, Arc<MemorySink>) {
    let sink = Arc::new(MemorySink::new());
    let writer: Arc<dyn PatchSink> = sink.clone();
    let config = SessionConfig { session_id: "invented-session".into(), node: "invented-node".into(), cwd: "/invented".into(), ..SessionConfig::default() };
    let mapper = Mapper::start(&config, "invented-thread", None, writer).expect("session");
    (mapper, sink)
}

#[test]
fn interrupted_turn_cancels_pending_calls_messages_and_decisions() {
    let (mut mapper, sink) = mapper();
    mapper.begin_turn(&Prompt { text: "Inspect".into(), by: "person".into(), ..Prompt::default() }).expect("prompt");
    mapper.notification("turn/started", &json!({"turn":{"id":"turn-a"}})).expect("turn start");
    mapper.notification("item/agentMessage/delta", &json!({"turnId":"turn-a","itemId":"msg-a","delta":"partial"})).expect("stream");
    mapper.notification("item/started", &json!({"turnId":"turn-a","item":{"id":"shell-a","type":"commandExecution","command":"slow-command"}})).expect("call");
    mapper.request(json!(12), "item/commandExecution/requestApproval", &json!({"turnId":"turn-a","itemId":"shell-a","command":"slow-command"})).expect("approval");
    mapper.notification("turn/completed", &json!({"turn":{"id":"turn-a","status":"interrupted"}})).expect("interrupted");
    assert_eq!(mapper.state(), SessionState::Idle);
    let dom = sink.dom();
    for kind in [ElementKind::Turn, ElementKind::Message, ElementKind::Call, ElementKind::Decision] {
        assert!(dom.iter().any(|n| n.kind == kind && n.str_attr(attr::STATUS) == Some("cancelled")), "{kind:?} must cancel");
    }
}

#[test]
fn multi_question_request_replies_only_after_all_answers() {
    let (mut mapper, sink) = mapper();
    mapper.begin_turn(&Prompt { text: "Plan".into(), by: "person".into(), ..Prompt::default() }).expect("prompt");
    let request = json!({"turnId":"turn-q","questions":[
        {"id":"first","header":"First","question":"Which fruit?","options":[{"label":"Apple"},{"label":"Pear"}]},
        {"id":"second","header":"Second","question":"Quantity?","options":[]}
    ]});
    assert!(mapper.request(json!(42), "item/tool/requestUserInput", &request).expect("questions").is_none());
    assert_eq!(mapper.state(), SessionState::Waiting);
    let dom = sink.dom();
    let decisions: Vec<_> = dom.iter().filter(|n| n.kind == ElementKind::Decision).map(|n| n.id).collect();
    assert_eq!(decisions.len(), 2);
    assert!(mapper.answer(decisions[0], &Answer { values: vec!["Pear".into()], by: "person".into(), ..Answer::default() }).expect("first answer").is_none());
    assert_eq!(mapper.state(), SessionState::Waiting);
    let reply = mapper.answer(decisions[1], &Answer { values: vec!["4".into()], by: "person".into(), ..Answer::default() }).expect("second answer");
    assert!(matches!(reply, Some(Reply::Result { id, result }) if id == json!(42) && result == json!({"answers":{"first":{"answers":["Pear"]},"second":{"answers":["4"]}}})));
    assert_eq!(mapper.state(), SessionState::Running);
}
