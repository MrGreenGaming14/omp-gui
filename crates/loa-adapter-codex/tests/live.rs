//! Opt-in tests against the installed `codex`, with real model calls.
//! Run with `cargo test -p loa-adapter-codex --test live -- --ignored`.
//! `LOA_LIVE_MODEL` picks the model (default: codex's configured default).

use loa_adapter::{Adapter, Answer, HarnessSession, Prompt, SessionConfig};
use loa_adapter_codex::CodexAdapter;
use loa_core::{Dom, ElementKind, MemorySink, attr};
use std::sync::Arc;
use std::time::Duration;

const TURN_TIMEOUT: Duration = Duration::from_secs(240);

async fn start(dir: &std::path::Path) -> (Arc<MemorySink>, Box<dyn HarnessSession>) {
    let adapter = CodexAdapter::new("live-test");
    let sink = Arc::new(MemorySink::new());
    let config = SessionConfig {
        session_id: "live-1".into(),
        node: "live-test".into(),
        cwd: dir.to_owned(),
        model: std::env::var("LOA_LIVE_MODEL").ok(),
        ..Default::default()
    };
    let session = adapter.start(config, sink.clone()).await.expect("codex starts its app server");
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed codex with real model calls"]
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
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed codex with real model calls"]
async fn a_real_tool_call_becomes_a_call_with_its_result() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secret.txt"), "the answer is 4217\n").unwrap();
    let (sink, session) = start(dir.path()).await;
    session
        .send(Prompt {
            text: "Run the shell command `cat secret.txt` in the current directory, then tell me the number in the file.".into(),
            by: "live-test".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    // Codex may ask for approval before running the command; allow whatever
    // it asks until the turn ends.
    let deadline = tokio::time::Instant::now() + TURN_TIMEOUT;
    let mut idle = Box::pin(session.wait_idle());
    let mut answered = std::collections::HashSet::new();
    loop {
        let waiting: Vec<_> = sink
            .dom()
            .iter()
            .filter(|n| n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some("waiting"))
            .map(|n| n.id)
            .filter(|id| answered.insert(*id))
            .collect();
        for decision in waiting {
            session
                .answer(decision, Answer { values: vec![loa_core::answer::ALLOW_ONCE.into()], reason: None, by: "live-test".into() })
                .await
                .unwrap();
        }
        match tokio::time::timeout(Duration::from_millis(200), &mut idle).await {
            Ok(done) => {
                done.expect("turn ended");
                break;
            }
            Err(_) => assert!(tokio::time::Instant::now() < deadline, "turn did not finish in time"),
        }
    }
    drop(idle);
    let dom = sink.dom();
    let call = dom.iter().find(|n| n.kind == ElementKind::Call).expect("codex called a tool");
    assert!(dom.children_of_kind(call.id, ElementKind::Result).next().is_some(), "the call has a result");
    assert!(assistant_text(&dom).contains("4217"), "assistant said {:?}", assistant_text(&dom));
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed codex with real model calls"]
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
#[ignore = "runs the installed codex"]
async fn models_lists_the_model_the_session_uses() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, session) = start(dir.path()).await;
    let models = session.models().await.expect("model/list answers");
    assert!(!models.is_empty(), "codex lists models");
    let used = std::env::var("LOA_LIVE_MODEL").ok().or_else(|| {
        let dom = sink.dom();
        dom.root().and_then(|r| r.str_attr(attr::MODEL)).map(str::to_owned)
    });
    if let Some(used) = used {
        assert!(models.iter().any(|m| m.id == used), "{used:?} is not in {:?}", models.iter().map(|m| &m.id).collect::<Vec<_>>());
    }
    session.shutdown().await.unwrap();
}
