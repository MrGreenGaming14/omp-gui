use loa_adapter::{Adapter, Answer, Prompt, SessionConfig, SessionState};
use loa_adapter_codex::{CodexAdapter, mapper::{Mapper, Reply}, rollout};
use loa_core::{ElementKind, MemorySink, PatchSink, attr, answer};
use serde_json::{Value as Json, json};
use std::{io::Cursor, sync::Arc};

fn config() -> SessionConfig {
    SessionConfig {
        session_id: "session-invented".into(),
        node: "local-node".into(),
        cwd: "/work/invented".into(),
        model: Some("gpt-invented".into()),
        ..SessionConfig::default()
    }
}

fn notify(mapper: &mut Mapper, method: &str, params: Json) {
    mapper.notification(method, &params).expect("scripted Codex notification");
}

#[test]
fn scripted_events_stream_tools_usage_and_approval_decisions() {
    let sink = Arc::new(MemorySink::new());
    let writer: Arc<dyn PatchSink> = sink.clone();
    let mut mapper = Mapper::start(&config(), "thread-one", Some("gpt-invented"), writer).expect("start DOM");
    mapper.begin_turn(&Prompt { text: "Inspect inventory".into(), by: "person".into(), ..Prompt::default() }).expect("prompt");
    notify(&mut mapper, "turn/started", json!({"turn": {"id": "turn-one"}}));
    notify(&mut mapper, "item/agentMessage/delta", json!({"turnId":"turn-one","itemId":"message-one","delta":"Inventory "}));
    notify(&mut mapper, "item/agentMessage/delta", json!({"turnId":"turn-one","itemId":"message-one","delta":"ready"}));
    notify(&mut mapper, "item/completed", json!({"turnId":"turn-one","item":{"id":"message-one","type":"agentMessage","text":"Inventory ready"}}));
    notify(&mut mapper, "item/reasoning/summaryTextDelta", json!({"turnId":"turn-one","itemId":"thought-one","delta":"Checking"}));
    notify(&mut mapper, "item/completed", json!({"turnId":"turn-one","item":{"id":"thought-one","type":"reasoning","summary":["Checking"]}}));
    notify(&mut mapper, "item/started", json!({"turnId":"turn-one","item":{"id":"command-one","type":"commandExecution","command":"printf inventory","cwd":"/work/invented"}}));
    let command_reply = mapper.request(json!(71), "item/commandExecution/requestApproval", &json!({"turnId":"turn-one","itemId":"command-one","command":"printf inventory"})).expect("approval request");
    assert!(command_reply.is_none());
    let dom = sink.dom();
    let approval = dom.iter().find(|node| node.kind == ElementKind::Decision).expect("command decision");
    assert!(approval.text.contains("printf inventory"));
    let reply = mapper.answer(approval.id, &Answer { values: vec![answer::ALLOW_SESSION.into()], by: "person".into(), ..Answer::default() }).expect("answer");
    assert!(matches!(reply, Some(Reply::Result { id, result }) if id == json!(71) && result == json!({"decision":"acceptForSession"})));
    notify(&mut mapper, "item/commandExecution/outputDelta", json!({"itemId":"command-one","delta":"inventory\n"}));
    notify(&mut mapper, "item/completed", json!({"turnId":"turn-one","item":{"id":"command-one","type":"commandExecution","command":"printf inventory","aggregatedOutput":"inventory\n","exitCode":0,"status":"completed"}}));
    notify(&mut mapper, "item/started", json!({"turnId":"turn-one","item":{"id":"patch-one","type":"fileChange","changes":[{"path":"inventory.txt","kind":{"type":"update"},"diff":"@@ -1 +1 @@\n-old\n+new"}]}}));
    mapper.request(json!(72), "item/fileChange/requestApproval", &json!({"turnId":"turn-one","itemId":"patch-one","reason":"update inventory"})).expect("patch approval request");
    let dom = sink.dom();
    let patch = dom.iter().find(|n| n.kind == ElementKind::Decision && n.str_attr(attr::REQUEST_ID) == Some("72")).expect("patch decision");
    assert!(patch.text.contains("+new"), "patch approval must display diff");
    let reply = mapper.answer(patch.id, &Answer { values: vec![answer::DENY.into()], by: "person".into(), ..Answer::default() }).expect("deny patch");
    assert!(matches!(reply, Some(Reply::Result { result, .. }) if result == json!({"decision":"decline"})));
    notify(&mut mapper, "item/completed", json!({"turnId":"turn-one","item":{"id":"patch-one","type":"fileChange","status":"declined","changes":[]}}));
    notify(&mut mapper, "item/completed", json!({"turnId":"turn-one","item":{"id":"mcp-one","type":"mcpToolCall","server":"catalog","tool":"lookup","arguments":{"item":"inventory"},"result":{"content":[{"type":"text","text":"found"}]}}}));
    notify(&mut mapper, "thread/tokenUsage/updated", json!({"tokenUsage":{"last":{"inputTokens":31,"cachedInputTokens":7,"outputTokens":11}}}));
    notify(&mut mapper, "turn/completed", json!({"turn":{"id":"turn-one","status":"completed"}}));
    assert_eq!(mapper.state(), SessionState::Idle);
    let dom = sink.dom();
    let turn = dom.iter().find(|n| n.kind == ElementKind::Turn).expect("turn");
    assert_eq!(turn.str_attr(attr::STATUS), Some("done"));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Message && n.text == "Inventory ready" && n.str_attr(attr::STATUS) == Some("done")));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Message && n.text == "Checking" && n.str_attr(attr::KIND) == Some("thinking")));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Call && n.str_attr(attr::TOOL) == Some("mcp__catalog__lookup")));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Call && n.str_attr(attr::STATUS) == Some("cancelled")));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Usage && n.int_attr(attr::INPUT_TOKENS) == Some(31) && n.int_attr(attr::CACHE_READ_TOKENS) == Some(7)));
}

#[test]
fn rollout_roundtrips_every_native_line_and_preserves_unknown_fields() {
    let fixture = include_str!("fixtures/rollout-basic.jsonl");
    let sink = MemorySink::new();
    rollout::import(Cursor::new(fixture), &sink, "local-node").expect("import rollout");
    let dom = sink.dom();
    let root = dom.root().expect("root");
    assert_eq!(root.str_attr(attr::NATIVE_ID), Some("0000aaaa-1111-7222-8333-444455556666"));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Compaction && n.text.contains("three")));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Call && n.str_attr(attr::TOOL) == Some("local_shell")));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Ext && n.text.contains("some_future_line")));
    let mut exported = Vec::new();
    rollout::export(&dom, &mut exported).expect("export rollout");
    let original: Vec<Json> = fixture.lines().map(|line| serde_json::from_str(line).expect("fixture JSON")).collect();
    let actual: Vec<Json> = String::from_utf8(exported).expect("UTF-8").lines().map(|line| serde_json::from_str(line).expect("export JSON")).collect();
    assert_eq!(actual, original, "native JSONL must survive import/export structurally");
}

#[cfg(unix)]
#[tokio::test]
async fn fake_codex_process_drives_turn_and_receives_session_overrides() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("temp dir");
    let script = dir.path().join("codex");
    let trace = dir.path().join("arguments");
    std::fs::write(&script, r#"#!/bin/sh
printf '%s\n' "$@" > "$TRACE_FILE"
rid() { printf '%s' "$1" | sed 's/^{"id":\([0-9]*\),.*/\1/'; }
while IFS= read -r line; do
    printf '%s\n' "$line" >> "$TRACE_FILE.stdin"
    case "$line" in
        *'"method":"initialize"'*) printf '%s\n' '{"id":0,"result":{"serverInfo":{"name":"fake","version":"1"}}}' ;;
        *'"method":"thread/start"'*) printf '%s\n' '{"id":1,"result":{"thread":{"id":"thread-fake"},"model":"gpt-invented"}}' ;;
        *'"method":"model/list"'*'"cursor":"page-2"'*)
            printf '{"id":%s,"result":{"data":[{"id":"mini","model":"gpt-mini","displayName":"GPT Mini","supportedReasoningEfforts":[]},{"id":"secret","model":"gpt-secret","hidden":true}],"nextCursor":null}}\n' "$(rid "$line")" ;;
        *'"method":"model/list"'*)
            printf '{"id":%s,"result":{"data":[{"id":"invented","model":"gpt-invented","displayName":"GPT Invented","supportedReasoningEfforts":[{"reasoningEffort":"low","description":"fast"},{"reasoningEffort":"high","description":"deep"}],"defaultReasoningEffort":"low","isDefault":true}],"nextCursor":"page-2"}}\n' "$(rid "$line")" ;;
        *'"method":"turn/start"'*)
            printf '{"id":%s,"result":{"turn":{"id":"turn-fake","status":"inProgress"}}}\n' "$(rid "$line")"
            printf '%s\n' '{"method":"item/agentMessage/delta","params":{"turnId":"turn-fake","itemId":"msg-fake","delta":"Invented answer"}}'
            printf '%s\n' '{"method":"item/completed","params":{"turnId":"turn-fake","item":{"id":"msg-fake","type":"agentMessage","text":"Invented answer"}}}'
            printf '%s\n' '{"method":"turn/completed","params":{"turn":{"id":"turn-fake","status":"completed"}}}' ;;
    esac
done
"#).expect("fake script");
    let mut permissions = std::fs::metadata(&script).expect("script metadata").permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&script, permissions).expect("executable script");
    let mut cfg = config();
    cfg.cwd = dir.path().to_path_buf();
    cfg.env.insert("TRACE_FILE".into(), trace.to_string_lossy().into_owned());
    cfg.mcp_servers.push(loa_adapter::McpServerSpec { name: "inventory".into(), transport: loa_adapter::McpTransport::Stdio { command: "inventory-mcp".into(), args: vec!["--stdio".into()], env: Default::default() } });
    let adapter = CodexAdapter::new("local-node").with_executable(script);
    let sink = Arc::new(MemorySink::new());
    let writer: Arc<dyn PatchSink> = sink.clone();
    let session = adapter.start(cfg, writer).await.expect("start fake Codex");
    let models = session.models().await.expect("model/list");
    let listed: Vec<(&str, &str, Vec<&str>)> =
        models.iter().map(|m| (m.id.as_str(), m.name.as_str(), m.efforts.iter().map(String::as_str).collect())).collect();
    assert_eq!(listed, [("gpt-invented", "GPT Invented", vec!["low", "high"]), ("gpt-mini", "GPT Mini", vec![])]);
    session.set_effort("high".into()).await.expect("set effort");
    assert_eq!(sink.dom().root().and_then(|root| root.str_attr(attr::EFFORT)), Some("high"));
    session.send(Prompt { text: "Ask inventory".into(), by: "person".into(), ..Prompt::default() }).await.expect("turn/start");
    tokio::time::timeout(std::time::Duration::from_secs(5), session.wait_idle()).await.expect("fake must complete").expect("idle");
    let dom = sink.dom();
    assert!(dom.iter().any(|n| n.kind == ElementKind::Message && n.text == "Invented answer"));
    assert_eq!(dom.iter().find(|n| n.kind == ElementKind::Turn).and_then(|n| n.str_attr(attr::STATUS)), Some("done"));
    let arguments = std::fs::read_to_string(trace).expect("CLI trace");
    assert!(arguments.contains("mcp_servers.inventory.command=\"inventory-mcp\""));
    assert!(arguments.contains("app-server\n--listen\nstdio://"));
    let sent = std::fs::read_to_string(dir.path().join("arguments.stdin")).expect("stdin trace");
    let turn: Json = sent.lines().map(|line| serde_json::from_str::<Json>(line).expect("JSON line")).find(|line| line["method"] == "turn/start").expect("turn/start sent");
    assert_eq!(turn["params"]["effort"], "high");
    session.shutdown().await.expect("shutdown fake Codex");
}
