use loa_adapter_omp::native;
use loa_core::{attr, Dom, Draft, ElementKind as K, MemorySink, Node, NodeId, PatchSink, Role, TurnStatus};
use serde_json::{json, Value};

const HEADER: &str = r#"{"type":"session","version":3,"id":"session-1","timestamp":"2026-02-16T10:20:30.000Z","cwd":"/work"}"#;

fn fixture(entries: &[Value]) -> String {
    let mut file = format!("{HEADER}\n");
    let mut parent = Value::Null;
    for (index, entry) in entries.iter().enumerate() {
        let mut entry = entry.clone();
        let id = format!("entry-{index}");
        entry["id"] = json!(id);
        entry["parentId"] = parent;
        entry["timestamp"] = json!(format!("2026-02-16T10:20:{:02}.000Z", index + 31));
        parent = json!(id);
        file.push_str(&entry.to_string());
        file.push('\n');
    }
    file
}

fn imported(file: &str) -> Dom {
    let sink = MemorySink::new();
    native::import_str(file, &sink, "owner").unwrap();
    sink.dom()
}

fn nodes(dom: &Dom, kind: K) -> Vec<&Node> {
    dom.iter().filter(|node| node.kind == kind).collect()
}

fn round_trip(file: &str) -> Dom {
    let dom = imported(file);
    let exported = native::export_string(&dom).unwrap();
    assert_eq!(exported, file);
    let reimported = imported(&exported);
    assert_eq!(structure(&dom), structure(&reimported));
    reimported
}

type NodeShape = (K, String, Vec<(String, String)>);

fn structure(dom: &Dom) -> Vec<NodeShape> {
    dom.iter().map(|node| {
        let attrs = node.attrs.iter().filter(|(key, _)| !key.starts_with("x-"))
            .map(|(key, value)| (key.clone(), format!("{value:?}"))).collect();
        (node.kind, node.text.clone(), attrs)
    }).collect()
}

#[test]
fn captured_omp_session_round_trips() {
    // Captured from omp 18.22.2 with a single no-tools prompt. Local paths and
    // IDs were replaced, credential/provider metadata removed, and custom
    // payloads cleared without changing the remaining entry chain.
    let file = include_str!("fixtures/omp-18.22.2-session.jsonl").replace("\r\n", "\n");
    let dom = round_trip(&file);
    let session = dom.root().unwrap();
    assert_eq!(session.str_attr(attr::CWD), Some("/workspace/omp-round-trip"));
    assert_eq!(session.str_attr(attr::MODEL), Some("openai-codex/gpt-6.2-sol"));
    assert_eq!(session.str_attr(attr::EFFORT), Some("medium"));

    let [turn]: [&Node; 1] = nodes(&dom, K::Turn).try_into().unwrap();
    assert_eq!(turn.str_attr(attr::STATUS), Some("done"));
    let [user, assistant]: [&Node; 2] = nodes(&dom, K::Message).try_into().unwrap();
    assert_eq!(user.str_attr(attr::ROLE), Some("user"));
    assert_eq!(user.text, "Reply with exactly this text and nothing else: OMP round-trip fixture");
    assert_eq!(assistant.str_attr(attr::ROLE), Some("assistant"));
    assert_eq!(assistant.text, "OMP round-trip fixture");
    let [usage]: [&Node; 1] = nodes(&dom, K::Usage).try_into().unwrap();
    assert_eq!(usage.int_attr(attr::OUTPUT_TOKENS), Some(8));
}

#[test]
fn all_message_roles_blocks_calls_results_and_usage() {
    let file = fixture(&[
        json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":"hello"},{"type":"text","text":"world"}]}}),
        json!({"type":"message","message":{"role":"assistant","provider":"acme","model":"large","content":[{"type":"thinking","thinking":"reason"},{"type":"text","text":"answer"},{"type":"toolCall","id":"call-1","name":"bash","intent":"list","arguments":{"command":"pwd"}}],"usage":{"input":12,"output":5,"cacheRead":3,"cacheWrite":2,"cost":{"total":0.25}},"stopReason":"toolUse"}}),
        json!({"type":"message","message":{"role":"toolResult","toolCallId":"call-1","content":[{"type":"text","text":"/work"}],"isError":false}}),
        json!({"type":"message","message":{"role":"assistant","provider":"acme","model":"large","content":[{"type":"text","text":"done"}],"stopReason":"stop"}}),
    ]);
    let dom = round_trip(&file);
    let turns = nodes(&dom, K::Turn);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].str_attr(attr::STATUS), Some("done"));
    let messages = nodes(&dom, K::Message);
    assert_eq!(messages.iter().map(|node| node.text.as_str()).collect::<Vec<_>>(), ["hello\nworld", "reason", "answer", "done"]);
    assert_eq!(messages[1].str_attr(attr::KIND), Some("thinking"));
    assert_eq!(messages[2].str_attr(attr::MODEL), Some("acme/large"));
    let calls = nodes(&dom, K::Call);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].str_attr(attr::CALL_ID), Some("call-1"));
    assert_eq!(calls[0].str_attr(attr::STATUS), Some("done"));
    assert_eq!(nodes(&dom, K::Input)[0].text, r#"{"command":"pwd"}"#);
    assert_eq!(nodes(&dom, K::Result)[0].text, "/work");
    assert_eq!(nodes(&dom, K::Usage)[0].int_attr(attr::INPUT_TOKENS), Some(12));
}

#[test]
fn taxonomy_changes_compaction_and_unknown_entries() {
    let file = fixture(&[
        json!({"type":"model_change","role":"default","model":"provider/model"}),
        json!({"type":"model_change","role":"fast","model":"ignored"}),
        json!({"type":"thinking_level_change","thinkingLevel":"high"}),
        json!({"type":"mode_change","mode":"plan"}),
        json!({"type":"title_change","title":"new title"}),
        json!({"type":"message","message":{"role":"user","content":"question"}}),
        json!({"type":"compaction","summary":"compressed","firstKeptEntryId":"entry-5","tokensBefore":900}),
        json!({"type":"message","message":{"role":"custom","content":"hook"}}),
        json!({"type":"custom","data":{"nested":true}}),
        json!({"type":"message","message":{"role":"user","synthetic":true,"content":"hidden"}}),
        json!({"type":"message","message":{"role":"user","attribution":"agent","content":"agent input"}}),
    ]);
    let dom = round_trip(&file);
    let session = dom.root().unwrap();
    assert_eq!(session.str_attr(attr::MODEL), Some("provider/model"));
    assert_eq!(session.str_attr(attr::EFFORT), Some("high"));
    assert_eq!(session.str_attr(attr::MODE), Some("plan"));
    assert_eq!(session.str_attr(attr::TITLE), Some("new title"));
    let compaction = nodes(&dom, K::Compaction)[0];
    assert_eq!(compaction.text, "compressed");
    assert_eq!(compaction.int_attr(attr::TOKENS_BEFORE), Some(900));
    assert_eq!(nodes(&dom, K::Message).len(), 1);
    assert_eq!(nodes(&dom, K::Ext).len(), 9);
}

#[test]
fn assistant_without_user_error_aborted_and_orphan_result() {
    let file = fixture(&[
        json!({"type":"message","message":{"role":"toolResult","toolCallId":"missing","content":"orphan"}}),
        json!({"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"failed"}],"stopReason":"error"}}),
        json!({"type":"message","message":{"role":"user","content":"next"}}),
        json!({"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"c","name":"shell","arguments":{}}],"stopReason":"aborted"}}),
        json!({"type":"message","message":{"role":"toolResult","toolCallId":"c","content":"oops","isError":true}}),
    ]);
    let dom = round_trip(&file);
    let turns = nodes(&dom, K::Turn);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].str_attr(attr::STATUS), Some("error"));
    assert_eq!(turns[1].str_attr(attr::STATUS), Some("cancelled"));
    assert_eq!(nodes(&dom, K::Result)[0].bool_attr(attr::IS_ERROR), Some(true));
    assert_eq!(nodes(&dom, K::Call)[0].str_attr(attr::STATUS), Some("error"));
    assert!(nodes(&dom, K::Ext)[0].text.contains("orphan"));
}

#[test]
fn title_slot_branches_and_lenient_lines_preserve_all_valid_entries() {
    let title = r#"{"type":"title","title":"from slot"}"#;
    let header = r#"{"type":"session","version":3,"id":"branch","timestamp":"2026-02-16T10:20:30Z","cwd":"/tmp"}"#;
    let a = r#"{"type":"message","id":"a","parentId":null,"message":{"role":"user","content":"first"}}"#;
    let b = r#"{"type":"message","id":"b","parentId":"a","message":{"role":"assistant","content":[{"type":"text","text":"abandoned"}]}}"#;
    let c = r#"{"type":"message","id":"c","parentId":"a","message":{"role":"assistant","content":[{"type":"text","text":"chosen"}]}}"#;
    let file = format!("{title}\n{header}\n{a}\nnot json\n{b}\n{c}\n");
    let expected = format!("{title}\n{header}\n{a}\n{b}\n{c}\n");
    let dom = imported(&file);
    assert_eq!(dom.root().unwrap().str_attr(attr::TITLE), Some("from slot"));
    assert_eq!(nodes(&dom, K::Message).iter().map(|node| node.text.as_str()).collect::<Vec<_>>(), ["first", "chosen"]);
    let branch = nodes(&dom, K::Ext).into_iter().find(|node| node.str_attr(attr::TYPE) == Some("branch")).unwrap();
    assert_eq!(branch.text, b);
    assert_eq!(native::export_string(&dom).unwrap(), expected);
    assert_eq!(structure(&dom), structure(&imported(&expected)));
}

#[test]
fn v1_migration_linear_ids_compaction_and_hook_role() {
    let file = concat!(
        "{\"type\":\"session\",\"version\":1,\"id\":\"old\",\"timestamp\":\"2020-01-01T00:00:00Z\"}\n",
        "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
        "{\"type\":\"compaction\",\"summary\":\"summary\",\"firstKeptEntryIndex\":1}\n",
        "{\"type\":\"message\",\"message\":{\"role\":\"hookMessage\",\"content\":\"custom\"}}\n",
    );
    let dom = imported(file);
    let output = native::export_string(&dom).unwrap();
    let lines: Vec<Value> = output.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(lines[0]["version"], 3);
    assert_eq!(lines[1]["id"], "00000001");
    assert!(lines[1]["parentId"].is_null());
    assert_eq!(lines[2]["parentId"], "00000001");
    assert_eq!(lines[2]["firstKeptEntryId"], "00000001");
    assert!(lines[2].get("firstKeptEntryIndex").is_none());
    assert_eq!(lines[3]["message"]["role"], "custom");
    assert_eq!(structure(&dom), structure(&imported(&output)));
}

#[test]
fn v2_migration_keeps_branches_and_converts_hook_role() {
    let file = concat!(
        "{\"type\":\"session\",\"version\":2,\"id\":\"old\"}\n",
        "{\"type\":\"message\",\"id\":\"a\",\"parentId\":null,\"message\":{\"role\":\"hookMessage\",\"content\":\"hook\"}}\n",
        "{\"type\":\"message\",\"id\":\"b\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"active\"}}\n",
    );
    let output = native::export_string(&imported(file)).unwrap();
    let lines: Vec<Value> = output.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(lines[0]["version"], 3);
    assert_eq!(lines[1]["id"], "a");
    assert_eq!(lines[1]["message"]["role"], "custom");
    assert_eq!(nodes(&imported(&output), K::Message)[0].text, "active");
}

#[test]
fn export_file_import_file_and_live_dom_round_trip() {
    let sink = MemorySink::new();
    let session = sink.commit_at(Draft::insert(NodeId::DOCUMENT, K::Session, vec![
        (attr::ID, "live".into()), (attr::HARNESS, "omp".into()), (attr::NODE, "owner".into()),
        (attr::CREATED, 1_771_234_000_000_i64.into()), (attr::CWD, "/work".into()),
    ], ""), 1_771_234_000_000).unwrap().node;
    let turn = sink.commit_at(Draft::insert(session, K::Turn, vec![
        (attr::STATUS, TurnStatus::Done.into()), (attr::STARTED, 1_771_234_000_001_i64.into()),
    ], ""), 1_771_234_000_001).unwrap().node;
    sink.commit_at(Draft::insert(turn, K::Message, vec![(attr::ROLE, Role::User.into())], "hi"), 1_771_234_000_001).unwrap();
    sink.commit_at(Draft::insert(turn, K::Message, vec![(attr::ROLE, Role::Assistant.into()), (attr::MODEL, "acme/large".into())], "hello"), 1_771_234_000_002).unwrap();
    let call = sink.commit_at(Draft::insert(turn, K::Call, vec![
        (attr::TOOL, "shell".into()), (attr::CALL_ID, "call-live".into()),
    ], ""), 1_771_234_000_002).unwrap().node;
    sink.commit_at(Draft::insert(call, K::Input, vec![(attr::FORMAT, "json".into())], r#"{"command":"pwd"}"#), 1_771_234_000_002).unwrap();
    sink.commit_at(Draft::insert(call, K::Result, vec![(attr::IS_ERROR, false.into())], "/work"), 1_771_234_000_003).unwrap();
    let assistant = nodes(&sink.dom(), K::Message).last().unwrap().id;
    sink.commit_at(Draft::insert(assistant, K::Usage, vec![
        (attr::INPUT_TOKENS, 10_i64.into()), (attr::OUTPUT_TOKENS, 4_i64.into()),
        (attr::COST, 0.125_f64.into()),
    ], ""), 1_771_234_000_002).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    native::export(&sink.dom(), &path).unwrap();
    let imported_sink = MemorySink::new();
    native::import(&path, &imported_sink, "owner").unwrap();
    let dom = imported_sink.dom();
    assert_eq!(dom.root().unwrap().str_attr(attr::ID), Some("live"));
    assert_eq!(nodes(&dom, K::Turn).len(), 1);
    assert_eq!(nodes(&dom, K::Message).iter().map(|message| message.text.as_str()).collect::<Vec<_>>(), ["hi", "hello"]);
    assert_eq!(nodes(&dom, K::Call)[0].str_attr(attr::CALL_ID), Some("call-live"));
    assert_eq!(nodes(&dom, K::Input)[0].text, r#"{"command":"pwd"}"#);
    assert_eq!(nodes(&dom, K::Result)[0].text, "/work");
    assert_eq!(nodes(&dom, K::Usage)[0].int_attr(attr::INPUT_TOKENS), Some(10));
    assert_eq!(nodes(&dom, K::Usage)[0].int_attr(attr::OUTPUT_TOKENS), Some(4));
    let exported = std::fs::read_to_string(path).unwrap();
    assert_eq!(native::export_string(&dom).unwrap(), exported);
}
