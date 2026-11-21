//! Codex app-server transcripts replayed through the mapper against goldens,
//! the rollout codec round trip, and the driver against a fake process.
//! Lines starting `!` drive the session: `prompt <text>`, `accepted <turn>`,
//! `rejected <error>`, `answer <nth decision> <values,…> [| reason]`,
//! `model`, `effort`, `mode`, `attach`, `exited <detail>`, `state`.

use loa_contracts::harness::{Answer, Prompt, SessionConfig};
use loa_core::{ElementKind, MemorySink, SessionMode};
use loa_harness::codex::mapper::{Mapper, Reply};
use loa_harness::codex::rollout;
use loa_harness::testing::{golden, render};
use serde_json::Value as Json;
use std::path::Path;
use std::sync::Arc;

fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex")
}

#[test]
fn transcripts_replay_to_their_goldens() {
    let text = std::fs::read_to_string(fixtures().join("turns.jsonl")).unwrap();
    let config = SessionConfig { session_id: "session-invented".into(), node: "local-node".into(), cwd: "/work/invented".into(), model: Some("gpt-invented".into()), ..SessionConfig::default() };
    let mut sink = Arc::new(MemorySink::new());
    let mut mapper = Mapper::start(&config, "thread-one", Some("gpt-invented"), sink.clone()).unwrap();
    let mut log = String::new();
    let mut reply = |log: &mut String, reply: Option<Reply>| match reply {
        Some(Reply::Result { id, result }) => log.push_str(&format!("> {id} {result}\n")),
        Some(Reply::Error { id, code, message }) => log.push_str(&format!("> {id} error {code} {message}\n")),
        Some(Reply::HostTool { id, tool, arguments }) => log.push_str(&format!("> {id} host {tool} {arguments}\n")),
        None => {}
    };
    for line in text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let outcome = match line.strip_prefix("! ") {
            None => {
                let msg: Json = serde_json::from_str(line).unwrap();
                let (method, params) = (msg["method"].as_str().unwrap(), &msg["params"]);
                match msg.get("id") {
                    Some(id) => mapper.request(id.clone(), method, params).map(|r| reply(&mut log, r)),
                    None => mapper.notification(method, params),
                }
            }
            Some(command) => {
                let (verb, rest) = command.split_once(' ').unwrap_or((command, ""));
                match verb {
                    "prompt" => mapper.begin_turn(&Prompt { text: rest.into(), by: "person".into(), ..Prompt::default() }),
                    "accepted" => mapper.turn_accepted(rest),
                    "rejected" => mapper.turn_rejected(rest),
                    "answer" => {
                        let (nth, rest) = rest.split_once(' ').unwrap();
                        let (values, reason) = rest.split_once(" | ").map_or((rest, None), |(v, r)| (v, Some(r.to_owned())));
                        let decision = sink.with_dom(|d| d.iter().filter(|n| n.kind == ElementKind::Decision).nth(nth.parse().unwrap()).unwrap().id);
                        let values = if values.starts_with('{') { vec![values.to_owned()] } else { values.split(',').map(str::to_owned).collect() };
                        mapper.answer(decision, &Answer { values, reason, by: "person".into() }).map(|r| reply(&mut log, r))
                    }
                    "model" => mapper.set_model(rest),
                    "effort" => mapper.set_effort(rest),
                    "mode" => mapper.set_mode(SessionMode::parse(rest).unwrap()),
                    "exited" => mapper.process_exited(rest),
                    "attach" => {
                        let dom = sink.dom();
                        sink = Arc::new(MemorySink::from_dom(dom.clone()));
                        Mapper::attach(&dom, sink.clone()).map(|m| mapper = m)
                    }
                    "state" => Ok(log.push_str(&format!("= {:?}\n", mapper.state()))),
                    other => panic!("unknown directive {other}"),
                }
            }
        };
        if let Err(err) = outcome {
            log.push_str(&format!("! error {err}\n"));
        }
    }
    log.push_str(&render(&sink.dom()));
    golden(&fixtures().join("turns.golden"), &log);
}

#[test]
fn rollouts_round_trip() {
    let path = fixtures().join("rollout-basic.jsonl");
    let fixture = std::fs::read_to_string(&path).unwrap();
    let sink = MemorySink::new();
    rollout::import(std::io::Cursor::new(&fixture), &sink, "local-node").unwrap();
    golden(&fixtures().join("rollout-basic.golden"), &render(&sink.dom()));
    let mut exported = Vec::new();
    rollout::export(&sink.dom(), &mut exported).unwrap();
    let parse = |text: &str| text.lines().map(|l| serde_json::from_str(l).unwrap()).collect::<Vec<Json>>();
    assert_eq!(parse(std::str::from_utf8(&exported).unwrap()), parse(&fixture));
}
