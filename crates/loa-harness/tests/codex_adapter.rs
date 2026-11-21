use loa_contracts::harness::{Adapter, McpServerSpec, McpTransport, Prompt, SessionConfig};
use loa_core::{ElementKind, MemorySink, attr};
use loa_harness::{codex::CodexAdapter, testing::FakeProcess};
use std::sync::Arc;

#[cfg(unix)]
#[tokio::test]
async fn fake_codex_process_drives_turn_and_receives_session_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeProcess::install(dir.path(), include_str!("../fixtures/codex/driver.script")).unwrap();
    let cfg = SessionConfig {
        session_id: "session-invented".into(), node: "local-node".into(), cwd: dir.path().into(),
        model: Some("gpt-invented".into()),
        mcp_servers: vec![McpServerSpec { name: "inventory".into(), transport: McpTransport::Stdio {
            command: "inventory-mcp".into(), args: vec!["--stdio".into()], env: Default::default(),
        }}], ..SessionConfig::default()
    };
    let adapter = CodexAdapter::new("local-node").with_executable(fake.executable());
    let sink = Arc::new(MemorySink::new());
    let session = adapter.start(cfg, sink.clone()).await.unwrap();
    let models = session.models().await.unwrap();
    let listed: Vec<_> = models.iter().map(|m| (m.id.as_str(), m.name.as_str(), m.efforts.iter().map(String::as_str).collect::<Vec<_>>())).collect();
    assert_eq!(listed, [("gpt-invented", "GPT Invented", vec!["low", "high"]), ("gpt-mini", "GPT Mini", vec![])]);
    session.set_effort("high".into()).await.unwrap();
    assert_eq!(sink.dom().root().and_then(|root| root.str_attr(attr::EFFORT)), Some("high"));
    session.send(Prompt { text: "Ask inventory".into(), by: "person".into(), ..Prompt::default() }).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), session.wait_idle()).await.unwrap().unwrap();
    let dom = sink.dom();
    assert!(dom.iter().any(|n| n.kind == ElementKind::Message && n.text == "Invented answer"));
    assert_eq!(dom.iter().find(|n| n.kind == ElementKind::Turn).and_then(|n| n.str_attr(attr::STATUS)), Some("done"));
    let args = fake.args();
    assert!(args.iter().any(|a| a == "mcp_servers.inventory.command=\"inventory-mcp\""));
    assert!(args.windows(3).any(|w| w == ["app-server", "--listen", "stdio://"]));
    let sent = fake.received();
    assert_eq!(sent.iter().find(|line| line["method"] == "turn/start").unwrap()["params"]["effort"], "high");
    assert_eq!(sent.iter().filter(|line| line["method"] == "model/list").nth(1).unwrap()["params"]["cursor"], "page-2");
    session.shutdown().await.unwrap();
}
