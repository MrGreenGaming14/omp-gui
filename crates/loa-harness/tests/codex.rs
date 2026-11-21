//! Codex app-server transcripts replayed through the mapper against goldens, and the rollout round trip.
//! Lines starting `!` drive the session: `prompt <text>`, `accepted <turn>`, `rejected <error>`,
//! `answer <nth decision> <values,…> [| reason]`, `model`, `effort`, `mode`, `attach`, `exited <detail>`, `state`.

use loa_contracts::harness::{Answer, Prompt, SessionConfig};
use loa_core::{DomWriter, ElementKind, MemorySink, SessionMode};
use loa_harness::codex::mapper::{Mapper, Reply};
use loa_harness::codex::rollout;
use loa_harness::testing::{golden, render};
use serde_json::Value as Json;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex")
}

#[test]
fn transcripts_replay_to_their_goldens() {
    let text = std::fs::read_to_string(fixtures().join("turns.jsonl")).unwrap();
    let config = SessionConfig { session_id: "session-invented".into(), node: "local-node".into(), cwd: "/work/invented".into(), model: Some("gpt-invented".into()), ..SessionConfig::default() };
    let mut sink = MemorySink::new();
    let mut mapper = Mapper::start(&config, "thread-one", Some("gpt-invented"), &DomWriter::new(&sink)).unwrap();
    let mut log = String::new();
    for line in text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let w = &DomWriter::new(&sink);
        let outcome = match line.strip_prefix("! ") {
            None => {
                let msg: Json = serde_json::from_str(line).unwrap();
                mapper.handle(msg.get("id").cloned(), msg["method"].as_str().unwrap(), &msg["params"], w)
            }
            Some(command) => {
                let (verb, rest) = command.split_once(' ').unwrap_or((command, ""));
                let done = match verb {
                    "prompt" => mapper.begin_turn(&Prompt { text: rest.into(), by: "person".into(), ..Prompt::default() }, w),
                    "accepted" => mapper.turn_accepted(rest, w),
                    "rejected" => mapper.turn_rejected(rest, w),
                    "answer" => {
                        let (nth, rest) = rest.split_once(' ').unwrap();
                        let (values, reason) = rest.split_once(" | ").map_or((rest, None), |(v, r)| (v, Some(r.to_owned())));
                        let decision = sink.with_dom(|d| d.iter().filter(|n| n.kind == ElementKind::Decision).nth(nth.parse().unwrap()).unwrap().id);
                        let values = if values.starts_with('{') { vec![values.to_owned()] } else { values.split(',').map(str::to_owned).collect() };
                        match mapper.answer(decision, &Answer { values, reason, by: "person".into() }, w) {
                            Ok(reply) => { if let Some(reply) = reply { log_reply(&mut log, reply); } Ok(()) },
                            Err(err) => Err(err),
                        }
                    }
                    "model" => mapper.set_model(rest, w),
                    "effort" => mapper.set_effort(rest, w),
                    "mode" => mapper.set_mode(SessionMode::parse(rest).unwrap(), w),
                    "exited" => mapper.process_exited(rest, w),
                    "attach" => {
                        let dom = sink.dom();
                        sink = MemorySink::from_dom(dom.clone());
                        Mapper::attach(&dom, &DomWriter::new(&sink)).map(|m| mapper = m)
                    }
                    "state" => { log.push_str(&format!("= {:?}\n", mapper.state())); Ok(()) },
                    other => panic!("unknown directive {other}"),
                };
                done.map(|()| None)
            }
        };
        match outcome {
            Ok(reply) => reply.into_iter().for_each(|r| log_reply(&mut log, r)),
            Err(err) => log.push_str(&format!("! error {err}\n")),
        }
    }
    log.push_str(&render(&sink.dom()));
    golden(&fixtures().join("turns.golden"), &log);
}

fn log_reply(log: &mut String, reply: Reply) {
    log.push_str(&match reply {
        Reply::Result { id, result } => format!("> {id} {result}\n"),
        Reply::Error { id, code, message } => format!("> {id} error {code} {message}\n"),
        Reply::HostTool { id, tool, arguments } => format!("> {id} host {tool} {arguments}\n"),
    });
}

#[test]
fn rollouts_round_trip() {
    let fixture = std::fs::read_to_string(fixtures().join("rollout-basic.jsonl")).unwrap();
    let sink = MemorySink::new();
    rollout::import(std::io::Cursor::new(&fixture), &sink, "local-node").unwrap();
    let dom = sink.dom();
    assert_eq!(dom.root().and_then(|n| n.str_attr(loa_core::attr::NATIVE_ID)), Some("0000aaaa-1111-7222-8333-444455556666"));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Compaction && n.text.contains("three")));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Call && n.str_attr(loa_core::attr::TOOL) == Some("local_shell")));
    assert!(dom.iter().any(|n| n.kind == ElementKind::Ext && n.text.contains("some_future_line")));
    golden(&fixtures().join("rollout-basic.golden"), &render(&sink.dom()));
    let mut exported = Vec::new();
    rollout::export(&sink.dom(), &mut exported).unwrap();
    let parse = |text: &str| text.lines().map(|l| serde_json::from_str(l).unwrap()).collect::<Vec<Json>>();
    assert_eq!(parse(std::str::from_utf8(&exported).unwrap()), parse(&fixture));
}
