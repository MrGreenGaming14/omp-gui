//! Opt-in tests against the installed `claude`, with real model calls.
//! Run with `cargo test -p loa-adapter-claude --test live -- --ignored`.
//! `LOA_LIVE_MODEL` picks the model (default: claude's configured default).

use loa_adapter::{Adapter, Answer, HarnessSession, Prompt, SessionConfig};
use loa_adapter_claude::ClaudeAdapter;
use loa_core::{Dom, ElementKind, MemorySink, attr};
use std::sync::Arc;
use std::time::Duration;

const TURN_TIMEOUT: Duration = Duration::from_secs(240);

async fn start(dir: &std::path::Path) -> (Arc<MemorySink>, Box<dyn HarnessSession>) {
    let adapter = ClaudeAdapter::new();
    let sink = Arc::new(MemorySink::new());
    let config = SessionConfig {
        session_id: "live-1".into(),
        node: "live-test".into(),
        cwd: dir.to_owned(),
        model: std::env::var("LOA_LIVE_MODEL").ok(),
        ..Default::default()
    };
    let session = adapter.start(config, sink.clone()).await.expect("claude starts in stream-json mode");
    (sink, session)
}

async fn turn(session: &dyn HarnessSession, text: &str) {
    session.send(Prompt { text: text.into(), by: "live-test".into(), ..Default::default() }).await.expect("prompt accepted");
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

/// Sends a prompt and runs it to the end, allowing any permission decision
/// claude raises along the way. Returns how many decisions were answered.
async fn turn_allowing(sink: &MemorySink, session: &dyn HarnessSession, text: &str) -> usize {
    session.send(Prompt { text: text.into(), by: "live-test".into(), ..Default::default() }).await.expect("prompt accepted");
    let mut answered = 0;
    tokio::time::timeout(TURN_TIMEOUT, async {
        loop {
            let waiting = sink.dom().iter().find(|n| n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some("waiting")).map(|n| n.id);
            if let Some(decision) = waiting {
                session
                    .answer(decision, Answer { values: vec![loa_core::answer::ALLOW_ONCE.into()], reason: None, by: "live-test".into() })
                    .await
                    .expect("decision answered");
                answered += 1;
                continue;
            }
            if tokio::time::timeout(Duration::from_millis(100), session.wait_idle()).await.is_ok() {
                // Idle is only final when no decision slipped in meanwhile.
                let still_waiting = sink.dom().iter().any(|n| n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some("waiting"));
                if !still_waiting {
                    return;
                }
            }
        }
    })
    .await
    .expect("turn finished in time");
    answered
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed claude with real model calls"]
async fn a_real_turn_streams_an_assistant_reply_into_the_dom() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, session) = start(dir.path()).await;
    turn(session.as_ref(), "Reply with exactly the single word: pong").await;
    let dom = sink.dom();
    let reply = assistant_text(&dom);
    assert!(reply.to_lowercase().contains("pong"), "assistant said {reply:?}");
    let turn = dom.iter().find(|n| n.kind == ElementKind::Turn).expect("a turn");
    assert_eq!(turn.str_attr(attr::STATUS), Some("done"));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == Some("user") && n.text.contains("pong")));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Usage), "the call's usage is recorded");
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed claude with real model calls"]
async fn a_real_tool_call_becomes_a_call_with_its_result() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secret.txt"), "the answer is 4217\n").unwrap();
    let (sink, session) = start(dir.path()).await;
    // Claude may ask permission for the read or may not, depending on its
    // configured mode; either way the turn has to finish with the result.
    turn_allowing(&sink, session.as_ref(), "Use your Read tool to read secret.txt in the current directory, then tell me the number in it.").await;
    let dom = sink.dom();
    let call = dom.iter().find(|n| n.kind == ElementKind::Call).expect("claude called a tool");
    assert!(dom.children_of_kind(call.id, ElementKind::Result).next().is_some(), "the call has a result");
    assert!(assistant_text(&dom).contains("4217"), "assistant said {:?}", assistant_text(&dom));
    assert!(dom.iter().all(|n| n.kind != ElementKind::Decision || n.str_attr(attr::STATUS) != Some("waiting")), "no decision left waiting");
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed claude with real model calls"]
async fn interrupt_cancels_a_streaming_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, session) = start(dir.path()).await;
    // Long enough that the abort always lands while the reply streams.
    session
        .send(Prompt { text: "Write the numbers from 1 to 3000, one per line, with no other text.".into(), by: "live-test".into(), ..Default::default() })
        .await
        .unwrap();
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
#[ignore = "runs the installed claude with real model calls"]
async fn the_harness_lists_its_models() {
    let dir = tempfile::tempdir().unwrap();
    let (_sink, session) = start(dir.path()).await;
    let models = session.models().await.expect("claude lists its models");
    assert!(!models.is_empty(), "at least one model is listed");
    assert!(models.iter().all(|m| !m.id.is_empty()), "every model has an id");
    session.shutdown().await.unwrap();
}
