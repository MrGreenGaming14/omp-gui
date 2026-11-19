//! Opt-in tests against the installed `omp`, with real model calls.
//! Run with `cargo test -p loa-adapter-omp --test live -- --ignored`.
//! `LOA_LIVE_MODEL` picks the model (default: omp's configured default).

use loa_adapter::{Adapter, Answer, HarnessSession, ModelSelection, Prompt, SessionConfig, SessionState};
use loa_adapter_omp::{OmpAdapter, OmpAdapterConfig};
use loa_core::{Dom, ElementKind, MemorySink, attr};
use std::sync::Arc;
use std::time::Duration;

const TURN_TIMEOUT: Duration = Duration::from_secs(240);

async fn start(dir: &std::path::Path) -> (Arc<MemorySink>, Box<dyn HarnessSession>) {
    start_with(dir, None, Vec::new()).await
}

async fn start_with(dir: &std::path::Path, approval_mode: Option<&str>, disabled_tools: Vec<String>) -> (Arc<MemorySink>, Box<dyn HarnessSession>) {
    let mut adapter_config = OmpAdapterConfig::new("live-test", dir.join("state"));
    adapter_config.approval_mode = approval_mode.map(str::to_owned);
    let adapter = OmpAdapter::new(adapter_config);
    let sink = Arc::new(MemorySink::new());
    let config = SessionConfig {
        session_id: "live-1".into(),
        node: "live-test".into(),
        cwd: dir.to_owned(),
        model: std::env::var("LOA_LIVE_MODEL").ok(),
        disabled_tools,
        ..Default::default()
    };
    let session = adapter.start(config, sink.clone()).await.expect("omp starts in rpc mode");
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
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
    // omp's TUI bookkeeping adds no rows; the call's usage is recorded.
    assert!(!dom.iter().any(|n| matches!(n.kind, ElementKind::Ext | ElementKind::Call)), "{:?}", dom.iter().map(|n| n.kind).collect::<Vec<_>>());
    assert!(dom.iter().any(|n| n.kind == ElementKind::Usage && n.attrs.contains_key(attr::COST)));
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn a_real_tool_call_becomes_a_call_with_its_result() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secret.txt"), "the answer is 4217\n").unwrap();
    let (sink, session) = start(dir.path()).await;
    turn(session.as_ref(), "Use your read tool to read secret.txt in the current directory, then tell me the number in it.").await;
    let dom = sink.dom();
    let call = dom.iter().find(|n| n.kind == ElementKind::Call).expect("omp called a tool");
    assert!(dom.children_of_kind(call.id, ElementKind::Result).next().is_some(), "the call has a result");
    assert!(assistant_text(&dom).contains("4217"), "assistant said {:?}", assistant_text(&dom));
    session.shutdown().await.unwrap();
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
#[ignore = "runs the installed omp with real model calls"]
async fn a_bash_call_waits_on_an_approval_card_and_runs_once_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, session) = start_with(dir.path(), Some("always-ask"), Vec::new()).await;
    session
        .send(Prompt { text: "Run the shell command `echo loa-approved-7731` with your bash tool and tell me what it printed.".into(), by: "live-test".into(), ..Default::default() })
        .await
        .unwrap();
    let decision = until(&sink, "an approval decision", |dom| {
        dom.iter().find(|n| n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some("waiting")).map(|n| n.id)
    })
    .await;
    assert_eq!(session.state(), SessionState::Waiting);
    session.answer(decision, Answer { values: vec![loa_core::answer::ALLOW_ONCE.into()], reason: None, by: "live-test".into() }).await.unwrap();
    tokio::time::timeout(TURN_TIMEOUT, session.wait_idle()).await.expect("turn finished").expect("turn ended");
    let dom = sink.dom();
    let answered = dom.get(decision).unwrap();
    assert_eq!(answered.str_attr(attr::STATUS), Some("answered"));
    assert_eq!(answered.str_attr(attr::BY), Some("live-test"));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Result && n.text.contains("loa-approved-7731")), "the approved command ran");
    assert_eq!(dom.iter().filter(|n| n.kind == ElementKind::Turn).count(), 1, "one prompt, one turn");
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
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
#[ignore = "runs the installed omp with real model calls"]
async fn a_prompt_sent_mid_turn_runs_after_the_first_in_its_own_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, session) = start(dir.path()).await;
    session
        .send(Prompt { text: "Write the numbers from 1 to 120, one per line, with no other text.".into(), by: "live-test".into(), ..Default::default() })
        .await
        .unwrap();
    until(&sink, "streamed assistant text", |dom| (!assistant_text(dom).is_empty()).then_some(())).await;
    turn(session.as_ref(), "Now reply with exactly the single word: second").await;
    let dom = sink.dom();
    let turns: Vec<_> = dom.iter().filter(|n| n.kind == ElementKind::Turn).collect();
    assert_eq!(turns.len(), 2, "both prompts have a turn");
    assert!(turns.iter().all(|t| t.str_attr(attr::STATUS) == Some("done")));
    let in_turn = |turn: loa_core::NodeId, role: &str| -> String {
        dom.children_of_kind(turn, ElementKind::Message)
            .filter(|m| m.str_attr(attr::ROLE) == Some(role) && m.str_attr(attr::KIND) != Some("thinking"))
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(in_turn(turns[0].id, "user").contains("1 to 120"));
    assert!(in_turn(turns[0].id, "assistant").contains("\n120"), "the first reply finished");
    assert!(in_turn(turns[1].id, "user").contains("second"));
    assert!(in_turn(turns[1].id, "assistant").to_lowercase().contains("second"));
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp"]
async fn set_model_and_effort_switch_to_a_listed_model() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, session) = start(dir.path()).await;
    let current = sink.dom().root().unwrap().str_attr(attr::MODEL).map(str::to_owned).expect("the session names its model");
    let models = session.models().await.unwrap();
    assert!(models.iter().any(|m| m.id == current), "{current} is listed");
    let other = models.iter().find(|m| m.id != current && m.provider == current.split('/').next().unwrap() && !m.efforts.is_empty()).expect("another model of the provider");
    session.set_model(ModelSelection { model: other.id.clone(), role: None }).await.unwrap();
    assert_eq!(sink.dom().root().unwrap().str_attr(attr::MODEL), Some(other.id.as_str()));
    let effort = other.efforts[0].clone();
    session.set_effort(effort.clone()).await.unwrap();
    assert_eq!(sink.dom().root().unwrap().str_attr(attr::EFFORT), Some(effort.as_str()));
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn a_disabled_tool_is_not_offered() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, session) = start_with(dir.path(), None, vec!["bash".into(), "eval".into()]).await;
    turn(session.as_ref(), "If you have a tool named bash, reply with exactly YES-BASH. Otherwise reply with exactly NO-BASH. Do not call any tool.").await;
    let dom = sink.dom();
    assert!(assistant_text(&dom).contains("NO-BASH"), "assistant said {:?}", assistant_text(&dom));
    assert!(dom.iter().all(|n| n.kind != ElementKind::Call || n.str_attr(attr::TOOL) != Some("bash")));
    session.shutdown().await.unwrap();
}