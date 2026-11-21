//! opencode: event replays against goldens and the native round trip.

use loa_core::MemorySink;
use loa_harness::opencode::event::Event;
use loa_harness::opencode::mapper::{EventMapper, RootInfo};
use loa_harness::opencode::native;
use loa_harness::testing::golden;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

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
        let mut mapper = EventMapper::start(&sink, root, 0).unwrap();
        let text = std::fs::read_to_string(&fixture).unwrap();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            mapper.apply(&sink, Event::parse(line).unwrap(), 0).unwrap_or_else(|e| panic!("{line}: {e}"));
        }
        let xml = loa_core::xml::to_string(&sink.dom());
        let out: String = xml.lines().filter(|l| !l.starts_with("<!--")).map(|l| format!("{l}\n")).collect();
        golden(&fixture.with_extension("golden"), &out);
    }
}

#[test]
fn native_exports_reproduce_their_imports() {
    for fixture in fixtures("json") {
        let sink = MemorySink::new();
        native::import(&fixture, &sink).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.json");
        native::export(&sink.dom(), &out).unwrap();
        let read = |p: &Path| serde_json::from_str::<Value>(&std::fs::read_to_string(p).unwrap()).unwrap();
        assert_eq!(read(&out), read(&fixture), "{}", fixture.display());
    }
}
