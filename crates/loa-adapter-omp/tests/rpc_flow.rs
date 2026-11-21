use loa_contracts::harness::{Adapter, Answer, HarnessSession, Prompt, SessionConfig, SessionState};
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

const APPROVAL: &str = include_str!("fixtures/omp19/approval.script");
const APPROVED: &str = include_str!("fixtures/omp19/approved.script");
const DENIED: &str = include_str!("fixtures/omp19/denied.script");
const ABORT: &str = include_str!("fixtures/omp19/abort.script");
const ABORT_QUEUED: &str = include_str!("fixtures/omp19/abort-queued.script");
const FOLLOW_UP: &str = include_str!("fixtures/omp19/follow-up.script");
const WAIT: Duration = Duration::from_secs(10);

/// A fake omp that answers the adapter's startup, then plays `script`.
async fn session_with(dir: &std::path::Path, script: &str) -> (FakeOmp, Arc<MemorySink>, Box<dyn HarnessSession>) {
    let mut full = handshake_v2();
    for command in ["set_event_filter", "set_subagent_subscription", "set_ask_dialog"] {
        full.push_str(&reply(command, command, "{}"));
    }
    full.push_str(&reply("get_state", "state", r#"{"sessionId":"native-1","sessionFile":"/tmp/native-1.jsonl","isStreaming":false,"model":{"id":"claude-haiku-5-5","provider":"anthropic"}}"#));
    full.push_str(script);
    let fake = FakeOmp::install(dir, &full).unwrap();
    let mut config = OmpAdapterConfig::new("owner", dir.join("state"));
    config.executable = fake.executable().to_owned();
    let sink = Arc::new(MemorySink::new());
    let session = OmpAdapter::new(config).start(SessionConfig { session_id: "app-1".into(), cwd: dir.to_owned(), ..Default::default() }, sink.clone()).await.unwrap();
    (fake, sink, session)
}

async fn prompt(session: &dyn HarnessSession, text: &str) {
    session.send(Prompt { text: text.into(), by: "owner".into(), ..Default::default() }).await.unwrap();
}

async fn until<T>(sink: &MemorySink, mut find: impl FnMut(&loa_core::Dom) -> Option<T>) -> T {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(found) = find(&sink.dom()) { break found; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("the DOM got there in time")
}

fn waiting_decision(dom: &loa_core::Dom) -> Option<loa_core::NodeId> {
    dom.iter().find(|n| n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some("waiting")).map(|n| n.id)
}

fn turn_statuses(dom: &loa_core::Dom) -> Vec<String> {
    dom.iter().filter(|n| n.kind == ElementKind::Turn).map(|n| n.str_attr(attr::STATUS).unwrap_or_default().to_owned()).collect()
}

/// Answers omp's bash approval prompt with `answer` and returns what the
/// adapter wrote back, after the turn ends.
async fn answer_approval(answer: &str, reason: Option<&str>, tail: &str) -> (Vec<serde_json::Value>, loa_core::Dom) {
    let dir = tempfile::tempdir().unwrap();
    let (fake, sink, session) = session_with(dir.path(), &format!("{APPROVAL}{tail}")).await;
    prompt(session.as_ref(), "Run `echo loa-approved-7731` with your bash tool.").await;
    let decision = until(&sink, waiting_decision).await;
    assert_eq!(session.state(), SessionState::Waiting);
    session.answer(decision, Answer { values: vec![answer.into()], by: "owner".into(), reason: reason.map(str::to_owned) }).await.unwrap();
    tokio::time::timeout(WAIT, session.wait_idle()).await.unwrap().unwrap();
    session.shutdown().await.unwrap();
    (fake.received(), sink.dom())
}

fn ui_response(sent: &[serde_json::Value]) -> &serde_json::Value {
    sent.iter().find(|frame| frame["type"] == "extension_ui_response").expect("an answer to the select")
}

#[tokio::test]
async fn allow_once_answers_omps_approval_select_with_approve() {
    let (sent, dom) = answer_approval(loa_core::answer::ALLOW_ONCE, None, APPROVED).await;
    assert_eq!(ui_response(&sent), &json!({"type":"extension_ui_response","id":"15d4058f5bbc68c9","value":"Approve"}));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Result && n.text == "loa-approved-7731\n"), "the approved command's output");
    assert_eq!(turn_statuses(&dom), ["done"]);
    assert!(!sent.iter().any(|frame| frame["type"] == "steer"));
}

#[tokio::test]
async fn allow_session_answers_with_omps_session_choice() {
    let (sent, _) = answer_approval(loa_core::answer::ALLOW_SESSION, None, APPROVED).await;
    assert_eq!(ui_response(&sent)["value"], "Approve bash for this session");
}

#[tokio::test]
async fn deny_with_a_reason_denies_and_steers_the_reason() {
    let (sent, dom) = answer_approval(loa_core::answer::DENY, Some("not on this machine"), DENIED).await;
    assert_eq!(ui_response(&sent)["value"], "Deny");
    let steer = sent.iter().find(|frame| frame["type"] == "steer").expect("the reason as steering");
    assert_eq!(steer["message"], "I denied that tool call: not on this machine");
    let decision = dom.iter().find(|n| n.kind == ElementKind::Decision).unwrap();
    assert_eq!(decision.str_attr(attr::REASON), Some("not on this machine"));
    let call = dom.iter().find(|n| n.kind == ElementKind::Call).unwrap();
    assert_eq!(call.str_attr(attr::STATUS), Some("error"));
}

#[tokio::test]
async fn interrupt_aborts_omp_and_cancels_the_streaming_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, sink, session) = session_with(dir.path(), ABORT).await;
    prompt(session.as_ref(), "Write the numbers from 1 to 400.").await;
    until(&sink, |dom| dom.iter().any(|n| n.kind == ElementKind::Message && n.text.contains("\n7")).then_some(())).await;
    session.interrupt().await.unwrap();
    tokio::time::timeout(WAIT, session.wait_idle()).await.unwrap().unwrap();
    let dom = sink.dom();
    assert_eq!(turn_statuses(&dom), ["cancelled"]);
    assert!(dom.iter().all(|n| !matches!(n.str_attr(attr::STATUS), Some("streaming" | "running"))), "nothing left streaming or running");
    session.shutdown().await.unwrap();
    assert!(fake.received().iter().any(|frame| frame["type"] == "abort_and_restore_queue"));
}

#[tokio::test]
async fn a_prompt_sent_mid_turn_is_queued_as_a_follow_up_and_runs_next() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, sink, session) = session_with(dir.path(), FOLLOW_UP).await;
    prompt(session.as_ref(), "Write the numbers from 1 to 60.").await;
    until(&sink, |dom| dom.iter().any(|n| n.kind == ElementKind::Message && n.text.starts_with("1\n2")).then_some(())).await;
    prompt(session.as_ref(), "Now reply with exactly the word: second").await;
    tokio::time::timeout(WAIT, session.wait_idle()).await.unwrap().unwrap();
    let dom = sink.dom();
    assert_eq!(turn_statuses(&dom), ["done", "done"]);
    let users: Vec<&str> = dom.iter().filter(|n| n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == Some("user")).map(|n| n.text.as_str()).collect();
    assert_eq!(users, ["Write the numbers from 1 to 60.", "Now reply with exactly the word: second"]);
    let turns: Vec<_> = dom.iter().filter(|n| n.kind == ElementKind::Turn).map(|n| n.id).collect();
    assert!(dom.children_of_kind(turns[1], ElementKind::Message).any(|m| m.text == "second"), "the follow-up's reply is in its own turn");
    session.shutdown().await.unwrap();
    let prompts: Vec<_> = fake.received().into_iter().filter(|frame| frame["type"] == "prompt").collect();
    assert!(prompts.iter().all(|frame| frame["streamingBehavior"] == "followUp"));
}

#[tokio::test]
async fn interrupt_withdraws_a_queued_follow_up_into_a_cancelled_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (_fake, sink, session) = session_with(dir.path(), ABORT_QUEUED).await;
    prompt(session.as_ref(), "Write the numbers from 1 to 600.").await;
    until(&sink, |dom| dom.iter().any(|n| n.kind == ElementKind::Message && n.text.starts_with("1\n2")).then_some(())).await;
    prompt(session.as_ref(), "Now reply with exactly the word: second").await;
    session.interrupt().await.unwrap();
    tokio::time::timeout(WAIT, session.wait_idle()).await.unwrap().unwrap();
    let dom = sink.dom();
    assert_eq!(turn_statuses(&dom), ["cancelled", "cancelled"]);
    assert!(dom.iter().any(|n| n.kind == ElementKind::Message && n.text == "Now reply with exactly the word: second"));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn set_model_and_set_effort_update_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let script = format!(
        "{}{}{}",
        reply("get_available_models", "models", r#"{"models":[{"id":"claude-sonnet-4-5","name":"Claude Sonnet 4.5","provider":"anthropic","reasoning":true,"thinking":{"efforts":["low","high"]}},{"id":"gpt-x","provider":"openai","reasoning":false}]}"#),
        "< set_model sm\n> {\"type\":\"model_changed\"}\n> {\"type\":\"response\",\"id\":\"{{sm}}\",\"command\":\"set_model\",\"success\":true,\"data\":{\"id\":\"claude-sonnet-4-5\",\"provider\":\"anthropic\"}}\n",
        reply("set_thinking_level", "effort", "{}"),
    );
    let (fake, sink, session) = session_with(dir.path(), &script).await;
    assert_eq!(sink.dom().root().unwrap().str_attr(attr::MODEL), Some("anthropic/claude-haiku-5-5"), "the model omp started with");
    let models = session.models().await.unwrap();
    assert_eq!(models.iter().map(|m| (m.id.as_str(), m.name.as_str(), m.efforts.len())).collect::<Vec<_>>(), [("anthropic/claude-sonnet-4-5", "Claude Sonnet 4.5", 2), ("openai/gpt-x", "gpt-x", 0)]);
    session.set_model(loa_contracts::harness::ModelSelection { model: "anthropic/claude-sonnet-4-5".into(), role: None }).await.unwrap();
    assert_eq!(sink.dom().root().unwrap().str_attr(attr::MODEL), Some("anthropic/claude-sonnet-4-5"));
    session.set_effort("low".into()).await.unwrap();
    assert_eq!(sink.dom().root().unwrap().str_attr(attr::EFFORT), Some("low"));
    session.shutdown().await.unwrap();
    let sent = fake.received();
    let set_model = sent.iter().find(|frame| frame["type"] == "set_model").unwrap();
    assert_eq!((&set_model["provider"], &set_model["modelId"]), (&json!("anthropic"), &json!("claude-sonnet-4-5")));
}

struct Screenshot;

#[async_trait::async_trait]
impl loa_contracts::harness::HostTools for Screenshot {
    async fn call(&self, _: &str, _: &str, _: &str) -> Result<String, String> { Err("images only".into()) }

    async fn call_content(&self, session: &str, tool: &str, _: &str) -> Result<Vec<loa_contracts::harness::OutputBlock>, String> {
        Ok(vec![
            loa_contracts::harness::OutputBlock::Text(format!("{tool} for {session}")),
            loa_contracts::harness::OutputBlock::Image { mime: "image/png".into(), data: b"\x89PNG".to_vec().into() },
        ])
    }
}

#[tokio::test]
async fn host_tool_images_reach_omp_as_image_blocks() {
    let mut script = handshake_v2();
    for command in ["set_event_filter", "set_subagent_subscription", "set_ask_dialog"] {
        script.push_str(&reply(command, command, "{}"));
    }
    script.push_str(&reply("set_host_tools", "tools", r#"{"toolNames":["screenshot"]}"#));
    script.push_str(&reply("get_state", "state", r#"{"sessionId":"native-1","isStreaming":false}"#));
    script.push_str("> {\"type\":\"host_tool_call\",\"id\":\"h1\",\"toolCallId\":\"tc1\",\"toolName\":\"screenshot\",\"arguments\":{}}\n< host_tool_result\n");
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeOmp::install(dir.path(), &script).unwrap();
    let mut config = OmpAdapterConfig::new("owner", dir.path().join("state"));
    config.executable = fake.executable().to_owned();
    let session = OmpAdapter::new(config).start(SessionConfig {
        session_id: "app-1".into(),
        cwd: dir.path().to_owned(),
        host_tools: vec![loa_contracts::harness::HostToolSpec { name: "screenshot".into(), description: "Capture the screen".into(), input_schema: r#"{"type":"object"}"#.into() }],
        host_tool_handler: Some(Arc::new(Screenshot)),
        ..Default::default()
    }, Arc::new(MemorySink::new())).await.unwrap();
    let result = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(frame) = fake.received().into_iter().find(|frame| frame["type"] == "host_tool_result") { break frame; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    assert_eq!(result, json!({"type":"host_tool_result","id":"h1","result":{"content":[
        {"type":"text","text":"screenshot for app-1"},
        {"type":"image","data":"iVBORw==","mimeType":"image/png"}
    ]}}));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn text_typed_on_an_ask_question_card_goes_through_omps_other_row() {
    // omp 19's `ask` tool in RPC: one `select` per question with an "Other"
    // row; picking it opens an `input` for the text.
    let dir = tempfile::tempdir().unwrap();
    let script = "< prompt p\n\
        > {\"id\":\"{{p}}\",\"type\":\"response\",\"command\":\"prompt\",\"success\":true}\n\
        > {\"type\":\"agent_start\"}\n\
        > {\"type\":\"tool_execution_start\",\"toolCallId\":\"toolu_ask\",\"toolName\":\"ask\",\"args\":{}}\n\
        > {\"type\":\"extension_ui_request\",\"id\":\"sel-1\",\"method\":\"select\",\"title\":\"Which database?\",\"options\":[\"Postgres (Recommended)\",\"SQLite\",\"Other (type your own)\"]}\n\
        < extension_ui_response\n\
        > {\"type\":\"extension_ui_request\",\"id\":\"in-1\",\"method\":\"input\",\"title\":\"Which database?\"}\n\
        < extension_ui_response\n\
        > {\"type\":\"tool_execution_end\",\"toolCallId\":\"toolu_ask\",\"toolName\":\"ask\",\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"User answered: DuckDB\"}]}}\n\
        > {\"type\":\"agent_end\",\"messages\":[],\"isTerminal\":true,\"yielded\":true}\n\
        > {\"type\":\"prompt_result\",\"id\":\"{{p}}\",\"agentInvoked\":true,\"status\":\"completed\",\"sessionSettled\":true}\n\
        > {\"type\":\"session_settled\"}\n";
    let (fake, sink, session) = session_with(dir.path(), script).await;
    prompt(session.as_ref(), "Ask me which database.").await;
    let decision = until(&sink, waiting_decision).await;
    session.answer(decision, Answer { values: vec!["DuckDB".into()], by: "owner".into(), reason: None }).await.unwrap();
    tokio::time::timeout(WAIT, session.wait_idle()).await.unwrap().unwrap();
    let dom = sink.dom();
    assert_eq!(dom.iter().filter(|n| n.kind == ElementKind::Decision).count(), 1, "the input gets no card of its own");
    assert_eq!(dom.get(decision).unwrap().str_attr(attr::STATUS), Some("answered"));
    session.shutdown().await.unwrap();
    let answers: Vec<_> = fake.received().into_iter().filter(|frame| frame["type"] == "extension_ui_response").collect();
    assert_eq!(answers, [
        json!({"type":"extension_ui_response","id":"sel-1","value":"Other (type your own)"}),
        json!({"type":"extension_ui_response","id":"in-1","value":"DuckDB"}),
    ]);
}

#[tokio::test]
async fn an_at_mention_of_a_text_file_stays_a_path_and_an_image_goes_as_an_image() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.md"), "# notes").unwrap();
    let image = dir.path().join("shot.png");
    std::fs::write(&image, [137, 80, 78, 71]).unwrap();
    let script = "< prompt p\n\
        > {\"id\":\"{{p}}\",\"type\":\"response\",\"command\":\"prompt\",\"success\":true}\n\
        > {\"type\":\"agent_start\"}\n\
        > {\"type\":\"agent_end\",\"messages\":[],\"isTerminal\":true}\n\
        > {\"type\":\"prompt_result\",\"id\":\"{{p}}\",\"agentInvoked\":true,\"status\":\"completed\",\"sessionSettled\":true}\n\
        > {\"type\":\"session_settled\"}\n";
    let (fake, _sink, session) = session_with(dir.path(), script).await;
    let notes = dir.path().join("notes.md");
    let text = format!("Summarize @{} and @{}", notes.display(), image.display());
    let attachments = vec![
        loa_contracts::harness::Attachment { path: notes, mime: None },
        loa_contracts::harness::Attachment { path: image, mime: Some("image/png".into()) },
    ];
    session.send(Prompt { text: text.clone(), attachments, by: "owner".into() }).await.expect("a text file mention doesn't fail the send");
    tokio::time::timeout(WAIT, session.wait_idle()).await.unwrap().unwrap();
    session.shutdown().await.unwrap();
    let sent = fake.received().into_iter().find(|frame| frame["type"] == "prompt").unwrap();
    assert_eq!(sent["message"], text.as_str(), "the text keeps both @paths");
    let images = sent["images"].as_array().unwrap();
    assert_eq!(images.len(), 1, "only the image is sent as image content");
    assert_eq!(images[0]["mimeType"], "image/png");
}
