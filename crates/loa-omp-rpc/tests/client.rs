use async_trait::async_trait;
use loa_omp_rpc::fake::{FakeOmp, READY_V2, handshake_v2};
use loa_omp_rpc::{
    AssistantEvent, Command, Event, HostHandler, HostToolCall, HostToolDefinition, HostToolOutput, Incoming, RpcClient,
    RpcError, SpawnOptions, StreamingBehavior, UiRequest, UiResponse,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn fake(script: &str) -> (tempfile::TempDir, FakeOmp) {
    let dir = tempfile::tempdir().unwrap();
    let omp = FakeOmp::install(dir.path(), script).unwrap();
    (dir, omp)
}

fn opts(omp: &FakeOmp) -> SpawnOptions {
    let mut opts = SpawnOptions::new(omp.executable());
    opts.ready_timeout = Duration::from_secs(20);
    opts
}

async fn next_event(events: &mut loa_omp_rpc::Events) -> Incoming {
    tokio::time::timeout(Duration::from_secs(20), events.recv()).await.unwrap().unwrap()
}

#[tokio::test]
async fn negotiates_v2_when_offered() {
    let (_dir, omp) = fake(&handshake_v2());
    let (client, mut events) = RpcClient::spawn(opts(&omp)).await.unwrap();
    assert_eq!(client.protocol_version(), 2);
    assert!(client.ready().supports_bash_exclude_from_context);
    client.shutdown(Duration::from_secs(10)).await.unwrap();
    assert!(matches!(next_event(&mut events).await, Incoming::Exited { .. }));
    let sent = omp.received();
    assert_eq!(sent[0]["type"], "negotiate_protocol");
    assert_eq!(sent[0]["protocolVersion"], 2);
    assert_eq!(omp.args()[..2], ["--mode".to_owned(), "rpc".to_owned()]);
}

#[tokio::test]
async fn stays_on_v1_for_an_omp_without_negotiation() {
    let (_dir, omp) = fake("> {\"type\":\"ready\"}\n");
    let (client, _events) = RpcClient::spawn(opts(&omp)).await.unwrap();
    assert_eq!(client.protocol_version(), 1);
    client.shutdown(Duration::from_secs(10)).await.unwrap();
    assert!(omp.received().is_empty(), "no negotiate_protocol sent to a v1 server");
}

#[tokio::test]
async fn refuses_an_omp_that_only_speaks_newer_protocols() {
    let (_dir, omp) = fake("> {\"type\":\"ready\",\"protocolVersion\":3,\"supportedProtocolVersions\":[3,4]}\n");
    let err = RpcClient::spawn(opts(&omp)).await.unwrap_err();
    match err {
        RpcError::Version { offered, min, max } => {
            assert_eq!(offered, vec![3, 4]);
            assert_eq!((min, max), (1, 2));
        }
        other => panic!("expected a version error, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_executable_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let err = RpcClient::spawn(SpawnOptions::new(dir.path().join("no-omp"))).await.unwrap_err();
    assert!(matches!(err, RpcError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn death_before_ready_reports_stderr() {
    let (_dir, omp) = fake("stderr config.yml is broken\nexit 1\n");
    let err = RpcClient::spawn(opts(&omp)).await.unwrap_err();
    match err {
        RpcError::Exited { status, stderr } => {
            assert_eq!(status.and_then(|s| s.code()), Some(1));
            assert!(stderr.contains("config.yml is broken"), "{stderr}");
        }
        other => panic!("expected exit, got {other:?}"),
    }
}

#[tokio::test]
async fn responses_are_matched_by_id_even_out_of_order() {
    let script = format!(
        "{}< get_state a\n< get_available_models b\n\
         > {{\"type\":\"response\",\"id\":\"{{{{b}}}}\",\"command\":\"get_available_models\",\"success\":true,\"data\":{{\"models\":[{{\"id\":\"m1\",\"provider\":\"p\"}}]}}}}\n\
         > {{\"type\":\"response\",\"id\":\"{{{{a}}}}\",\"command\":\"get_state\",\"success\":true,\"data\":{{\"sessionId\":\"s-1\",\"isStreaming\":false,\"thinkingLevel\":\"high\"}}}}\n",
        handshake_v2()
    );
    let (_dir, omp) = fake(&script);
    let (client, _events) = RpcClient::spawn(opts(&omp)).await.unwrap();
    let (state, models) = tokio::join!(client.get_state(), async {
        // Ordered after get_state on the wire.
        tokio::task::yield_now().await;
        client.get_available_models().await
    });
    let state = state.unwrap();
    assert_eq!(state.session_id, "s-1");
    assert_eq!(state.thinking_level.as_deref(), Some("high"));
    assert_eq!(models.unwrap()[0].selector(), "p/m1");
}

#[tokio::test]
async fn a_failed_command_carries_message_and_code() {
    let script = format!(
        "{}< fork f\n> {{\"type\":\"response\",\"id\":\"{{{{f}}}}\",\"command\":\"fork\",\"success\":false,\"error\":\"busy\",\"code\":\"session_busy\"}}\n",
        handshake_v2()
    );
    let (_dir, omp) = fake(&script);
    let (client, _events) = RpcClient::spawn(opts(&omp)).await.unwrap();
    let err = client.fork(None).await.unwrap_err();
    assert_eq!(err.code(), Some("session_busy"));
    assert!(err.to_string().contains("busy"));
}

#[tokio::test]
async fn a_prompt_accepted_without_data_still_expects_a_turn() {
    // omp 19 answers an accepted prompt with no `data` field at all.
    let script = format!(
        "{}< prompt p\n> {{\"type\":\"response\",\"id\":\"{{{{p}}}}\",\"command\":\"prompt\",\"success\":true}}\n",
        handshake_v2()
    );
    let (_dir, omp) = fake(&script);
    let (client, _events) = RpcClient::spawn(opts(&omp)).await.unwrap();
    let ack = client.prompt("hi", Vec::new(), None).await.unwrap();
    assert_eq!(ack.agent_invoked, None, "no data means omp did not finish the prompt locally");
}

#[tokio::test]
async fn a_prompt_goes_out_under_the_id_reserved_for_it() {
    let script = format!(
        "{}< prompt p\n> {{\"type\":\"response\",\"id\":\"{{{{p}}}}\",\"command\":\"prompt\",\"success\":true}}\n",
        handshake_v2()
    );
    let (_dir, omp) = fake(&script);
    let (client, _events) = RpcClient::spawn(opts(&omp)).await.unwrap();
    let id = client.reserve_id();
    assert_ne!(client.reserve_id(), id, "ids are never reused");
    client.prompt_as(id.clone(), "hi", Vec::new(), Some(StreamingBehavior::FollowUp)).await.unwrap();
    client.shutdown(Duration::from_secs(10)).await.unwrap();
    let sent = omp.received();
    let prompt = sent.iter().find(|frame| frame["type"] == "prompt").unwrap();
    assert_eq!((&prompt["id"], &prompt["streamingBehavior"]), (&json!(id), &json!("followUp")));
}

#[tokio::test]
async fn events_ui_requests_and_unknown_frames_arrive_in_order() {
    let script = format!(
        "{}> {{\"type\":\"agent_start\"}}\n\
         > {{\"type\":\"message_update\",\"messageId\":\"m1\",\"message\":{{\"role\":\"assistant\"}},\"assistantMessageEvent\":{{\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"Hi\"}}}}\n\
         > {{\"type\":\"future_event\",\"x\":1}}\n\
         > {{\"type\":\"extension_ui_request\",\"id\":\"ui1\",\"method\":\"confirm\",\"title\":\"Go?\",\"message\":\"Continue?\"}}\n\
         < extension_ui_response\n",
        handshake_v2()
    );
    let (_dir, omp) = fake(&script);
    let (client, mut events) = RpcClient::spawn(opts(&omp)).await.unwrap();
    assert_eq!(next_event(&mut events).await, Incoming::Event(Event::AgentStart));
    match next_event(&mut events).await {
        Incoming::Event(Event::MessageUpdate { role, event: AssistantEvent::TextDelta { delta, .. }, message_id }) => {
            assert_eq!((role.as_str(), delta.as_str(), message_id.as_deref()), ("assistant", "Hi", Some("m1")));
        }
        other => panic!("{other:?}"),
    }
    match next_event(&mut events).await {
        Incoming::Event(Event::Unknown { kind, raw }) => {
            assert_eq!(kind, "future_event");
            assert_eq!(raw["x"], 1);
        }
        other => panic!("{other:?}"),
    }
    let Incoming::Ui(UiRequest::Confirm { id, .. }) = next_event(&mut events).await else { panic!() };
    client.respond_ui(&UiResponse::Confirmed { id, confirmed: true }).await.unwrap();
    client.shutdown(Duration::from_secs(10)).await.unwrap();
    let sent = omp.received();
    assert_eq!(sent.last().unwrap(), &json!({"type":"extension_ui_response","id":"ui1","confirmed":true}));
}

struct Echo;

#[async_trait]
impl HostHandler for Echo {
    async fn tool_call(&self, call: HostToolCall) -> HostToolOutput {
        HostToolOutput::ok(format!("{}:{}", call.tool_name, call.arguments["q"].as_str().unwrap_or_default()))
    }
}

#[tokio::test]
async fn host_tools_are_registered_and_served() {
    let script = format!(
        "{}< set_host_tools t\n> {{\"type\":\"response\",\"id\":\"{{{{t}}}}\",\"command\":\"set_host_tools\",\"success\":true,\"data\":{{\"toolNames\":[\"project_search\"]}}}}\n\
         > {{\"type\":\"host_tool_call\",\"id\":\"h1\",\"toolCallId\":\"tc1\",\"toolName\":\"project_search\",\"arguments\":{{\"q\":\"iced\"}}}}\n\
         < host_tool_result\nexit 0\n",
        handshake_v2()
    );
    let (_dir, omp) = fake(&script);
    let mut o = opts(&omp);
    o.host = Some(Arc::new(Echo));
    let (client, mut events) = RpcClient::spawn(o).await.unwrap();
    let names = client
        .set_host_tools(vec![HostToolDefinition {
            name: "project_search".into(),
            label: None,
            description: "Search the project".into(),
            parameters: json!({"type":"object"}),
            hidden: None,
            load_mode: None,
        }])
        .await
        .unwrap();
    assert_eq!(names, ["project_search"]);
    // The fake exits after reading the result, which ends the event stream.
    while !matches!(next_event(&mut events).await, Incoming::Exited { .. }) {}
    let sent = omp.received();
    let result = sent.iter().find(|f| f["type"] == "host_tool_result").unwrap();
    assert_eq!(
        result,
        &json!({"type":"host_tool_result","id":"h1","result":{"content":[{"type":"text","text":"project_search:iced"}]}})
    );
    assert_eq!(sent[1]["tools"][0]["parameters"], json!({"type":"object"}));
}

#[tokio::test]
async fn child_death_fails_pending_requests_and_ends_events() {
    let script = format!("{}< get_state\nstderr out of memory\nexit 9\n", handshake_v2());
    let (_dir, omp) = fake(&script);
    let (client, mut events) = RpcClient::spawn(opts(&omp)).await.unwrap();
    let err = client.get_state().await.unwrap_err();
    assert!(matches!(&err, RpcError::Exited { status: Some(s), .. } if s.code() == Some(9)), "{err:?}");
    let Incoming::Exited { stderr, .. } = next_event(&mut events).await else { panic!() };
    assert!(stderr.contains("out of memory"));
    assert!(client.has_exited());
    assert!(matches!(client.request_unit(Command::Abort).await, Err(RpcError::Exited { .. })));
}

#[tokio::test]
async fn chunked_frames_are_reassembled() {
    use base64::Engine as _;
    let body = r#"{"type":"response","id":"{{s}}","command":"get_state","success":true,"data":{"sessionId":"big"}}"#;
    // Ids are substituted before the line is written, so the chunk carries a
    // fixed id the client generated: c1 is negotiate_protocol, c2 get_state.
    let body = body.replace("{{s}}", "c2");
    let (a, b) = body.as_bytes().split_at(30);
    let chunk = |i: u32, data: &[u8]| {
        json!({"type":"rpc_chunk","chunkId":"k1","index":i,"count":2,"byteLength":body.len(),
            "data": base64::engine::general_purpose::STANDARD.encode(data)})
        .to_string()
    };
    let script = format!("{}< get_state\n> {}\n> {}\n", handshake_v2(), chunk(0, a), chunk(1, b));
    let (_dir, omp) = fake(&script);
    let (client, _events) = RpcClient::spawn(opts(&omp)).await.unwrap();
    assert_eq!(client.get_state().await.unwrap().session_id, "big");
}

#[tokio::test]
async fn version_flag_reads_the_installed_version() {
    let (_dir, omp) = fake(&format!("> {READY_V2}\n"));
    omp.set_version("19.0.0").unwrap();
    assert_eq!(loa_omp_rpc::omp_version(omp.executable()).await.unwrap(), "19.0.0");
}

/// Talks to the real omp when one is installed: startup, negotiation and
/// calls that make no model call. Skipped when `omp` isn't on `PATH`. omp
/// refuses to start without a model, so it runs in its own agent directory
/// with one placeholder model that is never called; the user's omp sign-ins
/// and settings are never read.
#[tokio::test]
async fn live_omp_negotiates_and_reports_state() {
    let Some(path) = std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).map(|p| p.join("omp")).find(|p| p.is_file()))
    else {
        return;
    };
    let cwd = tempfile::tempdir().unwrap();
    let agent_dir = cwd.path().join("agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let models = json!({"providers": {"loa-test": {
        "baseUrl": "http://127.0.0.1:9/v1",
        "apiKey": "LOA_TEST_UNUSED_KEY",
        "api": "openai-completions",
        "models": [{"id": "never-called", "name": "never called", "reasoning": false, "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 8192, "maxTokens": 1024}],
    }}});
    std::fs::write(agent_dir.join("models.yml"), serde_json::to_vec_pretty(&models).unwrap()).unwrap();
    let mut o = SpawnOptions::new(path)
        .arg("--no-session")
        .arg("--no-extensions")
        .arg("--no-skills")
        .arg("--offline")
        .arg("--model")
        .arg("loa-test/never-called");
    o.cwd = Some(cwd.path().to_path_buf());
    o.no_ui = true;
    o.env.push(("PI_CODING_AGENT_DIR".into(), agent_dir.into_os_string()));
    o.env.push(("LOA_TEST_UNUSED_KEY".into(), "unused".into()));
    let (client, _events) = RpcClient::spawn(o).await.unwrap();
    assert_eq!(client.protocol_version(), 2);
    let state = client.get_state().await.unwrap();
    assert!(!state.session_id.is_empty());
    assert!(!state.is_streaming);
    client.get_available_models().await.unwrap();
    client.shutdown(Duration::from_secs(30)).await.unwrap();
}
