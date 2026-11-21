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
