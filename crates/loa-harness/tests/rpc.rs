//! `RpcClient` on `StdioRpc<OmpFrames>` against a scripted fake omp.

use async_trait::async_trait;
use base64::Engine as _;
use loa_harness::omp::rpc::{
    AssistantEvent, Command, Event, Events, HostHandler, HostToolCall, HostToolDefinition, HostToolOutput, Incoming,
    RpcClient, RpcError, SpawnOptions, StreamingBehavior, UiRequest, UiResponse, omp_version,
};
use loa_harness::testing::{FakeProcess, READY_V2, handshake_v2};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn fake(script: &str) -> (tempfile::TempDir, FakeProcess, SpawnOptions) {
    let dir = tempfile::tempdir().unwrap();
    let omp = FakeProcess::install(dir.path(), script).unwrap();
    let mut opts = SpawnOptions::new(omp.executable());
    opts.ready_timeout = Duration::from_secs(20);
    (dir, omp, opts)
}

fn reply(name: &str, command: &str, data: &str) -> String {
    format!("> {{\"type\":\"response\",\"id\":\"{{{{{name}}}}}\",\"command\":\"{command}\",\"success\":true,\"data\":{data}}}\n")
}

async fn next(events: &mut Events) -> Incoming {
    tokio::time::timeout(Duration::from_secs(20), events.recv()).await.unwrap().unwrap()
}

#[tokio::test]
async fn startup_negotiates_the_protocol_or_reports_why_it_cannot() {
    let (_dir, omp, opts) = fake(&handshake_v2());
    let (client, mut events) = RpcClient::spawn(opts).await.unwrap();
    assert_eq!(client.protocol_version(), 2);
    assert!(client.ready().supports_bash_exclude_from_context);
    client.shutdown(Duration::from_secs(10)).await.unwrap();
    assert!(matches!(next(&mut events).await, Incoming::Exited { .. }));
    assert_eq!((omp.received()[0]["type"].as_str(), &omp.args()[..2]), (Some("negotiate_protocol"), &["--mode".to_owned(), "rpc".to_owned()][..]));

    let (_dir, omp, opts) = fake("> {\"type\":\"ready\"}\n");
    let (client, _events) = RpcClient::spawn(opts).await.unwrap();
    assert_eq!(client.protocol_version(), 1);
    client.shutdown(Duration::from_secs(10)).await.unwrap();
    assert!(omp.received().is_empty(), "no negotiate_protocol sent to a v1 server");

    let (_dir, _omp, opts) = fake("> {\"type\":\"ready\",\"protocolVersion\":3,\"supportedProtocolVersions\":[3,4]}\n");
    let err = RpcClient::spawn(opts).await.unwrap_err();
    assert!(matches!(&err, RpcError::Version { offered, min: 1, max: 2 } if *offered == [3, 4]), "{err:?}");

    let (dir, _omp, opts) = fake("stderr config.yml is broken\nexit 1\n");
    let err = RpcClient::spawn(opts).await.unwrap_err();
    assert!(matches!(&err, RpcError::Exited { status: Some(s), stderr } if s.code() == Some(1) && stderr.contains("config.yml is broken")), "{err:?}");

    let missing = RpcClient::spawn(SpawnOptions::new(dir.path().join("no-omp"))).await.unwrap_err();
    assert!(matches!(missing, RpcError::NotFound(_)), "{missing:?}");

    let (_dir, omp, _opts) = fake(&format!("> {READY_V2}\n"));
    omp.set_version("19.1.0").unwrap();
    assert_eq!(omp_version(omp.executable()).await.unwrap(), "19.1.0");
}

#[tokio::test]
async fn requests_match_by_id_fail_with_codes_and_reassemble_chunks() {
    // c1 negotiates, c2..c4 are get_state, get_available_models and fork, so the chunked body can carry a fixed id.
    let body = r#"{"type":"response","id":"c5","command":"get_state","success":true,"data":{"sessionId":"big"}}"#;
    let (a, b) = body.as_bytes().split_at(30);
    let chunk = |i: u32, data: &[u8]| {
        let data = base64::engine::general_purpose::STANDARD.encode(data);
        json!({"type":"rpc_chunk","chunkId":"k1","index":i,"count":2,"byteLength":body.len(),"data":data}).to_string()
    };
    let script = format!(
        "{}< get_state a\n< get_available_models b\n{}{}< fork f\n\
         > {{\"type\":\"response\",\"id\":\"{{{{f}}}}\",\"command\":\"fork\",\"success\":false,\"error\":\"busy\",\"code\":\"session_busy\"}}\n\
         < get_state\n> {}\n> {}\n< prompt p\n> {{\"type\":\"response\",\"id\":\"{{{{p}}}}\",\"command\":\"prompt\",\"success\":true}}\n",
        handshake_v2(),
        reply("b", "get_available_models", r#"{"models":[{"id":"m1","provider":"p"}]}"#),
        reply("a", "get_state", r#"{"sessionId":"s-1","isStreaming":false,"thinkingLevel":"high"}"#),
        chunk(0, a),
        chunk(1, b),
    );
    let (_dir, omp, opts) = fake(&script);
    let (client, _events) = RpcClient::spawn(opts).await.unwrap();
    let (state, models) = tokio::join!(client.get_state(), async {
        tokio::task::yield_now().await;
        client.get_available_models().await
    });
    assert_eq!((state.unwrap().thinking_level.as_deref(), models.unwrap()[0].selector()), (Some("high"), "p/m1".to_owned()));
    let err = client.fork(None).await.unwrap_err();
    assert_eq!((err.code(), err.to_string().contains("busy")), (Some("session_busy"), true));
    assert_eq!(client.get_state().await.unwrap().session_id, "big");
    let id = client.reserve_id();
    assert_ne!(client.reserve_id(), id, "ids are never reused");
    // omp 19 answers an accepted prompt with no `data` at all.
    let ack = client.prompt_as(id.clone(), "hi", Vec::new(), Some(StreamingBehavior::FollowUp)).await.unwrap();
    assert_eq!(ack.agent_invoked, None);
    client.shutdown(Duration::from_secs(10)).await.unwrap();
    let prompt = omp.received().into_iter().find(|frame| frame["type"] == "prompt").unwrap();
    assert_eq!((&prompt["id"], &prompt["streamingBehavior"]), (&json!(id), &json!("followUp")));
}

struct Echo;

#[async_trait]
impl HostHandler for Echo {
    async fn tool_call(&self, call: HostToolCall) -> HostToolOutput {
        HostToolOutput::ok(format!("{}:{}", call.tool_name, call.arguments["q"].as_str().unwrap_or_default()))
    }
}

#[tokio::test]
async fn events_ui_requests_and_host_tools_flow_in_order_until_exit() {
    let script = format!(
        "{}> {{\"type\":\"agent_start\"}}\n\
         > {{\"type\":\"message_update\",\"messageId\":\"m1\",\"message\":{{\"role\":\"assistant\"}},\"assistantMessageEvent\":{{\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"Hi\"}}}}\n\
         > {{\"type\":\"future_event\",\"x\":1}}\n\
         > {{\"type\":\"extension_ui_request\",\"id\":\"ui1\",\"method\":\"confirm\",\"title\":\"Go?\",\"message\":\"Continue?\"}}\n\
         < extension_ui_response\n< set_host_tools t\n{}\
         > {{\"type\":\"host_tool_call\",\"id\":\"h1\",\"toolCallId\":\"tc1\",\"toolName\":\"project_search\",\"arguments\":{{\"q\":\"iced\"}}}}\n\
         < host_tool_result\nstderr out of memory\nexit 9\n",
        handshake_v2(),
        reply("t", "set_host_tools", r#"{"toolNames":["project_search"]}"#),
    );
    let (_dir, omp, mut opts) = fake(&script);
    opts.host = Some(Arc::new(Echo));
    let (client, mut events) = RpcClient::spawn(opts).await.unwrap();
    assert_eq!(next(&mut events).await, Incoming::Event(Event::AgentStart));
    let Incoming::Event(Event::MessageUpdate { role, event: AssistantEvent::TextDelta { delta, .. }, message_id }) = next(&mut events).await else { panic!() };
    assert_eq!((role.as_str(), delta.as_str(), message_id.as_deref()), ("assistant", "Hi", Some("m1")));
    let Incoming::Event(Event::Unknown(raw)) = next(&mut events).await else { panic!() };
    assert_eq!((&raw["type"], &raw["x"]), (&json!("future_event"), &json!(1)));
    let Incoming::Ui(UiRequest::Confirm { id, .. }) = next(&mut events).await else { panic!() };
    client.respond_ui(&UiResponse::Confirmed { id, confirmed: true }).await.unwrap();
    let tool = HostToolDefinition {
        name: "project_search".into(),
        label: None,
        description: "Search the project".into(),
        parameters: json!({"type":"object"}),
        hidden: None,
        load_mode: None,
    };
    assert_eq!(client.set_host_tools(vec![tool]).await.unwrap(), ["project_search"]);
    let Incoming::Exited { status, stderr } = next(&mut events).await else { panic!() };
    assert_eq!((status.and_then(|s| s.code()), stderr.contains("out of memory")), (Some(9), true));
    assert!(client.has_exited() && matches!(client.request_unit(Command::Abort).await, Err(RpcError::Exited { .. })));
    let sent = omp.received();
    assert_eq!(sent[1], json!({"type":"extension_ui_response","id":"ui1","confirmed":true}));
    assert_eq!(sent[2]["tools"][0]["parameters"], json!({"type":"object"}));
    let result = json!({"type":"host_tool_result","id":"h1","result":{"content":[{"type":"text","text":"project_search:iced"}]}});
    assert_eq!(sent[3], result);
}
