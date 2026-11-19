use loa_adapter::{Adapter, Answer, Prompt, SessionConfig, SessionState};
use loa_adapter_omp::{OmpAdapter, OmpAdapterConfig};
use loa_core::{attr, ElementKind, MemorySink};
use loa_omp_rpc::fake::{FakeOmp, handshake_v2};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn reply(command: &str, id: &str, data: &str) -> String {
    format!("< {command} {id}\n> {{\"type\":\"response\",\"id\":\"{{{{{id}}}}}\",\"command\":\"{command}\",\"success\":true,\"data\":{data}}}\n")
}

#[tokio::test]
async fn scripted_stream_approval_and_ask_round_trip_through_dom() {
    let mut script = handshake_v2();
    for command in ["set_event_filter", "set_subagent_subscription", "set_ask_dialog"] {
        script.push_str(&reply(command, command, "{}"));
    }
    script.push_str(&reply("get_state", "state", r#"{"sessionId":"native-1","sessionFile":"/tmp/native-1.jsonl","isStreaming":false}"#));
    script.push_str(&reply("prompt", "prompt", r#"{"agentInvoked":true}"#));
    script.push_str("> {\"type\":\"agent_start\"}\n\
        > {\"type\":\"message_update\",\"messageId\":\"m1\",\"message\":{\"role\":\"assistant\"},\"assistantMessageEvent\":{\"type\":\"text_start\",\"contentIndex\":0}}\n\
        > {\"type\":\"message_update\",\"messageId\":\"m1\",\"message\":{\"role\":\"assistant\"},\"assistantMessageEvent\":{\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"Working\"}}\n\
        > {\"type\":\"tool_execution_start\",\"toolCallId\":\"call-1\",\"toolName\":\"read\",\"args\":{\"path\":\"a.rs\"}}\n\
        > {\"type\":\"tool_execution_end\",\"toolCallId\":\"call-1\",\"toolName\":\"read\",\"result\":\"file text\"}\n\
        > {\"type\":\"extension_ui_request\",\"id\":\"approval\",\"method\":\"confirm\",\"title\":\"Run?\",\"message\":\"write files\"}\n\
        < extension_ui_response\n\
        > {\"type\":\"extension_ui_request\",\"id\":\"ask-1\",\"method\":\"ask\",\"questions\":[{\"id\":\"q1\",\"question\":\"Which?\",\"options\":[{\"label\":\"one\"},{\"label\":\"two\"}]}]}\n\
        < extension_ui_response\n\
        > {\"type\":\"message_update\",\"messageId\":\"m1\",\"message\":{\"role\":\"assistant\"},\"assistantMessageEvent\":{\"type\":\"text_end\",\"contentIndex\":0,\"content\":\"Working\"}}\n\
        > {\"type\":\"prompt_result\",\"status\":\"completed\",\"agentInvoked\":true,\"sessionSettled\":true}\n");
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeOmp::install(dir.path(), &script).unwrap();
    let mut config = OmpAdapterConfig::new("owner", dir.path().join("state"));
    config.executable = fake.executable().to_owned();
    let adapter = OmpAdapter::new(config);
    let sink = Arc::new(MemorySink::new());
    let session = adapter.start(SessionConfig { session_id: "app-1".into(), cwd: dir.path().to_owned(), ..Default::default() }, sink.clone()).await.unwrap();
    session.send(Prompt { text: "Please read".into(), by: "owner".into(), ..Default::default() }).await.unwrap();
    for (request, response) in [("approval", "allow_once"), ("ask-1", "two")] {
        let decision = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(node) = sink.dom().iter().find(|node| node.kind == ElementKind::Decision && node.str_attr(attr::REQUEST_ID) == Some(request)) {
                    break node.id;
                }
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert_eq!(session.state(), SessionState::Waiting);
        session.answer(decision, Answer { values: vec![response.into()], by: "owner".into(), reason: None }).await.unwrap();
        assert_eq!(sink.dom().get(decision).unwrap().str_attr(attr::STATUS), Some("answered"));
    }
    tokio::time::timeout(Duration::from_secs(10), session.wait_idle()).await.unwrap().unwrap();
    let dom = sink.dom();
    assert_eq!(dom.root().unwrap().str_attr(attr::NATIVE_ID), Some("native-1"));
    assert!(dom.iter().any(|node| node.kind == ElementKind::Message && node.text == "Working"));
    assert!(dom.iter().any(|node| node.kind == ElementKind::Result && node.text == "file text"));
    assert!(dom.iter().any(|node| node.kind == ElementKind::Turn && node.str_attr(attr::STATUS) == Some("done")));
    session.shutdown().await.unwrap();
    let sent = fake.received();
    assert_eq!(sent.iter().find(|frame| frame["id"] == "approval").unwrap()["confirmed"], true);
    assert_eq!(sent.iter().find(|frame| frame["id"] == "ask-1").unwrap()["answers"], json!([{"id":"q1","selectedOptions":["two"]}]));
    assert_eq!(sent.iter().find(|frame| frame["type"] == "set_ask_dialog").unwrap()["enabled"], true);
    let args = fake.args();
    assert_eq!(&args[..2], ["--mode", "rpc-ui"]);
    let overlay = args.iter().position(|arg| arg == "--config").unwrap();
    assert_eq!(std::fs::read_to_string(&args[overlay + 1]).unwrap(), "memory:\n  backend: \"off\"\n");
}
