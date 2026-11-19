//! Handwritten Claude Code transcript fixtures: no real user session data.

use loa_adapter::{Adapter, SessionConfig};
use loa_adapter_claude::{ClaudeAdapter, native};
use loa_core::{Draft, ElementKind, MemorySink, PatchSink, Role, Value, attr};
use serde_json::{Value as Json, json};
use std::path::Path;

fn fixture() -> Vec<Json> {
    let envelope = |kind: &str, uuid: &str, parent: Option<&str>| {
        json!({
            "type": kind, "uuid": uuid, "parentUuid": parent,
            "sessionId": "11111111-2222-4333-8444-555555555555",
            "timestamp": "2026-01-02T03:04:05.000Z", "cwd": "/invented/project",
            "isSidechain": false, "version": "2.1.0", "futureEnvelope": {"nested": [1, false]}
        })
    };
    let mut user = envelope("user", "u1", None);
    user["message"] = json!({"role": "user", "content": [{"type":"text","text":"Investigate","citations":["invented"]}, {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AA=="}}]});
    let mut assistant = envelope("assistant", "a1", Some("u1"));
    assistant["message"] = json!({"id":"msg_one","role":"assistant","model":"claude-fiction","usage":{"input_tokens":8,"output_tokens":12,"cache_read_input_tokens":2,"unknown_usage":17},"content":[{"type":"thinking","thinking":"Consider options","signature":"invented-signature"},{"type":"text","text":"Checking file","citations":[{"start":0}]},{"type":"tool_use","id":"toolu_1","name":"Agent","input":{"description":"Check a file","subagent_type":"Explore"},"futureBlock":42}]});
    let mut result = envelope("user", "u2", Some("a1"));
    result["message"] = json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"first","annotations":[1]},{"type":"text","text":"second","annotations":[2]}],"is_error":false,"extra":"retained"}]});
    result["toolUseResult"] = json!({"agentId":"sub-7","agentType":"Explore","extra":true});
    let mut compact_boundary = envelope("system", "sys1", Some("u2"));
    compact_boundary["subtype"] = json!("compact_boundary");
    compact_boundary["compactMetadata"] = json!({"preTokens":321,"unknown":true});
    let mut summary = envelope("user", "u3", Some("sys1"));
    summary["isCompactSummary"] = json!(true);
    summary["message"] = json!({"role":"user","content":"Invented summary"});
    let mut next_user = envelope("user", "u4", Some("u3"));
    next_user["message"] = json!({"role":"user","content":"Continue"});
    vec![
        json!({"type":"queue-operation","operation":"enqueue","sessionId":"11111111-2222-4333-8444-555555555555","unknown":[3,2,1]}),
        user, assistant, result, compact_boundary, summary, next_user,
        json!({"type":"summary","summary":"Invented title","leafUuid":"u4","future":true}),
        json!({"type":"last-prompt","leafUuid":"u4","lastPrompt":"Continue"}),
    ]
}

fn write_lines(path: &Path, lines: &[Json]) {
    std::fs::write(path, lines.iter().map(Json::to_string).collect::<Vec<_>>().join("\n") + "\n").unwrap();
}

fn parsed(path: &Path) -> Vec<Json> {
    std::fs::read_to_string(path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

#[test]
fn imported_transcript_maps_blocks_compaction_and_sidechain_without_losing_native_fields() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("11111111-2222-4333-8444-555555555555.jsonl");
    let lines = fixture();
    write_lines(&source, &lines);
    let side = source.with_extension("");
    std::fs::create_dir_all(side.join("subagents")).unwrap();
    let child = json!({"type":"user","isSidechain":true,"uuid":"child1","parentUuid":null,"sessionId":"child-session","timestamp":"2026-01-02T03:04:05.000Z","message":{"role":"user","content":"Inspect the test"}});
    write_lines(&side.join("subagents/agent-sub-7.jsonl"), std::slice::from_ref(&child));
    std::fs::write(side.join("subagents/agent-sub-7.meta.json"), r#"{"toolUseId":"toolu_1","agentType":"Explore","description":"Check a file","extra":23}"#).unwrap();
    std::fs::create_dir_all(side.join("tool-results")).unwrap();
    std::fs::write(side.join("tool-results/blob.bin"), [0, 255, 42]).unwrap();

    let sink = MemorySink::new();
    ClaudeAdapter::new().import(&source, &sink).unwrap();
    let dom = sink.dom();
    let root = dom.root().unwrap();
    assert_eq!(root.str_attr(attr::NATIVE_ID), Some("11111111-2222-4333-8444-555555555555"));
    assert_eq!(root.str_attr(attr::CWD), Some("/invented/project"));
    assert_eq!(root.str_attr(attr::TITLE), Some("Invented title"));
    let find = |kind| dom.iter().filter(|n| n.kind == kind).collect::<Vec<_>>();
    assert_eq!(find(ElementKind::Turn).len(), 2);
    assert!(find(ElementKind::Message).iter().any(|n| n.text == "Consider options" && n.str_attr(attr::KIND) == Some("thinking")));
    let call = find(ElementKind::Call)[0];
    assert_eq!(call.str_attr(attr::CALL_ID), Some("toolu_1"));
    assert_eq!(call.str_attr(attr::STATUS), Some("done"));
    let job = find(ElementKind::Job)[0];
    assert_eq!(job.str_attr("x-claude-file"), Some("subagents/agent-sub-7.jsonl"));
    assert_eq!(find(ElementKind::Result)[0].text, "first\nsecond");
    let compact = find(ElementKind::Compaction)[0];
    assert_eq!(compact.text, "Invented summary");
    assert_eq!(compact.int_attr(attr::TOKENS_BEFORE), Some(321));
    assert!(find(ElementKind::Ext).iter().any(|n| n.str_attr(attr::TYPE) == Some("block") && n.text.contains("\"type\":\"image\"")));

    let out = dir.path().join("exported.jsonl");
    ClaudeAdapter::new().export(&dom, &out).unwrap();
    assert_eq!(parsed(&out), lines);
    let exported_side = out.with_extension("");
    assert_eq!(parsed(&exported_side.join("subagents/agent-sub-7.jsonl")), vec![child]);
    assert_eq!(std::fs::read(exported_side.join("tool-results/blob.bin")).unwrap(), [0, 255, 42]);
    assert_eq!(std::fs::read(exported_side.join("subagents/agent-sub-7.meta.json")).unwrap(), std::fs::read(side.join("subagents/agent-sub-7.meta.json")).unwrap());
}

#[test]
fn imports_sidechain_transcript_as_its_own_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("child.jsonl");
    let user = json!({
        "type":"user", "isSidechain":true, "uuid":"child-user", "parentUuid":null,
        "sessionId":"child-session", "timestamp":"2026-01-02T03:04:05.000Z",
        "message":{"role":"user","content":"Inspect the test"}, "unknown":{"scope":"child"}
    });
    let assistant = json!({
        "type":"assistant", "isSidechain":true, "uuid":"child-assistant",
        "parentUuid":"child-user", "sessionId":"child-session",
        "timestamp":"2026-01-02T03:04:06.000Z",
        "message":{"role":"assistant","id":"child-message","model":"claude-fiction",
            "content":[{"type":"text","text":"Looks good"}]}
    });
    write_lines(&path, &[user.clone(), assistant.clone()]);
    let sink = MemorySink::new();
    native::import(&path, &sink).unwrap();
    let dom = sink.dom();
    assert_eq!(dom.root().unwrap().str_attr(attr::NATIVE_ID), Some("child-session"));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Message && n.text == "Looks good"));
    let out = dir.path().join("child-export.jsonl");
    native::export(&dom, &out).unwrap();
    assert_eq!(parsed(&out), vec![user, assistant]);
}

#[test]
fn editing_imported_text_updates_native_content_without_erasing_unknown_fields() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("session.jsonl");
    write_lines(&source, &fixture());
    let sink = MemorySink::new();
    native::import(&source, &sink).unwrap();
    let message = sink.with_dom(|dom| dom.iter().find(|n| n.kind == ElementKind::Message && n.text == "Checking file").unwrap().id);
    sink.commit(Draft::append(message, " again")).unwrap();
    let out = dir.path().join("edited.jsonl");
    native::export(&sink.dom(), &out).unwrap();
    let edited = parsed(&out);
    assert_eq!(edited[2]["message"]["content"][1]["text"], "Checking file again");
    assert_eq!(edited[2]["message"]["content"][1]["citations"], json!([{"start":0}]));
    assert_eq!(edited[2]["futureEnvelope"], json!({"nested":[1,false]}));
    assert_eq!(edited[2]["message"]["usage"]["unknown_usage"], 17);
}

#[test]
fn exports_new_dom_messages_as_chainable_native_lines() {
    let dir = tempfile::tempdir().unwrap();
    let sink = MemorySink::new();
    let config = SessionConfig { session_id: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into(), cwd: dir.path().to_owned(), ..SessionConfig::default() };
    // A fresh app session has no x-claude-line provenance.
    let root = sink.commit(Draft::insert(loa_core::NodeId::DOCUMENT, ElementKind::Session, [
        (attr::ID, Value::from(config.session_id)), (attr::HARNESS, Value::from("claude-code")),
        (attr::NODE, Value::from("local")), (attr::CREATED, Value::Int(0)), (attr::SCHEMA, Value::Int(1)),
    ], "")).unwrap().node;
    let turn = sink.commit(Draft::insert(root, ElementKind::Turn, [(attr::STATUS, Value::from("done")), (attr::STARTED, Value::Int(1000))], "")).unwrap().node;
    sink.commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from(Role::User)), (attr::STATUS, Value::from("done"))], "Start here")).unwrap();
    sink.commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from(Role::Assistant)), (attr::STATUS, Value::from("done"))], "Here is the result")).unwrap();
    let out = dir.path().join("new.jsonl");
    native::export(&sink.dom(), &out).unwrap();
    let lines = parsed(&out);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["parentUuid"], Json::Null);
    assert_eq!(lines[1]["parentUuid"], lines[0]["uuid"]);
    assert_eq!(lines[0]["message"]["content"], "Start here");
    assert_eq!(lines[1]["message"]["content"][0]["text"], "Here is the result");
}
