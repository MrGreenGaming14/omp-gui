use super::{OpenCodeAdapter, event::Event, mapper::{EventMapper, RootInfo}, native};
use axum::{Json, Router, extract::{Path, State}, response::sse::{Event as SseEvent, Sse}, routing::{get, post}};
use futures::stream;
use loa_contracts::harness::{Adapter, Answer, Prompt, SessionConfig};
use loa_core::{ElementKind as K, MemorySink, attr};
use serde_json::{Value as JsonValue, json};
use std::{convert::Infallible, sync::{Arc, Mutex}};
use tokio::sync::mpsc;

#[test]
fn native_export_preserves_unknown_fields_and_parts() {
    let fixture = json!({
        "info": {"id":"ses_fixture", "title":"Example", "directory":"/work", "time":{"created":100,"updated":130,"future":42}, "projectID":"p", "version":"1", "plugin":{"flag":true}},
        "messages": [
            {"info":{"id":"msg_user", "sessionID":"ses_fixture", "role":"user", "time":{"created":101}, "model":{"providerID":"p","modelID":"m"}, "future":"yes"},
             "parts":[{"id":"prt_user", "sessionID":"ses_fixture", "messageID":"msg_user", "type":"text", "text":"hi", "future":{"x":2}}]},
            {"info":{"id":"msg_assistant", "sessionID":"ses_fixture", "role":"assistant", "parentID":"msg_user", "time":{"created":102,"completed":130}, "providerID":"p", "modelID":"m", "tokens":{"input":2,"output":4}, "future":false},
             "parts":[{"id":"prt_reason", "sessionID":"ses_fixture", "messageID":"msg_assistant", "type":"reasoning", "text":"thinking", "time":{"start":103,"end":104}, "future":[1,2]},
                      {"id":"prt_tool", "sessionID":"ses_fixture", "messageID":"msg_assistant", "type":"tool", "callID":"call_1", "tool":"bash", "state":{"status":"completed", "input":{"command":"ls"}, "output":"ok", "title":"List", "time":{"start":105,"end":106}}, "future":"tool"},
                      {"id":"prt_step", "sessionID":"ses_fixture", "messageID":"msg_assistant", "type":"step-finish", "reason":"stop", "future":9},
                      {"id":"prt_answer", "sessionID":"ses_fixture", "messageID":"msg_assistant", "type":"text", "text":"done", "time":{"start":110,"end":130}}]}
        ]
    });
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.json");
    std::fs::write(&source, serde_json::to_vec(&fixture).unwrap()).unwrap();
    let sink = MemorySink::new();
    native::import(&source, &sink).unwrap();
    let dom = sink.dom();
    let root = dom.root().unwrap();
    let turn = dom.children_of_kind(root.id, K::Turn).next().unwrap();
    assert_eq!(turn.str_attr(attr::ID), Some("msg_user"));
    assert!(dom.children_of_kind(turn.id, K::Call).next().is_some());
    let dest = dir.path().join("dest.json");
    native::export(&dom, &dest).unwrap();
    let result: JsonValue = serde_json::from_slice(&std::fs::read(dest).unwrap()).unwrap();
    assert_eq!(result, fixture);
}

#[test]
fn scripted_stream_maps_decisions_todos_compaction_and_child_jobs() {
    let sink = MemorySink::new();
    let mut mapper = EventMapper::start(&sink, RootInfo {
        session_id: "app".into(), native: json!({"id":"ses_1", "time":{"created":1}}), ..Default::default()
    }, 1).unwrap();
    let events = [
        json!({"type":"message.updated","properties":{"sessionID":"ses_1","info":{"id":"msg_u","sessionID":"ses_1","role":"user","time":{"created":2}}}}),
        json!({"type":"message.part.updated","properties":{"sessionID":"ses_1","part":{"id":"prt_u","messageID":"msg_u","sessionID":"ses_1","type":"text","text":"hello"}}}),
        json!({"type":"message.updated","properties":{"sessionID":"ses_1","info":{"id":"msg_a","sessionID":"ses_1","role":"assistant","parentID":"msg_u","providerID":"p","modelID":"m","time":{"created":3}}}}),
        json!({"type":"message.part.updated","properties":{"sessionID":"ses_1","part":{"id":"prt_t","sessionID":"ses_1","messageID":"msg_a","type":"tool","callID":"call_1","tool":"task","state":{"status":"running","input":{"description":"delegate"},"time":{"start":4}}}}}),
        json!({"type":"permission.asked","properties":{"id":"perm_1","sessionID":"ses_1","permission":"bash","patterns":["*"],"tool":{"messageID":"msg_a","callID":"call_1"}}}),
        json!({"type":"todo.updated","properties":{"sessionID":"ses_1","todos":[{"content":"Implement","status":"in_progress","priority":"high"}]}}),
        json!({"type":"message.part.updated","properties":{"sessionID":"ses_1","part":{"id":"prt_t","sessionID":"ses_1","messageID":"msg_a","type":"tool","callID":"call_1","tool":"task","state":{"status":"completed","input":{"description":"delegate"},"output":"child finished","metadata":{"sessionId":"ses_child"},"time":{"start":4,"end":9}}}}}),
        json!({"type":"message.updated","properties":{"sessionID":"ses_1","info":{"id":"msg_a","sessionID":"ses_1","role":"assistant","parentID":"msg_u","providerID":"p","modelID":"m","time":{"created":3,"completed":10}}}}),
        json!({"type":"session.idle","properties":{"sessionID":"ses_1"}}),
    ];
    for (i, event) in events.into_iter().enumerate() {
        mapper.apply(&sink, Event::parse(&event.to_string()).unwrap(), i as i64 + 2).unwrap();
    }
    let dom = sink.dom();
    let root = dom.root().unwrap();
    let turn = dom.children_of_kind(root.id, K::Turn).next().unwrap();
    let call = dom.children_of_kind(turn.id, K::Call).next().unwrap();
    assert_eq!(call.str_attr(attr::STATUS), Some("done"));
    assert_eq!(turn.str_attr(attr::STATUS), Some("done"));
    assert!(dom.children_of_kind(call.id, K::Decision).next().is_some());
    assert_eq!(dom.children_of_kind(root.id, K::Todo).next().unwrap().text, "Implement");
}

#[derive(Clone)]
struct Fake {
    events: Arc<Mutex<Option<mpsc::Receiver<String>>>>,
    requests: mpsc::UnboundedSender<(String, JsonValue)>,
}

async fn stream_events(State(fake): State<Fake>) -> Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = fake.events.lock().unwrap().take().unwrap();
    Sse::new(stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|data| (Ok(SseEvent::default().data(data)), rx)) }))
}
async fn create(State(fake): State<Fake>, Json(body): Json<JsonValue>) -> Json<JsonValue> {
    fake.requests.send(("create".into(), body)).unwrap();
    Json(json!({"id":"ses_1", "time":{"created":100}, "directory":"/work"}))
}
async fn prompt(Path(_id): Path<String>, State(fake): State<Fake>, Json(body): Json<JsonValue>) {
    fake.requests.send(("prompt".into(), body)).unwrap();
}
async fn abort(Path(_id): Path<String>, State(fake): State<Fake>) {
    fake.requests.send(("abort".into(), json!({}))).unwrap();
}
async fn permission(Path(id): Path<String>, State(fake): State<Fake>, Json(body): Json<JsonValue>) {
    fake.requests.send((id, body)).unwrap();
}

#[tokio::test]
async fn http_driver_prompts_receives_events_replies_and_aborts() {
    let (events_tx, events_rx) = mpsc::channel(16);
    let (requests_tx, mut requests_rx) = mpsc::unbounded_channel();
    let fake = Fake { events: Arc::new(Mutex::new(Some(events_rx))), requests: requests_tx };
    let router = Router::new().route("/event", get(stream_events)).route("/session", post(create))
        .route("/session/{id}/prompt_async", post(prompt)).route("/session/{id}/abort", post(abort))
        .route("/permission/{id}/reply", post(permission)).with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let adapter = OpenCodeAdapter::new(format!("http://{addr}"), None);
    let sink = Arc::new(MemorySink::new());
    let config = SessionConfig { session_id: "app_1".into(), cwd: "/work".into(), model: Some("p/m".into()), ..SessionConfig::default() };
    let session = adapter.start(config, sink.clone()).await.unwrap();
    assert_eq!(requests_rx.recv().await.unwrap().0, "create");
    session.send(Prompt { text: "hello".into(), by: "me".into(), ..Prompt::default() }).await.unwrap();
    let (_, sent) = requests_rx.recv().await.unwrap();
    assert_eq!(sent["parts"][0]["text"], "hello");
    assert_eq!(sent["model"], json!({"providerID":"p", "modelID":"m"}));
    let part = json!({"type":"message.updated","properties":{"sessionID":"ses_1","info":{"id":"msg_a","sessionID":"ses_1","role":"assistant","providerID":"p","modelID":"m","time":{"created":101}}}});
    events_tx.send(part.to_string()).await.unwrap();
    events_tx.send(json!({"type":"message.part.updated","properties":{"sessionID":"ses_1","part":{"id":"prt_a","sessionID":"ses_1","messageID":"msg_a","type":"text","text":"answer"}}}).to_string()).await.unwrap();
    events_tx.send(json!({"type":"permission.asked","properties":{"id":"perm_1","sessionID":"ses_1","permission":"bash","patterns":["*"]}}).to_string()).await.unwrap();
    let decision = loop {
        let notified = sink.with_dom(|dom| dom.descendants(dom.root().unwrap().id).find(|n| n.kind == K::Decision).map(|n| n.id));
        if let Some(id) = notified { break id; }
        tokio::task::yield_now().await;
    };
    session.answer(decision, Answer { values: vec!["allow_once".into()], by: "me".into(), ..Answer::default() }).await.unwrap();
    assert_eq!(requests_rx.recv().await.unwrap(), ("perm_1".into(), json!({"reply":"once"})));
    events_tx.send(json!({"type":"session.idle","properties":{"sessionID":"ses_1"}}).to_string()).await.unwrap();
    session.wait_idle().await.unwrap();
    let dom = sink.dom();
    let turn = dom.children_of_kind(dom.root().unwrap().id, K::Turn).next().unwrap();
    assert_eq!(turn.str_attr(attr::STATUS), Some("done"));
    assert!(dom.children_of_kind(turn.id, K::Message).any(|node| node.text == "answer"));
    session.send(Prompt { text: "again".into(), ..Prompt::default() }).await.unwrap();
    assert_eq!(requests_rx.recv().await.unwrap().0, "prompt");
    session.interrupt().await.unwrap();
    assert_eq!(requests_rx.recv().await.unwrap().0, "abort");
    session.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn availability_probes_the_server_and_models_come_from_its_providers() {
    use loa_contracts::harness::AvailabilityStatus;
    // A port nothing listens on: bind, read the port, close.
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
    let down = OpenCodeAdapter::new(format!("http://{closed}"), None).availability().await;
    assert_eq!(down.status, AvailabilityStatus::Unreachable);
    assert!(down.detail.unwrap().contains("opencode serve"));

    let (_events_tx, events_rx) = mpsc::channel::<String>(1);
    let (requests_tx, _requests_rx) = mpsc::unbounded_channel();
    let fake = Fake { events: Arc::new(Mutex::new(Some(events_rx))), requests: requests_tx };
    let providers = json!({
        "providers": [
            {"id": "anthropic", "name": "Anthropic", "models": {"claude-haiku": {"id": "claude-haiku", "name": "Claude Haiku"}}},
            {"id": "openai", "name": "OpenAI", "models": {"gpt-mini": {"id": "gpt-mini"}}}
        ],
        "default": {"anthropic": "claude-haiku"}
    });
    let router = Router::new()
        .route("/global/health", get(|| async { Json(json!({"healthy": true, "version": "1.4.2"})) }))
        .route("/config/providers", get(move || async move { Json(providers) }))
        .route("/event", get(stream_events))
        .route("/session", post(create))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let adapter = OpenCodeAdapter::new(format!("http://{addr}"), None);
    let up = adapter.availability().await;
    assert!(up.is_ready(), "{up:?}");
    assert_eq!(up.version.as_deref(), Some("1.4.2"));

    let config = SessionConfig { session_id: "app_1".into(), cwd: "/work".into(), ..SessionConfig::default() };
    let session = adapter.start(config, Arc::new(MemorySink::new())).await.unwrap();
    let models: Vec<(String, String, String)> =
        session.models().await.unwrap().into_iter().map(|m| (m.id, m.name, m.provider)).collect();
    assert_eq!(models, [
        ("anthropic/claude-haiku".to_owned(), "Claude Haiku".to_owned(), "anthropic".to_owned()),
        ("openai/gpt-mini".to_owned(), "gpt-mini".to_owned(), "openai".to_owned()),
    ]);
    session.shutdown().await.unwrap();
    server.abort();
}
