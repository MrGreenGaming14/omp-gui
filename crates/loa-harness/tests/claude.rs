//! Claude Code stream transcripts replayed through the mapper against goldens.
//! Lines starting `!` drive the session: `prompt <text>`, `answer <nth
//! decision> <values,…> [| reason]`, `interrupt`, `attach`, `state`, `dom`.

use loa_contracts::harness::{Answer, SessionConfig};
use loa_core::{Dom, ElementKind, MemorySink};
use loa_harness::claude::events::{Mapper, Output};
use loa_harness::testing::{golden, render};
use std::path::Path;
use std::sync::Arc;

fn run(name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/claude");
    let text = std::fs::read_to_string(dir.join(format!("{name}.jsonl"))).unwrap();
    let config = SessionConfig {
        session_id: "app-session".into(),
        node: "node-key".into(),
        cwd: "/work/demo".into(),
        model: Some("claude-sonnet-x".into()),
        ..SessionConfig::default()
    };
    let mut sink = Arc::new(MemorySink::new());
    let mut mapper = Mapper::create(sink.clone(), &config).unwrap();
    let mut log = String::new();
    let mut prompts = 0;
    for line in text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let Some(command) = line.strip_prefix("! ") else {
            match mapper.handle(serde_json::from_str(line).unwrap()) {
                Ok(Some(Output::Write(line))) => log.push_str(&format!("> {line}\n")),
                Ok(Some(Output::Mcp { request_id, server, message })) => log.push_str(&format!("> mcp {request_id} {server} {message}\n")),
                Ok(Some(other)) => log.push_str(&format!("> {other:?}\n")),
                Ok(None) => {}
                Err(err) => log.push_str(&format!("! error {err}\n")),
            }
            continue;
        };
        let (verb, rest) = command.split_once(' ').unwrap_or((command, ""));
        let outcome = match verb {
            "prompt" => {
                prompts += 1;
                mapper.open_turn(rest, "alice", &format!("p{prompts}")).map(drop)
            }
            "answer" => {
                let (nth, rest) = rest.split_once(' ').unwrap();
                let (values, reason) = rest.split_once(" | ").map_or((rest, None), |(v, r)| (v, Some(r.to_owned())));
                let decision = sink.with_dom(|d| d.iter().filter(|n| n.kind == ElementKind::Decision).nth(nth.parse().unwrap()).unwrap().id);
                let answer = Answer { values: values.split(',').map(str::to_owned).collect(), reason, by: "alice".into() };
                mapper.answer(decision, answer).map(|line| line.into_iter().for_each(|line| log.push_str(&format!("> {line}\n"))))
            }
            "interrupt" => mapper.interrupt(),
            "attach" => {
                let dom = sink.dom();
                sink = Arc::new(MemorySink::from_dom(dom.clone()));
                Mapper::attach(sink.clone(), &dom).map(|m| mapper = m)
            }
            "state" => Ok(log.push_str(&format!("= {:?}\n", mapper.state()))),
            "dom" => Ok(log.push_str(&render(&sink.dom()))),
            other => panic!("unknown directive {other}"),
        };
        if let Err(err) = outcome {
            log.push_str(&format!("! error {err}\n"));
        }
    }
    log.push_str(&render(&sink.dom()));
    golden(&dir.join(format!("{name}.golden")), &log);
}

#[test]
fn transcripts_replay_to_their_goldens() {
    for name in ["streamed", "complete", "decisions", "interrupt", "jobs", "attach"] {
        run(name);
    }
}

fn parsed(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

#[test]
fn native_transcripts_round_trip() {
    use loa_core::{Draft, PatchSink, Value as V, attr};
    use loa_harness::claude::native;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/claude/native");
    let side = dir.join("11111111-2222-4333-8444-555555555555");
    let source = side.with_extension("jsonl");
    let sink = MemorySink::new();
    native::import(&source, &sink).unwrap();
    golden(&dir.join("../native.golden"), &render(&sink.dom()));
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("exported.jsonl");
    native::export(&sink.dom(), &out).unwrap();
    assert_eq!(parsed(&out), parsed(&source));
    for file in ["subagents/agent-sub-7.jsonl", "subagents/agent-sub-7.meta.json", "tool-results/blob.bin"] {
        assert_eq!(std::fs::read(tmp.path().join("exported").join(file)).unwrap(), std::fs::read(side.join(file)).unwrap(), "{file}");
    }
    // A sidechain transcript imports as its own session.
    let child = side.join("subagents/agent-sub-7.jsonl");
    let child_sink = MemorySink::new();
    native::import(&child, &child_sink).unwrap();
    native::export(&child_sink.dom(), &tmp.path().join("child.jsonl")).unwrap();
    assert_eq!(parsed(&tmp.path().join("child.jsonl")), parsed(&child));
    // Edited text exports into the native block, keeping its unknown fields.
    let message = sink.with_dom(|d| d.iter().find(|n| n.text == "Checking file").unwrap().id);
    sink.commit(Draft::append(message, " again")).unwrap();
    native::export(&sink.dom(), &out).unwrap();
    let edited = parsed(&out);
    assert_eq!(edited[2]["message"]["content"][1], serde_json::json!({"type": "text", "text": "Checking file again", "citations": [{"start": 0}]}));
    assert_eq!(edited[2]["message"]["usage"]["unknown_usage"], 17);
    // Messages the app wrote export as fresh, chained lines.
    let fresh = MemorySink::new();
    let w = loa_core::DomWriter::new(&fresh);
    let attrs = [(attr::ID, V::from("s")), (attr::HARNESS, "claude-code".into()), (attr::NODE, "n".into()), (attr::CREATED, V::Int(0)), (attr::SCHEMA, V::Int(1))];
    let root = w.insert(loa_core::NodeId::DOCUMENT, ElementKind::Session, attrs, "").unwrap();
    let turn = w.insert(root, ElementKind::Turn, [(attr::STATUS, V::from("done")), (attr::STARTED, V::Int(1000))], "").unwrap();
    w.insert(turn, ElementKind::Message, [(attr::ROLE, "user"), (attr::STATUS, "done")], "Start here").unwrap();
    w.insert(turn, ElementKind::Message, [(attr::ROLE, "assistant"), (attr::STATUS, "done")], "Here is the result").unwrap();
    native::export(&fresh.dom(), &out).unwrap();
    let lines = parsed(&out);
    assert_eq!((lines.len(), &lines[0]["parentUuid"], &lines[1]["parentUuid"]), (2, &serde_json::Value::Null, &lines[0]["uuid"]));
    assert_eq!(lines[0]["message"]["content"], "Start here");
    assert_eq!(lines[1]["message"]["content"][0]["text"], "Here is the result");
}
