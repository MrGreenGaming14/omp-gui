//! omp2 transcript replays. Fixture lines are omp2 stdout lines, or
//! `{"$":"send","text"}` (the user's prompt) and `{"$":"answer","values"}`
//! (answers the newest waiting decision).

use loa_contracts::harness::{Answer, Prompt};
use loa_core::{DomWriter, Draft, ElementKind, MemorySink, NodeId, PatchSink, attr};
use loa_harness::kit::EventMapper;
use loa_harness::omp2::Omp2Mapper;
use loa_harness::testing::{golden, render};
use serde_json::Value as Json;
use std::path::Path;

#[test]
fn transcripts_replay_to_their_goldens() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/omp2");
    let mut fixtures: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect();
    fixtures.sort();
    assert!(!fixtures.is_empty());
    for fixture in fixtures {
        let sink = MemorySink::new();
        let attrs = [(attr::ID, "s".into()), (attr::HARNESS, "omp2".into()), (attr::NODE, "owner".into()), (attr::CREATED, 1_i64.into())];
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "")).unwrap().node;
        let mut mapper = Omp2Mapper::new(root);
        let w = DomWriter::new(&sink).at(0);
        let mut out = Vec::new();
        let text = std::fs::read_to_string(&fixture).unwrap();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let raw: Json = serde_json::from_str(line).unwrap();
            let text = |key: &str| raw[key].as_str().unwrap_or_default().to_owned();
            match raw["$"].as_str() {
                Some("send") => drop(mapper.begin(&w, &Prompt { text: text("text"), by: "owner".into(), ..Prompt::default() }).unwrap()),
                Some("answer") => {
                    let waiting = |n: &&loa_core::Node| n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some("waiting");
                    let decision = sink.with_dom(|dom| dom.iter().filter(waiting).map(|n| n.id).max()).unwrap();
                    let values = raw["values"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect();
                    out.extend(mapper.answer(decision, &Answer { values, reason: None, by: "owner".into() }, &w).unwrap());
                }
                _ => out.extend(mapper.apply(serde_json::from_value(raw).unwrap_or_else(|e| panic!("{line}: {e}")), &w).unwrap_or_else(|e| panic!("{line}: {e}"))),
            }
        }
        let replies: String = out.iter().map(|reply| format!("{}\n", serde_json::to_string(reply).unwrap())).collect();
        golden(&fixture.with_extension("golden"), &format!("{}\n--- replies\n{replies}", render(&sink.dom())));
    }
}

#[test]
fn tool_approval_options_reflect_advertised_scopes() {
    use loa_core::answer;
    use loa_harness::omp2::wire::{Input, Reply};
    use serde_json::json;

    let sink = MemorySink::new();
    let attrs = [(attr::ID, "s".into()), (attr::HARNESS, "omp2".into()), (attr::NODE, "owner".into()), (attr::CREATED, 1_i64.into())];
    let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "")).unwrap().node;
    let mut mapper = Omp2Mapper::new(root);
    let w = DomWriter::new(&sink).at(0);
    mapper.begin(&w, &Prompt { text: "run".into(), by: "owner".into(), ..Default::default() }).unwrap();

    for frame in [
        json!({"type": "tool_approval_request", "promptId": "p1", "title": "Run bash?", "scopes": ["once", "session"]}),
        json!({"type": "tool_approval_request", "promptId": "p2", "title": "Run rm?", "scopes": ["once"]}),
        json!({"type": "tool_approval_request", "promptId": "p3", "title": "Run grep?"}),
    ] {
        mapper.apply(serde_json::from_value::<Input>(frame).unwrap(), &w).unwrap();
    }

    let decisions: Vec<(NodeId, Vec<String>)> = sink.with_dom(|dom| {
        dom.iter()
            .filter(|n| n.kind == ElementKind::Decision)
            .map(|n| (n.id, n.attr(attr::OPTIONS).unwrap().str_items().map(str::to_owned).collect()))
            .collect()
    });
    assert_eq!(decisions[0].1, [answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY]);
    assert_eq!(decisions[1].1, [answer::ALLOW_ONCE, answer::DENY]);
    assert_eq!(decisions[2].1, [answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY]);

    let session_reply = mapper.answer(decisions[0].0, &Answer { values: vec![answer::ALLOW_SESSION.into()], reason: None, by: "owner".into() }, &w).unwrap();
    assert_eq!(session_reply, Some(Reply::Approve { prompt_id: "p1".into(), approved: true, scope: "session", reason: None }));

    let fallback_reply = mapper.answer(decisions[1].0, &Answer { values: vec![answer::ALLOW_SESSION.into()], reason: None, by: "owner".into() }, &w).unwrap();
    assert_eq!(fallback_reply, Some(Reply::Approve { prompt_id: "p2".into(), approved: true, scope: "once", reason: None }));
}
