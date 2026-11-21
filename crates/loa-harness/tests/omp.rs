//! omp: transcript replays against goldens, native round trips, and one
//! session against a scripted fake omp.

use loa_contracts::harness::{Adapter, Answer, Prompt, SessionConfig, SessionState};
use loa_core::{Draft, ElementKind, MemorySink, NodeId, PatchSink, attr};
use loa_harness::omp::mapper::Mapper;
use loa_harness::omp::{OmpAdapter, OmpAdapterConfig, OmpNative};
use loa_harness::testing::{FakeProcess, WatchSink, golden, handshake_v2, replay, round_trip};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn fixtures(dir: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/omp").join(dir);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect();
    files.sort();
    files
}

#[test]
fn transcripts_replay_to_their_goldens() {
    for fixture in fixtures("") {
        let sink = MemorySink::new();
        let attrs = vec![(attr::ID, "s".into()), (attr::HARNESS, "omp".into()), (attr::NODE, "owner".into()), (attr::CREATED, 1_i64.into()), (attr::SCHEMA, 1_i64.into())];
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "")).unwrap().node;
        let replies = replay(Mapper::new(root), &sink, &fixture);
        // The seq comment is left out: goldens compare content only.
        let xml = loa_core::xml::to_string(&sink.dom());
        let mut out: String = xml.lines().filter(|l| !l.starts_with("<!--")).map(|l| format!("{l}\n")).collect();
        out.push_str("\n--- replies\n");
        for reply in replies {
            out.push_str(&format!("ui {:?}\n", reply.response));
            if let Some(steer) = reply.steer {
                out.push_str(&format!("steer {steer:?}\n"));
            }
        }
        golden(&fixture.with_extension("golden"), &out);
    }
}

#[test]
fn native_sessions_round_trip() {
    for fixture in fixtures("native") {
        round_trip::<OmpNative>(&fixture);
    }
}

#[test]
fn a_turn_records_when_it_started_and_when_it_ended() {
    use loa_harness::kit::EventMapper as _;
    use loa_harness::omp::mapper::Input;
    let sink = MemorySink::new();
    let attrs = vec![(attr::ID, "s".into()), (attr::HARNESS, "omp".into()), (attr::NODE, "owner".into()), (attr::CREATED, 1_i64.into()), (attr::SCHEMA, 1_i64.into())];
    let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "")).unwrap().node;
    let mut mapper = Mapper::new(root);
    let w = loa_core::DomWriter::new(&sink);
    let mut feed = |line: serde_json::Value| mapper.apply(serde_json::from_value::<Input>(line).unwrap(), &w).unwrap();
    feed(json!({"$": "send", "id": "p", "text": "read a.rs"}));
    feed(json!({"type": "agent_start"}));
    let times = |sink: &MemorySink| {
        let dom = sink.dom();
        let turn = dom.iter().find(|n| n.kind == ElementKind::Turn).expect("turn");
        (turn.int_attr(attr::STARTED), turn.int_attr(attr::ENDED))
    };
    let (started, ended) = times(&sink);
    assert!(started.is_some() && ended.is_none(), "a running turn has started but not ended");
    feed(json!({"type": "prompt_result", "id": "p", "agentInvoked": true, "status": "completed", "sessionSettled": true}));
    let (started, ended) = times(&sink);
    assert!(started.is_some() && ended.is_some(), "a finished turn records when it ended");
}

fn reply(command: &str, data: &str) -> String {
    format!("< {command} {command}\n> {{\"type\":\"response\",\"id\":\"{{{{{command}}}}}\",\"command\":\"{command}\",\"success\":true,\"data\":{data}}}\n")
}

#[tokio::test]
async fn a_session_streams_asks_and_settles_against_a_fake_omp() {
    let mut script = handshake_v2();
    for command in ["set_event_filter", "set_subagent_subscription", "set_ask_dialog"] {
        script.push_str(&reply(command, "{}"));
    }
    script.push_str(&reply("get_state", r#"{"sessionId":"native-1","sessionFile":"/tmp/native-1.jsonl","isStreaming":false}"#));
    script.push_str(&reply("prompt", r#"{"agentInvoked":true}"#));
    let update = |event: &str| format!("> {{\"type\":\"message_update\",\"messageId\":\"m1\",\"message\":{{\"role\":\"assistant\"}},\"assistantMessageEvent\":{event}}}\n");
    script.push_str("> {\"type\":\"agent_start\"}\n");
    script.push_str(&update(r#"{"type":"text_start","contentIndex":0}"#));
    script.push_str(&update(r#"{"type":"text_delta","contentIndex":0,"delta":"Working"}"#));
    script.push_str("> {\"type\":\"tool_execution_start\",\"toolCallId\":\"call-1\",\"toolName\":\"read\",\"args\":{\"path\":\"a.rs\"}}\n");
    script.push_str("> {\"type\":\"tool_execution_end\",\"toolCallId\":\"call-1\",\"toolName\":\"read\",\"result\":\"file text\"}\n");
    script.push_str("> {\"type\":\"extension_ui_request\",\"id\":\"approval\",\"method\":\"confirm\",\"title\":\"Run?\",\"message\":\"write files\"}\n< extension_ui_response\n");
    script.push_str(&update(r#"{"type":"text_end","contentIndex":0,"content":"Working"}"#));
    script.push_str("> {\"type\":\"prompt_result\",\"status\":\"completed\",\"agentInvoked\":true,\"sessionSettled\":true}\n");
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeProcess::install(dir.path(), &script).unwrap();
    let mut config = OmpAdapterConfig::new("owner", dir.path().join("state"));
    config.executable = fake.executable().to_owned();
    let sink = Arc::new(WatchSink::default());
    let start = SessionConfig { session_id: "app-1".into(), cwd: dir.path().to_owned(), ..Default::default() };
    let session = OmpAdapter::new(config).start(start, sink.clone()).await.unwrap();
    session.send(Prompt { text: "Please read".into(), by: "owner".into(), ..Default::default() }).await.unwrap();
    let waiting = |dom: &loa_core::Dom| dom.iter().find(|n| n.kind == ElementKind::Decision).map(|n| n.id);
    sink.until("the approval", |dom| waiting(dom).is_some()).await;
    assert_eq!(session.state(), SessionState::Waiting);
    let decision = sink.inner.with_dom(waiting).unwrap();
    session.answer(decision, Answer { values: vec!["allow_once".into()], by: "owner".into(), reason: None }).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), session.wait_idle()).await.unwrap().unwrap();
    let dom = sink.inner.dom();
    assert_eq!(dom.root().unwrap().str_attr(attr::NATIVE_ID), Some("native-1"));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Message && n.text == "Working"));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Result && n.text == "file text"));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Turn && n.str_attr(attr::STATUS) == Some("done")));
    session.shutdown().await.unwrap();
    assert_eq!(fake.received().iter().find(|frame| frame["id"] == "approval").unwrap()["confirmed"], json!(true));
    let args = fake.args();
    assert_eq!(&args[..2], ["--mode", "rpc-ui"]);
    let overlay = args.iter().position(|arg| arg == "--config").unwrap();
    assert_eq!(std::fs::read_to_string(&args[overlay + 1]).unwrap(), loa_harness::omp::MEMORY_OFF);
}
