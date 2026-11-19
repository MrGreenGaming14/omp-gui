use loa_adapter::{Answer, Prompt};
use loa_adapter_omp::mapping::Mapping;
use loa_core::{attr, Draft, ElementKind as K, MemorySink, NodeId, PatchSink};
use loa_omp_rpc::{AssistantEvent, Event, PromptStatus, UiRequest};
use serde_json::json;

fn commit(sink: &MemorySink, mapper: &mut Mapping, drafts: Vec<Draft>) {
    let mut pending = drafts;
    while !pending.is_empty() {
        let mut next = Vec::new();
        for draft in pending {
            let patch = sink.commit(draft).unwrap();
            next.extend(mapper.committed(&[patch]));
        }
        pending = next;
    }
}

fn fixture() -> (MemorySink, Mapping) {
    let sink = MemorySink::new();
    let root = sink.commit(Draft::insert(NodeId::DOCUMENT, K::Session, vec![
        (attr::ID, "s".into()), (attr::HARNESS, "omp".into()),
        (attr::NODE, "owner".into()), (attr::CREATED, 1_i64.into()),
        (attr::SCHEMA, 1_i64.into()),
    ], "")).unwrap().node;
    (sink, Mapping::new(root))
}

#[test]
fn streamed_text_and_tool_result_survive_prompt_completion() {
    let (sink, mut mapper) = fixture();
    let drafts = mapper_sent(&mut mapper);
    commit(&sink, &mut mapper, drafts);
    for event in [
        Event::MessageUpdate {
            role: "assistant".into(), message_id: Some("a".into()),
            event: AssistantEvent::TextStart { content_index: 0 },
        },
        Event::MessageUpdate {
            role: "assistant".into(), message_id: Some("a".into()),
            event: AssistantEvent::TextDelta { content_index: 0, delta: "hello".into() },
        },
        Event::ToolExecutionStart {
            tool_call_id: "c".into(), tool_name: "read".into(), args: Some(json!({"path":"a.rs"})), intent: None,
        },
        Event::ToolExecutionEnd {
            tool_call_id: "c".into(), tool_name: "read".into(), result: Some(json!("contents")), is_error: false,
        },
        Event::PromptResult {
            id: Some("p".into()), agent_invoked: true, status: PromptStatus::Completed,
            error: None, session_settled: true,
        },
    ] {
        let drafts = mapper.event(&event);
        commit(&sink, &mut mapper, drafts);
    }
    let dom = sink.dom();
    assert!(dom.iter().any(|n| n.kind == K::Message && n.text == "hello"));
    assert!(dom.iter().any(|n| n.kind == K::Result && n.text == "contents"));
    assert!(dom.iter().any(|n| n.kind == K::Turn && n.str_attr(attr::STATUS) == Some("done")));
}

fn mapper_sent(mapper: &mut Mapping) -> Vec<Draft> {
    mapper.sent(&Prompt { text: "read a.rs".into(), by: "owner".into(), ..Prompt::default() })
}

#[test]
fn approval_request_and_answer_updates_decision_and_turn() {
    let (sink, mut mapper) = fixture();
    let drafts = mapper_sent(&mut mapper);
    commit(&sink, &mut mapper, drafts);
    let drafts = mapper.ui(&UiRequest::Confirm {
        id: "approval-1".into(), title: "Run command?".into(),
        message: "write to disk".into(), timeout: None,
    });
    commit(&sink, &mut mapper, drafts);
    let decision = *mapper.decisions.get("approval-1").unwrap();
    let dom = sink.dom();
    assert_eq!(dom.get(decision).unwrap().str_attr(attr::STATUS), Some("waiting"));
    let drafts = mapper.answered(decision, &Answer { values: vec!["allow_once".into()], by: "owner".into(), reason: None });
    commit(&sink, &mut mapper, drafts);
    let dom = sink.dom();
    assert_eq!(dom.get(decision).unwrap().str_attr(attr::STATUS), Some("answered"));
    assert!(dom.iter().any(|n| n.kind == K::Turn && n.str_attr(attr::STATUS) == Some("running")));
}
