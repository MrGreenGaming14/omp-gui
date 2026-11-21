//! opencode: event replays against goldens, the native round trip, and a session against a fake server.

use axum::extract::{Path as UrlPath, State};
use axum::response::sse::{Event as SseEvent, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use loa_contracts::harness::{Adapter, Answer, AvailabilityStatus, Prompt, SessionConfig};
use loa_core::{DomWriter, ElementKind as K, MemorySink, attr};
use loa_harness::kit::NativeCodec;
use loa_harness::opencode::mapper::{Mapper, RootInfo};
use loa_harness::opencode::{OpenCodeAdapter, OpenCodeNative};
use loa_harness::testing::{golden, replay};
use serde_json::{Value, json};
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

fn fixtures(ext: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/opencode");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == ext)).collect();
    files.sort();
    assert!(!files.is_empty());
    files
}

#[test]
fn transcripts_replay_to_their_goldens() {
    for fixture in fixtures("jsonl") {
        let sink = MemorySink::new();
        let root = RootInfo { session_id: "s".into(), node: "owner".into(), native: json!({"id":"ses_1","time":{"created":1}}), ..Default::default() };
        let mapper = Mapper::start(&DomWriter::new(&sink).at(0), root).unwrap();
        assert!(replay(mapper, &sink, &fixture).is_empty());
        let xml = loa_core::xml::to_string(&sink.dom());
        let out: String = xml.lines().filter(|l| !l.starts_with("<!--")).map(|l| format!("{l}\n")).collect();
        golden(&fixture.with_extension("golden"), &out);
    }
}

#[test]
fn native_exports_reproduce_their_imports() {
    for fixture in fixtures("json") {
        let sink = MemorySink::new();
        OpenCodeNative::import(&fixture, &sink).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.json");
        OpenCodeNative::export(&sink.dom(), &out).unwrap();
        let read = |p: &Path| serde_json::from_str::<Value>(&std::fs::read_to_string(p).unwrap()).unwrap();
        assert_eq!(read(&out), read(&fixture), "{}", fixture.display());
    }
}

/// A fake `opencode serve`: events are fed through a channel, requests are recorded.
#[derive(Clone)]
struct Fake {
    events: Arc<Mutex<Option<mpsc::Receiver<String>>>>,
    requests: mpsc::UnboundedSender<(String, Value)>,
}

async fn serve(fake: Fake) -> (String, tokio::task::JoinHandle<()>) {
    async fn events(State(fake): State<Fake>) -> Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>> {
        let rx = fake.events.lock().unwrap().take().unwrap();
        Sse::new(futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|data| (Ok(SseEvent::default().data(data)), rx)) }))
    }
    async fn record(UrlPath(path): UrlPath<String>, State(fake): State<Fake>, body: Option<Json<Value>>) -> Json<Value> {
        fake.requests.send((path, body.map_or(Value::Null, |b| b.0))).unwrap();
        Json(json!({"id":"ses_1", "time":{"created":100}, "directory":"/work"}))
    }
    let providers = json!({"providers": [
        {"id": "anthropic", "models": {"claude-haiku": {"id": "claude-haiku", "name": "Claude Haiku"}}},
        {"id": "openai", "models": {"gpt-mini": {"id": "gpt-mini"}}}
    ]});
    let router = Router::new()
        .route("/global/health", get(|| async { Json(json!({"healthy": true, "version": "1.4.2"})) }))
        .route("/config/providers", get(move || async move { Json(providers) }))
        .route("/event", get(events))
        .route("/{*path}", post(record))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (format!("http://{addr}"), tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }))
}

#[tokio::test]
async fn a_session_prompts_answers_and_aborts_against_a_fake_server() {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
    let down = OpenCodeAdapter::new(format!("http://{closed}"), None).availability().await;
    assert_eq!(down.status, AvailabilityStatus::Unreachable);
    assert!(down.detail.unwrap().contains("opencode serve"));

    let (events_tx, events_rx) = mpsc::channel(16);
    let (requests_tx, mut requests) = mpsc::unbounded_channel();
    let (base, server) = serve(Fake { events: Arc::new(Mutex::new(Some(events_rx))), requests: requests_tx }).await;
    let adapter = OpenCodeAdapter::new(base, None);
    let up = adapter.availability().await;
    assert_eq!(up.version.as_deref(), Some("1.4.2"), "{up:?}");
    let sink = Arc::new(MemorySink::new());
    let config = SessionConfig { session_id: "app_1".into(), cwd: "/work".into(), model: Some("p/m".into()), ..SessionConfig::default() };
    let session = adapter.start(config, sink.clone()).await.unwrap();
    assert_eq!(requests.recv().await.unwrap().0, "session");
    let models: Vec<String> = session.models().await.unwrap().into_iter().map(|m| format!("{} {}", m.id, m.name)).collect();
    assert_eq!(models, ["anthropic/claude-haiku Claude Haiku", "openai/gpt-mini gpt-mini"]);

    session.send(Prompt { text: "hello".into(), by: "me".into(), ..Prompt::default() }).await.unwrap();
    let (path, sent) = requests.recv().await.unwrap();
    assert_eq!(path, "session/ses_1/prompt_async");
    assert_eq!((&sent["parts"][0]["text"], &sent["model"]), (&json!("hello"), &json!({"providerID":"p", "modelID":"m"})));
    let user = sent["messageID"].as_str().unwrap().to_owned();
    let events = [
        json!({"type":"message.updated","properties":{"sessionID":"ses_1","info":{"id":user,"sessionID":"ses_1","role":"user","time":{"created":101}}}}),
        json!({"type":"message.part.updated","properties":{"sessionID":"ses_1","part":{"id":"prt_u","sessionID":"ses_1","messageID":user,"type":"text","text":"hello"}}}),
        json!({"type":"message.updated","properties":{"sessionID":"ses_1","info":{"id":"msg_a","sessionID":"ses_1","role":"assistant","parentID":user,"providerID":"p","modelID":"m","time":{"created":101}}}}),
        json!({"type":"message.part.updated","properties":{"sessionID":"ses_1","part":{"id":"prt_a","sessionID":"ses_1","messageID":"msg_a","type":"text","text":"answer"}}}),
        json!({"type":"permission.asked","properties":{"id":"perm_1","sessionID":"ses_1","permission":"bash","patterns":["*"]}}),
    ];
    for event in events {
        events_tx.send(event.to_string()).await.unwrap();
    }
    let decision = loop {
        if let Some(id) = sink.with_dom(|dom| dom.iter().find(|n| n.kind == K::Decision).map(|n| n.id)) {
            break id;
        }
        tokio::task::yield_now().await;
    };
    session.answer(decision, Answer { values: vec!["allow_once".into()], by: "me".into(), ..Answer::default() }).await.unwrap();
    assert_eq!(requests.recv().await.unwrap(), ("permission/perm_1/reply".into(), json!({"reply":"once"})));
    events_tx.send(json!({"type":"session.idle","properties":{"sessionID":"ses_1"}}).to_string()).await.unwrap();
    session.wait_idle().await.unwrap();
    let dom = sink.dom();
    let turn = dom.iter().find(|n| n.kind == K::Turn).unwrap();
    assert_eq!((turn.str_attr(attr::ID), turn.str_attr(attr::STATUS)), (Some(user.as_str()), Some("done")));
    let texts: Vec<_> = dom.children_of_kind(turn.id, K::Message).map(|n| (n.str_attr(attr::ID).unwrap(), n.text.as_str())).collect();
    assert_eq!(texts, [("prt_u", "hello"), ("prt_a", "answer")], "the sent prompt binds to its native part");

    session.send(Prompt { text: "again".into(), ..Prompt::default() }).await.unwrap();
    assert_eq!(requests.recv().await.unwrap().0, "session/ses_1/prompt_async");
    session.interrupt().await.unwrap();
    assert_eq!(requests.recv().await.unwrap().0, "session/ses_1/abort");
    assert_eq!(sink.dom().iter().filter(|n| n.kind == K::Turn).last().unwrap().str_attr(attr::STATUS), Some("cancelled"));
    session.shutdown().await.unwrap();
    server.abort();
}
