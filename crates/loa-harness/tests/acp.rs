//! ACP transcript replays, host confinement, and a session with a fake agent.

use loa_contracts::harness::{Adapter, Prompt, SessionConfig};
use loa_core::{Draft, ElementKind, MemorySink, NodeId, PatchSink, attr};
use loa_harness::acp::host::Host;
use loa_harness::acp::mapper::{AcpMapper, Reply};
use loa_harness::acp::AcpAdapter;
use loa_harness::testing::{FakeProcess, WatchSink, golden, replay};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn transcripts_replay_to_their_goldens() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/acp");
    let mut fixtures: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect();
    fixtures.sort();
    assert!(!fixtures.is_empty());
    for fixture in fixtures {
        let sink = MemorySink::new();
        let attrs = [(attr::ID, "s".into()), (attr::HARNESS, "acp".into()), (attr::NODE, "owner".into()), (attr::CREATED, 1_i64.into()), (attr::SCHEMA, 1_i64.into())];
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "")).unwrap().node;
        let mut mapper = AcpMapper::new(root, Some("fake-model".into()));
        mapper.adopt_native("s-1");
        let replies = replay(mapper, &sink, &fixture);
        let xml = loa_core::xml::to_string(&sink.dom());
        let mut out: String = xml.lines().filter(|line| !line.starts_with("<!--")).map(|line| format!("{line}\n")).collect();
        out.push_str("\n--- replies\n");
        for reply in replies {
            match reply {
                Reply::Respond { id, result } => out.push_str(&format!("{}\n", json!({"id": id, "result": result}))),
                Reply::Reject { id, message } => out.push_str(&format!("{}\n", json!({"id": id, "error": message}))),
                Reply::Cancel => out.push_str("notify session/cancel\n"),
            }
        }
        golden(&fixture.with_extension("golden"), &out);
    }
}

#[tokio::test]
async fn host_refuses_paths_outside_the_session_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let outside = dir.path().join("secret");
    std::fs::write(&outside, "secret").unwrap();
    let host = Host::new(&root).unwrap();
    host.serve("fs/write_text_file", json!({"path":"nested/file", "content":"one\ntwo\n"})).await.unwrap();
    assert_eq!(host.serve("fs/read_text_file", json!({"path":"nested/file", "line":2, "limit":1})).await.unwrap(), json!({"content":"two\n"}));
    for path in ["../secret".to_owned(), outside.to_string_lossy().into_owned()] {
        assert!(host.serve("fs/read_text_file", json!({"path":path})).await.is_err());
        assert!(host.serve("fs/write_text_file", json!({"path":path, "content":"overwritten"})).await.is_err());
        assert!(host.serve("terminal/create", json!({"command":"true", "cwd":path})).await.is_err());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.path(), root.join("escape")).unwrap();
        assert!(host.serve("fs/read_text_file", json!({"path":"escape/secret"})).await.is_err());
        assert!(host.serve("fs/write_text_file", json!({"path":"escape/new", "content":"bad"})).await.is_err());
        assert!(host.serve("terminal/create", json!({"command":"true", "cwd":"escape"})).await.is_err());
        assert!(!dir.path().join("new").exists());
    }
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "secret");
}

#[cfg(unix)]
#[tokio::test]
async fn host_terminal_runs_and_releases_inside_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::new(dir.path()).unwrap();
    let created = host.serve("terminal/create", json!({"command":"sh", "args":["-c", "printf 'hello'"]})).await.unwrap();
    let id = created["terminalId"].as_str().unwrap();
    let terminal = json!({"terminalId":id});
    let exit = tokio::time::timeout(Duration::from_secs(20), host.serve("terminal/wait_for_exit", terminal.clone())).await.unwrap().unwrap();
    assert_eq!(exit["exitCode"], 0);
    let output = host.serve("terminal/output", terminal.clone()).await.unwrap();
    assert_eq!(output["output"], "hello");
    assert_eq!(output["truncated"], false);
    host.serve("terminal/release", terminal.clone()).await.unwrap();
    assert!(host.serve("terminal/output", terminal).await.is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn terminals_run_in_a_plain_non_verbatim_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = loa_util::fs::canonicalize(&cwd).unwrap();
    assert!(!cwd.to_string_lossy().starts_with(r"\\?\"));
    let host = Host::new(&cwd).unwrap();

    let request = json!({ "command": "cmd", "args": ["/C", "cd & echo %GREETING% 1>&2 & exit 3"], "env": [{ "name": "GREETING", "value": "hi" }] });
    let terminal = json!({ "terminalId": host.serve("terminal/create", request).await.unwrap()["terminalId"].clone() });
    let exit = tokio::time::timeout(Duration::from_secs(20), host.serve("terminal/wait_for_exit", terminal.clone())).await.unwrap().unwrap();
    assert_eq!(exit["exitCode"], 3);
    let output = host.serve("terminal/output", terminal).await.unwrap();
    let text = output["output"].as_str().unwrap();
    assert!(text.contains(cwd.to_str().unwrap()), "{text}");
    assert!(text.contains("hi"), "{text}");

    let request = json!({ "command": "ping", "args": ["-n", "600", "127.0.0.1"] });
    let terminal = json!({ "terminalId": host.serve("terminal/create", request).await.unwrap()["terminalId"].clone() });
    host.serve("terminal/kill", terminal.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), host.serve("terminal/wait_for_exit", terminal)).await.unwrap().unwrap();
}

#[tokio::test]
async fn session_streams_and_settles_against_a_fake_agent() {
    let script = concat!(
        "< initialize init\n",
        "> {\"jsonrpc\":\"2.0\",\"id\":\"{{init}}\",\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{}}}\n",
        "< session/new new\n",
        "> {\"jsonrpc\":\"2.0\",\"id\":\"{{new}}\",\"result\":{\"sessionId\":\"native-1\"}}\n",
        "< session/prompt prompt\n",
        "> {\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"native-1\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Hello\"}}}}\n",
        "> {\"jsonrpc\":\"2.0\",\"id\":\"{{prompt}}\",\"result\":{\"stopReason\":\"end_turn\"}}\n",
    );
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeProcess::install(dir.path(), script).unwrap();
    let adapter = AcpAdapter::new(fake.executable(), std::iter::empty::<String>());
    let sink = Arc::new(WatchSink::default());
    let config = SessionConfig { session_id: "app-1".into(), cwd: dir.path().to_owned(), ..Default::default() };
    let session = adapter.start(config, sink.clone()).await.unwrap();
    session.send(Prompt { text: "Say hello".into(), by: "owner".into(), ..Default::default() }).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), session.wait_idle()).await.unwrap().unwrap();
    let dom = sink.inner.dom();
    assert_eq!(dom.root().unwrap().str_attr(attr::NATIVE_ID), Some("native-1"));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Message && n.text == "Hello"));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Turn && n.str_attr(attr::STATUS) == Some("done")));
    session.shutdown().await.unwrap();
    let sent = fake.received();
    assert_eq!(sent[0]["method"], "initialize");
    assert_eq!(sent[1]["method"], "session/new");
    assert_eq!(sent[2]["params"]["sessionId"], "native-1");
    assert_eq!(sent[2]["params"]["prompt"][0]["text"], "Say hello");
}

#[test]
fn permission_options_advertise_canonical_session_and_preserve_name_or_id_answers() {
    use loa_contracts::harness::Answer;
    use loa_core::{DomWriter, answer};
    use loa_harness::acp::mapper::Input;
    use loa_harness::kit::EventMapper as _;

    let sink = MemorySink::new();
    let attrs = [(attr::ID, "s".into()), (attr::HARNESS, "acp".into()), (attr::NODE, "owner".into()), (attr::CREATED, 1_i64.into()), (attr::SCHEMA, 1_i64.into())];
    let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "")).unwrap().node;
    let mut mapper = AcpMapper::new(root, None);
    let w = DomWriter::new(&sink).at(0);
    mapper.apply(Input::Prompt { id: "1".into(), prompt: Prompt { text: "go".into(), by: "owner".into(), ..Default::default() } }, &w).unwrap();

    let ask = |id: i64, options: serde_json::Value| Input::Permission {
        id: json!(id),
        params: json!({ "toolCall": { "toolCallId": format!("c{id}"), "title": "run" }, "options": options }),
    };
    let full = json!([
        { "optionId": "opt-once", "name": "Allow", "kind": "allow_once" },
        { "optionId": "opt-always", "name": "Always allow", "kind": "allow_always" },
        { "optionId": "opt-no", "name": "Reject", "kind": "reject_once" }
    ]);
    let once_only = json!([
        { "optionId": "opt-once", "name": "Allow", "kind": "allow_once" },
        { "optionId": "opt-no", "name": "Reject", "kind": "reject_once" }
    ]);

    mapper.apply(ask(1, full.clone()), &w).unwrap();
    mapper.apply(ask(2, full), &w).unwrap();
    mapper.apply(ask(3, once_only), &w).unwrap();

    let decisions: Vec<(NodeId, Vec<String>)> = sink.with_dom(|dom| {
        dom.iter()
            .filter(|n| n.kind == ElementKind::Decision)
            .map(|n| (n.id, n.attr(attr::OPTIONS).unwrap().str_items().map(str::to_owned).collect()))
            .collect()
    });
    assert_eq!(decisions[0].1, [answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY]);
    assert_eq!(decisions[1].1, [answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY]);
    assert_eq!(decisions[2].1, [answer::ALLOW_ONCE, answer::DENY]);

    // Answering by option_id and by option name stays supported alongside canonical answers.
    let by_id = mapper.answer(decisions[0].0, &Answer { values: vec!["opt-always".into()], reason: None, by: "owner".into() }, &w).unwrap();
    assert_eq!(by_id, Some(Reply::Respond { id: json!(1), result: json!({ "outcome": { "optionId": "opt-always", "outcome": "selected" } }) }));

    let by_name = mapper.answer(decisions[1].0, &Answer { values: vec!["Always allow".into()], reason: None, by: "owner".into() }, &w).unwrap();
    assert_eq!(by_name, Some(Reply::Respond { id: json!(2), result: json!({ "outcome": { "optionId": "opt-always", "outcome": "selected" } }) }));

    let by_canonical = mapper.answer(decisions[2].0, &Answer { values: vec![answer::ALLOW_ONCE.into()], reason: None, by: "owner".into() }, &w).unwrap();
    assert_eq!(by_canonical, Some(Reply::Respond { id: json!(3), result: json!({ "outcome": { "optionId": "opt-once", "outcome": "selected" } }) }));
}
