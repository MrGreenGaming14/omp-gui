//! Opt-in tests against the installed `opencode`, with real model calls.
//! Run with `cargo test -p loa-adapter-opencode --test live -- --ignored`.
//! Each test spawns its own `opencode serve` on a free port, unless
//! `LOA_LIVE_OPENCODE_URL` points at a running server.
//! `LOA_LIVE_MODEL` picks the model as `provider/model` (default: the free
//! `opencode/big-pickle`).

use loa_adapter::{Adapter, AvailabilityStatus, HarnessSession, Prompt, SessionConfig, SessionState};
use loa_adapter_opencode::OpenCodeAdapter;
use loa_core::{Dom, ElementKind, MemorySink, attr};
use std::sync::Arc;
use std::time::Duration;

const TURN_TIMEOUT: Duration = Duration::from_secs(240);
const SERVER_TIMEOUT: Duration = Duration::from_secs(60);

fn model() -> String {
    std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "opencode/big-pickle".into())
}

/// A spawned `opencode serve`; killed when dropped.
struct Server {
    _child: Option<tokio::process::Child>,
    url: String,
}

async fn spawn_server(dir: &std::path::Path) -> Server {
    if let Ok(url) = std::env::var("LOA_LIVE_OPENCODE_URL") {
        return Server { _child: None, url };
    }
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let child = tokio::process::Command::new("opencode")
        .args(["serve", "--hostname", "127.0.0.1", "--port", &port.to_string()])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("opencode is installed and starts");
    let url = format!("http://127.0.0.1:{port}");
    let adapter = OpenCodeAdapter::new(url.clone(), None);
    tokio::time::timeout(SERVER_TIMEOUT, async {
        loop {
            if adapter.availability().await.status == AvailabilityStatus::Ready {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("opencode serve became ready");
    Server { _child: Some(child), url }
}

async fn start(dir: &std::path::Path) -> (Server, Arc<MemorySink>, Box<dyn HarnessSession>) {
    let server = spawn_server(dir).await;
    let adapter = OpenCodeAdapter::new(server.url.clone(), None);
    let sink = Arc::new(MemorySink::new());
    let config = SessionConfig {
        session_id: "live-1".into(),
        node: "live-test".into(),
        cwd: dir.to_owned(),
        model: Some(model()),
        ..Default::default()
    };
    let session = adapter.start(config, sink.clone()).await.expect("opencode session starts");
    (server, sink, session)
}

async fn send(session: &dyn HarnessSession, text: &str) {
    session.send(Prompt { text: text.into(), by: "live-test".into(), ..Default::default() }).await.expect("prompt accepted");
}

async fn turn(session: &dyn HarnessSession, text: &str) {
    send(session, text).await;
    tokio::time::timeout(TURN_TIMEOUT, session.wait_idle()).await.expect("turn finished in time").expect("turn ended");
}

fn assistant_text(dom: &Dom) -> String {
    dom.iter()
        .filter(|n| n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == Some("assistant") && n.str_attr(attr::KIND) != Some("thinking"))
        .map(|n| n.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn until<T>(sink: &MemorySink, what: &str, mut find: impl FnMut(&Dom) -> Option<T>) -> T {
    tokio::time::timeout(TURN_TIMEOUT, async {
        loop {
            if let Some(found) = find(&sink.dom()) {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs an opencode server with real model calls"]
async fn a_real_turn_streams_an_assistant_reply_into_the_dom() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, sink, session) = start(dir.path()).await;
    turn(session.as_ref(), "Reply with exactly the single word: pong").await;
    let dom = sink.dom();
    let reply = assistant_text(&dom);
    assert!(reply.to_lowercase().contains("pong"), "assistant said {reply:?}");
    let turn = dom.iter().find(|n| n.kind == ElementKind::Turn).expect("a turn");
    assert_eq!(turn.str_attr(attr::STATUS), Some("done"));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == Some("user") && n.text.contains("pong")));
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs an opencode server with real model calls"]
async fn a_real_tool_call_becomes_a_call_with_its_result() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secret.txt"), "the answer is 4217\n").unwrap();
    let (_server, sink, session) = start(dir.path()).await;
    send(session.as_ref(), "Use your read tool to read secret.txt in the current directory, then tell me the number in it.").await;
    // Allow any approval the server asks for while the turn runs.
    let mut answered = std::collections::HashSet::new();
    tokio::time::timeout(TURN_TIMEOUT, async {
        loop {
            let waiting: Vec<_> = sink
                .dom()
                .iter()
                .filter(|n| n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some("waiting"))
                .map(|n| n.id)
                .filter(|id| !answered.contains(id))
                .collect();
            for id in waiting {
                answered.insert(id);
                session
                    .answer(id, loa_adapter::Answer { values: vec![loa_core::answer::ALLOW_ONCE.into()], reason: None, by: "live-test".into() })
                    .await
                    .unwrap();
            }
            if session.state() == SessionState::Idle {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("turn finished in time");
    let dom = sink.dom();
    let call = dom.iter().find(|n| n.kind == ElementKind::Call).expect("opencode called a tool");
    assert!(dom.children_of_kind(call.id, ElementKind::Result).next().is_some(), "the call has a result");
    assert!(assistant_text(&dom).contains("4217"), "assistant said {:?}", assistant_text(&dom));
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs an opencode server with real model calls"]
async fn interrupt_cancels_a_streaming_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, sink, session) = start(dir.path()).await;
    // Long enough that the abort always lands while the reply streams.
    send(session.as_ref(), "Write the numbers from 1 to 3000, one per line, with no other text.").await;
    until(&sink, "streamed assistant text", |dom| (assistant_text(dom).len() > 20).then_some(())).await;
    session.interrupt().await.unwrap();
    tokio::time::timeout(TURN_TIMEOUT, session.wait_idle()).await.expect("turn stopped").expect("turn ended");
    let dom = sink.dom();
    let turn = dom.iter().find(|n| n.kind == ElementKind::Turn).unwrap();
    assert_eq!(turn.str_attr(attr::STATUS), Some("cancelled"));
    assert!(!assistant_text(&dom).contains("\n3000"), "the reply was cut short");
    assert!(dom.iter().all(|n| !matches!(n.str_attr(attr::STATUS), Some("streaming" | "running"))), "nothing is left streaming or running");
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs an opencode server"]
async fn the_models_list_contains_the_chosen_model() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, _sink, session) = start(dir.path()).await;
    let models = session.models().await.expect("models listed");
    assert!(!models.is_empty(), "the server lists models");
    let chosen = model();
    assert!(models.iter().any(|m| m.id == chosen), "{chosen} is not among {:?}", models.iter().map(|m| &m.id).collect::<Vec<_>>());
    session.shutdown().await.unwrap();
}
