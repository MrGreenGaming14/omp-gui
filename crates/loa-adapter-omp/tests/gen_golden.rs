//! Temporary: renders the replay goldens of loa-harness/fixtures/omp with the old mapper.
use loa_adapter_omp::mapping::Mapping;
use loa_contracts::harness::{Answer, Prompt};
use loa_core::{Draft, ElementKind as K, MemorySink, NodeId, PatchSink, attr};
use loa_harness::omp::rpc::Frame;
use serde_json::Value;

fn commit(sink: &MemorySink, mapper: &mut Mapping, drafts: Vec<Draft>) {
    let mut pending = drafts;
    while !pending.is_empty() {
        let mut next = Vec::new();
        for draft in pending {
            let patch = sink.commit(draft).unwrap();
            next.extend(mapper.committed(&[patch]));
        }
        pending = next;
    }
}

#[test]
fn render_goldens() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../loa-harness/fixtures/omp");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
        let sink = MemorySink::new();
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, K::Session, vec![(attr::ID, "s".into()), (attr::HARNESS, "omp".into()), (attr::NODE, "owner".into()), (attr::CREATED, 1_i64.into()), (attr::SCHEMA, 1_i64.into())], "")).unwrap().node;
        let mut mapper = Mapping::new(root);
        let mut replies = Vec::new();
        for line in std::fs::read_to_string(&path).unwrap().lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let json: Value = serde_json::from_str(line).unwrap();
            match json["$"].as_str() {
                Some("send") => mapper.queue(json["id"].as_str().unwrap(), &Prompt { text: json["text"].as_str().unwrap().into(), by: "owner".into(), ..Prompt::default() }),
                Some("answer") => {
                    let decision = *mapper.decisions.get(json["request"].as_str().unwrap()).unwrap();
                    let answer = Answer { values: serde_json::from_value(json["values"].clone()).unwrap(), by: "owner".into(), reason: json["reason"].as_str().map(str::to_owned) };
                    let reply = mapper.reply(decision, &answer).unwrap();
                    replies.push(format!("ui {:?}", reply.response));
                    if let Some(steer) = reply.steer { replies.push(format!("steer {steer:?}")); }
                    let drafts = mapper.answered(decision, &answer);
                    commit(&sink, &mut mapper, drafts);
                }
                _ => {
                    let drafts = match Frame::from_json(json) {
                        Frame::Event(event) => mapper.event(&event),
                        Frame::Ui(request) => match mapper.auto_reply(&request) {
                            Some(response) => { replies.push(format!("ui {response:?}")); Vec::new() }
                            None => mapper.ui(&request),
                        },
                    };
                    commit(&sink, &mut mapper, drafts);
                }
            }
        }
        let mut out = loa_core::xml::to_string(&sink.dom());
        out.push_str("\n--- replies\n");
        for reply in replies { out.push_str(&reply); out.push('\n'); }
        std::fs::write(path.with_extension("golden"), out).unwrap();
    }
}
