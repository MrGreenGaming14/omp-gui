use loa_contracts::harness::{Answer, Prompt};
use loa_adapter_omp::mapping::{COMMANDS, Mapping};
use loa_core::{answer, attr, Draft, ElementKind as K, MemorySink, NodeId, PatchSink};
use loa_omp_rpc::{AssistantEvent, Event, Frame, PromptStatus, UiRequest, UiResponse};
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
    mapper.queue("p", &Prompt { text: "read a.rs".into(), by: "owner".into(), ..Prompt::default() });
    Vec::new()
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

/// Replays a fake-omp script (`tests/fixtures/omp19`) into the mapper: `>`
/// frames are fed in order, `< prompt NAME` registers a prompt sent as command
/// NAME, and a placeholder `{{NAME}}` reads as `NAME`.
fn replay(sink: &MemorySink, mapper: &mut Mapping, script: &str) {
    for line in script.lines() {
        if let Some(step) = line.strip_prefix("< ") {
            if let Some(name) = step.strip_prefix("prompt ") {
                mapper.queue(name, &Prompt { text: format!("prompt {name}"), by: "owner".into(), ..Prompt::default() });
            }
            continue;
        }
        let Some(frame) = line.strip_prefix("> ") else { continue };
        let frame = frame.replace("\"{{", "\"").replace("}}\"", "\"");
        let drafts = match Frame::parse(frame.as_bytes()).unwrap() {
            Frame::Event(event) => mapper.event(&event),
            Frame::Ui(request) => mapper.ui(&request),
            _ => continue,
        };
        commit(sink, mapper, drafts);
    }
}

const APPROVAL: &str = include_str!("fixtures/omp19/approval.script");
const APPROVED: &str = include_str!("fixtures/omp19/approved.script");
const ABORT: &str = include_str!("fixtures/omp19/abort.script");
const ABORT_QUEUED: &str = include_str!("fixtures/omp19/abort-queued.script");
const FOLLOW_UP: &str = include_str!("fixtures/omp19/follow-up.script");

fn ans(values: &[&str], reason: Option<&str>) -> Answer {
    Answer { values: values.iter().map(|v| (*v).to_owned()).collect(), by: "owner".into(), reason: reason.map(str::to_owned) }
}

fn statuses(dom: &loa_core::Dom, kind: K) -> Vec<String> {
    dom.iter().filter(|n| n.kind == kind).map(|n| n.str_attr(attr::STATUS).unwrap_or_default().to_owned()).collect()
}

#[test]
fn omps_tool_approval_select_is_an_approval_card_answered_with_its_labels() {
    let (sink, mut mapper) = fixture();
    replay(&sink, &mut mapper, APPROVAL);
    let dom = sink.dom();
    let decision = dom.iter().find(|n| n.kind == K::Decision).expect("a decision");
    assert_eq!(decision.str_attr(attr::KIND), Some("approval"));
    assert_eq!(decision.str_attr(attr::TITLE), Some("Allow tool: bash"));
    assert!(decision.text.contains("Command: echo loa-approved-7731"));
    let options: Vec<&str> = decision.attrs.get(attr::OPTIONS).unwrap().str_items().collect();
    assert_eq!(options, [answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY]);
    let id = decision.id;
    let value = |answer: Answer| match mapper.reply(id, &answer).unwrap().response {
        UiResponse::Value { value, .. } => value,
        other => panic!("{other:?}"),
    };
    // omp denies any value that isn't one of its labels.
    assert_eq!(value(ans(&[answer::ALLOW_ONCE], None)), "Approve");
    assert_eq!(value(ans(&[answer::ALLOW_SESSION], None)), "Approve bash for this session");
    assert_eq!(value(ans(&[answer::DENY], None)), "Deny");
    assert_eq!(mapper.reply(id, &ans(&[answer::DENY], None)).unwrap().steer, None);
    let denied = mapper.reply(id, &ans(&[answer::DENY], Some("not on this machine"))).unwrap();
    assert_eq!(denied.steer.as_deref(), Some("I denied that tool call: not on this machine"));
    assert!(mapper.reply(id, &ans(&["maybe"], None)).is_err());
    assert_eq!(mapper.reply(id, &ans(&[answer::ALLOW_ONCE], Some("ignored"))).unwrap().steer, None);
}

#[test]
fn an_approval_select_with_grants_offers_no_session_answer() {
    let (sink, mut mapper) = fixture();
    mapper.queue("p", &Prompt { text: "read".into(), by: "owner".into(), ..Prompt::default() });
    let frame = r#"{"type":"extension_ui_request","id":"g1","method":"select","title":"Allow tool: read","options":["Approve File","Always Approve File","Approve Once","Deny"]}"#;
    let Frame::Ui(request) = Frame::parse(frame.as_bytes()).unwrap() else { panic!() };
    let drafts = mapper.ui(&request);
    commit(&sink, &mut mapper, drafts);
    let dom = sink.dom();
    let decision = dom.iter().find(|n| n.kind == K::Decision).unwrap();
    let options: Vec<&str> = decision.attrs.get(attr::OPTIONS).unwrap().str_items().collect();
    assert_eq!(options, [answer::ALLOW_ONCE, answer::DENY]);
    let reply = mapper.reply(decision.id, &ans(&[answer::ALLOW_ONCE], None)).unwrap();
    assert_eq!(reply.response, UiResponse::Value { id: "g1".into(), value: "Approve Once".into() });
    assert!(mapper.reply(decision.id, &ans(&[answer::ALLOW_SESSION], None)).is_err());
    // A grant passes through by its label.
    let reply = mapper.reply(decision.id, &ans(&["Approve File"], None)).unwrap();
    assert_eq!(reply.response, UiResponse::Value { id: "g1".into(), value: "Approve File".into() });
}

#[test]
fn an_approved_bash_call_keeps_the_command_output_as_its_result() {
    let (sink, mut mapper) = fixture();
    replay(&sink, &mut mapper, APPROVAL);
    let decision = sink.dom().iter().find(|n| n.kind == K::Decision).unwrap().id;
    let drafts = mapper.answered(decision, &ans(&[answer::ALLOW_ONCE], None));
    commit(&sink, &mut mapper, drafts);
    replay(&sink, &mut mapper, APPROVED);
    let dom = sink.dom();
    let call = dom.iter().find(|n| n.kind == K::Call).unwrap();
    assert_eq!(call.str_attr(attr::STATUS), Some("done"));
    let result = dom.children_of_kind(call.id, K::Result).next().expect("a result");
    assert_eq!(result.text, "loa-approved-7731\n");
    let job = dom.iter().find(|n| n.kind == K::Job).unwrap();
    assert_eq!((job.text.as_str(), job.str_attr(attr::STATUS)), ("loa-approved-7731\n", Some("done")));
    // One prompt, one turn: omp's model steps (turn_end) stay inside it.
    assert_eq!(statuses(&dom, K::Turn), ["done"]);
    assert!(dom.iter().any(|n| n.kind == K::Message && n.text == "The command printed `loa-approved-7731`."));
    assert!(dom.iter().any(|n| n.kind == K::Message && n.str_attr(attr::ROLE) == Some("user") && n.text == "prompt p"));
}

#[test]
fn an_aborted_stream_cancels_its_turn_and_leaves_nothing_streaming() {
    let (sink, mut mapper) = fixture();
    replay(&sink, &mut mapper, ABORT);
    let dom = sink.dom();
    assert_eq!(statuses(&dom, K::Turn), ["cancelled"]);
    let text = dom.iter().find(|n| n.kind == K::Message && n.str_attr(attr::KIND) == Some("text") && n.str_attr(attr::ROLE) == Some("assistant")).unwrap();
    assert_eq!(text.text, "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11");
    assert_eq!(text.str_attr(attr::STATUS), Some("cancelled"));
    let thinking = dom.iter().find(|n| n.kind == K::Message && n.str_attr(attr::KIND) == Some("thinking")).unwrap();
    assert_eq!(thinking.str_attr(attr::STATUS), Some("done"), "a finished block stays done");
    assert!(dom.iter().all(|n| !matches!(n.str_attr(attr::STATUS), Some("streaming" | "running"))));
    assert!(mapper.is_idle());
}

#[test]
fn a_follow_up_runs_as_its_own_turn_after_the_first() {
    let (sink, mut mapper) = fixture();
    replay(&sink, &mut mapper, FOLLOW_UP);
    let dom = sink.dom();
    let turns: Vec<_> = dom.iter().filter(|n| n.kind == K::Turn).collect();
    assert_eq!(turns.len(), 2);
    let texts = |turn: loa_core::NodeId| -> Vec<(String, String)> {
        dom.children_of_kind(turn, K::Message).map(|m| (m.str_attr(attr::ROLE).unwrap_or_default().to_owned(), m.text.clone())).collect()
    };
    assert_eq!(texts(turns[0].id), [("user".to_owned(), "prompt p".to_owned()), ("assistant".to_owned(), "1\n2\n3\n4\n5\n6".to_owned())]);
    assert_eq!(texts(turns[1].id), [("user".to_owned(), "prompt p2".to_owned()), ("assistant".to_owned(), "second".to_owned())]);
    assert_eq!(statuses(&dom, K::Turn), ["done", "done"]);
    assert!(mapper.is_idle());
}

#[test]
fn an_abort_withdrawing_a_queued_prompt_records_it_as_a_cancelled_turn() {
    let (sink, mut mapper) = fixture();
    replay(&sink, &mut mapper, ABORT_QUEUED);
    let dom = sink.dom();
    let turns: Vec<_> = dom.iter().filter(|n| n.kind == K::Turn).collect();
    assert_eq!(statuses(&dom, K::Turn), ["cancelled", "cancelled"]);
    let user = |turn: loa_core::NodeId| dom.children_of_kind(turn, K::Message).find(|m| m.str_attr(attr::ROLE) == Some("user")).map(|m| m.text.clone());
    assert_eq!(user(turns[0].id).as_deref(), Some("prompt p"));
    assert_eq!(user(turns[1].id).as_deref(), Some("prompt p2"), "the withdrawn prompt isn't lost");
    assert!(dom.iter().all(|n| !matches!(n.str_attr(attr::STATUS), Some("streaming" | "running"))));
    assert!(mapper.is_idle());
}

fn ask_card(questions: serde_json::Value) -> (MemorySink, Mapping, loa_core::NodeId) {
    let (sink, mut mapper) = fixture();
    mapper.queue("p", &Prompt { text: "pick".into(), by: "owner".into(), ..Prompt::default() });
    let frame = json!({"type":"extension_ui_request","id":"ask-1","method":"ask","questions":questions}).to_string();
    let Frame::Ui(request) = Frame::parse(frame.as_bytes()).unwrap() else { panic!() };
    let drafts = mapper.ui(&request);
    commit(&sink, &mut mapper, drafts);
    let decision = sink.dom().iter().find(|n| n.kind == K::Decision).unwrap().id;
    (sink, mapper, decision)
}

fn answers(mapper: &Mapping, decision: loa_core::NodeId, values: &[&str]) -> serde_json::Value {
    match mapper.reply(decision, &ans(values, None)).unwrap().response {
        UiResponse::Answers { answers, .. } => serde_json::to_value(answers).unwrap(),
        UiResponse::Cancelled { .. } => serde_json::Value::Null,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_single_question_ask_is_a_question_card() {
    let (sink, mapper, decision) = ask_card(json!([{"id":"db","header":"Database","question":"Which database?","options":[{"label":"Postgres"},{"label":"SQLite"}],"multi":true}]));
    let dom = sink.dom();
    let node = dom.get(decision).unwrap();
    assert_eq!(node.str_attr(attr::KIND), Some("question"));
    assert_eq!(node.str_attr(attr::TITLE), Some("Database"));
    assert_eq!(node.text, "Which database?");
    assert_eq!(node.attrs.get(attr::OPTIONS).unwrap().str_items().collect::<Vec<_>>(), ["Postgres", "SQLite"]);
    assert_eq!(node.bool_attr(attr::MULTI), Some(true));
    assert_eq!(answers(&mapper, decision, &["Postgres", "SQLite"]), json!([{"id":"db","selectedOptions":["Postgres","SQLite"]}]));
    assert_eq!(answers(&mapper, decision, &["DuckDB"]), json!([{"id":"db","selectedOptions":[],"customInput":"DuckDB"}]));
    assert_eq!(answers(&mapper, decision, &[]), serde_json::Value::Null);
}

#[test]
fn a_multi_question_ask_is_a_form_card_with_fields() {
    let (sink, mapper, decision) = ask_card(json!([
        {"id":"db","question":"Which database?","header":"Storage","options":[{"label":"Postgres"},{"label":"SQLite"}],"recommended":1},
        {"id":"features","question":"Which features?","options":[{"label":"Auth"},{"label":"Search"}],"multi":true},
        {"id":"name","question":"Project name?","options":[]}
    ]));
    let dom = sink.dom();
    let node = dom.get(decision).unwrap();
    assert_eq!(node.str_attr(attr::KIND), Some("form"));
    let fields: serde_json::Value = serde_json::from_str(node.str_attr(attr::FIELDS).unwrap()).unwrap();
    assert_eq!(fields, json!({
        "type": "object",
        "properties": {
            "db": {"title": "Which database?", "description": "Storage", "type": "string", "enum": ["Postgres", "SQLite"], "default": "SQLite"},
            "features": {"title": "Which features?", "type": "array", "items": {"type": "string", "enum": ["Auth", "Search"]}},
            "name": {"title": "Project name?", "type": "string"}
        },
        "required": ["db", "features", "name"]
    }));
    let form = r#"{"db":"Postgres","features":["Auth","Search"],"name":"loa"}"#;
    assert_eq!(answers(&mapper, decision, &[form]), json!([
        {"id":"db","selectedOptions":["Postgres"]},
        {"id":"features","selectedOptions":["Auth","Search"]},
        {"id":"name","selectedOptions":[],"customInput":"loa"}
    ]));
    assert_eq!(answers(&mapper, decision, &["not json"]), serde_json::Value::Null);
    assert_eq!(answers(&mapper, decision, &[form, form]), serde_json::Value::Null);
}

const TUI_NOISE: &str = include_str!("fixtures/omp19/tui-noise.script");

#[test]
fn omp_tui_bookkeeping_adds_no_elements_and_usage_lands_on_the_reply() {
    let (sink, mut mapper) = fixture();
    replay(&sink, &mut mapper, TUI_NOISE);
    let dom = sink.dom();
    let kinds: Vec<K> = dom.iter().filter(|n| n.str_attr(attr::TYPE) != Some(COMMANDS)).map(|n| n.kind).collect();
    assert!(!kinds.contains(&K::Ext) && !kinds.contains(&K::Call), "{kinds:?}");
    let turn = dom.iter().find(|n| n.kind == K::Turn).unwrap();
    assert_eq!(turn.str_attr(attr::STATUS), Some("done"));
    let reply = dom.children_of_kind(turn.id, K::Message).find(|m| m.str_attr(attr::ROLE) == Some("assistant")).unwrap();
    assert!(!reply.text.is_empty());
    let usage = dom.children_of_kind(reply.id, K::Usage).next().expect("usage on the reply");
    assert_eq!(usage.attrs.get(attr::INPUT_TOKENS), Some(&loa_core::Value::Int(4)));
    assert_eq!(usage.attrs.get(attr::OUTPUT_TOKENS), Some(&loa_core::Value::Int(6)));
    assert_eq!(usage.attrs.get(attr::CACHE_WRITE_TOKENS), Some(&loa_core::Value::Int(11487)));
    assert_eq!(usage.str_attr(attr::MODEL), Some("anthropic/claude-haiku-5-5"));
    let Some(loa_core::Value::Float(cost)) = usage.attrs.get(attr::COST) else { panic!("cost: {:?}", usage.attrs) };
    assert!((cost - 0.0023008).abs() < 1e-9);
}

const SLASH_COMMAND: &str = include_str!("fixtures/omp19/slash-command.script");

#[test]
fn omps_command_list_is_recorded_on_the_root_and_a_command_it_runs_answers_in_its_turn() {
    let (sink, mut mapper) = fixture();
    replay(&sink, &mut mapper, SLASH_COMMAND);
    // The runtime turns the `agentInvoked: false` response into this.
    let drafts = mapper.event(&Event::PromptResult {
        id: Some("p".into()), agent_invoked: false, status: PromptStatus::Completed, error: None, session_settled: true,
    });
    commit(&sink, &mut mapper, drafts);
    let dom = sink.dom();
    let root = dom.root().unwrap().id;

    let lists: Vec<_> = dom.iter().filter(|n| n.kind == K::Ext && n.str_attr(attr::TYPE) == Some(COMMANDS)).collect();
    assert_eq!(lists.len(), 1, "a repeated list adds nothing");
    assert_eq!(lists[0].parent, root);
    assert_eq!(lists[0].str_attr(attr::HARNESS), Some("omp"));
    let listed: serde_json::Value = serde_json::from_str(&lists[0].text).unwrap();
    let names: Vec<&str> = listed["commands"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["dirs", "switch", "review"]);

    let turn = dom.iter().find(|n| n.kind == K::Turn).expect("the command's turn");
    assert_eq!(turn.str_attr(attr::STATUS), Some("done"));
    let messages: Vec<_> = dom.children_of_kind(turn.id, K::Message).map(|m| (m.str_attr(attr::ROLE), m.text.as_str())).collect();
    assert_eq!(messages, [(Some("user"), "prompt p"), (Some("assistant"), "Workspace directories:\n  /tmp (working directory)")]);
    assert_eq!(dom.iter().filter(|n| n.kind == K::Turn).count(), 1);
}

#[test]
fn command_output_read_after_the_response_closed_the_turn_joins_that_turn() {
    let (sink, mut mapper) = fixture();
    mapper.queue("p", &Prompt { text: "/dirs".into(), by: "owner".into(), ..Prompt::default() });
    let drafts = mapper.event(&Event::PromptResult {
        id: Some("p".into()), agent_invoked: false, status: PromptStatus::Completed, error: None, session_settled: true,
    });
    commit(&sink, &mut mapper, drafts);
    let Frame::Event(output) = Frame::parse(br#"{"type":"command_output","text":"Workspace directories:\n  /tmp"}"#).unwrap() else { panic!() };
    let drafts = mapper.event(&output);
    commit(&sink, &mut mapper, drafts);
    let dom = sink.dom();
    let turn = dom.iter().find(|n| n.kind == K::Turn).unwrap();
    let messages: Vec<_> = dom.children_of_kind(turn.id, K::Message).map(|m| (m.str_attr(attr::ROLE), m.text.as_str())).collect();
    assert_eq!(messages, [(Some("user"), "/dirs"), (Some("assistant"), "Workspace directories:\n  /tmp")]);
    assert!(!dom.iter().any(|n| n.kind == K::Ext));
}

#[test]
fn fire_and_forget_ui_requests_map_to_their_meaning() {
    let (sink, mut mapper) = fixture();
    for frame in [
        r#"{"type":"extension_ui_request","id":"u1","method":"setTitle","title":"Fix the parser"}"#,
        r#"{"type":"extension_ui_request","id":"u2","method":"notify","message":"Index rebuilt","notifyType":"info"}"#,
        r#"{"type":"extension_ui_request","id":"u3","method":"open_url","url":"https://example.com/login","instructions":"Sign in"}"#,
        r#"{"type":"extension_ui_request","id":"u4","method":"setStatus","statusKey":"tokens","statusText":"12k"}"#,
        r#"{"type":"extension_ui_request","id":"u5","method":"set_editor_text","text":"draft"}"#,
    ] {
        let Frame::Ui(request) = Frame::parse(frame.as_bytes()).unwrap() else { panic!("{frame}") };
        let drafts = mapper.ui(&request);
        commit(&sink, &mut mapper, drafts);
    }
    let dom = sink.dom();
    assert_eq!(dom.root().unwrap().str_attr(attr::TITLE), Some("Fix the parser"));
    let exts: Vec<&str> = dom.iter().filter(|n| n.kind == K::Ext).filter_map(|n| n.str_attr(attr::TYPE)).collect();
    assert_eq!(exts, ["notice", "open_url"]);
    assert!(dom.iter().any(|n| n.kind == K::Ext && n.text.contains("Index rebuilt")));
    assert!(dom.iter().any(|n| n.kind == K::Ext && n.text.contains("https://example.com/login")));
}
