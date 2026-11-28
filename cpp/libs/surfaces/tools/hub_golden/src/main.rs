//! Writes the `surface/hub/*` golden fixtures: the `surface_*` tool specs of every
//! `tool_set` scope and the `<surface>` session elements. Run with the output directory.

use loa_contracts::surface::{SurfaceInfo, SurfaceMode, SurfaceUri};
use loa_core::{Draft, ElementKind, MemorySink, NodeId, PatchSink};
use loa_surfaces::hub::dom;
use loa_surfaces::{HubConfig, Origin, SurfaceHub, tool_set};
use prost::Message;
use serde_json::json;
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("output directory"));
    std::fs::create_dir_all(&out).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let mut config = HubConfig::discover(dir.path());
    config.browser = None;
    let hub = SurfaceHub::new(config);
    let mut scopes = serde_json::Map::new();
    for (key, scope) in [("all", None), ("browser", Some("browser")), ("docs", Some("docs")), ("vm", Some("vm"))] {
        let specs: Vec<_> = tool_set(&hub, scope)
            .specs()
            .into_iter()
            .map(|s| json!({ "name": s.name, "description": s.description, "input_schema": s.input_schema }))
            .collect();
        scopes.insert(key.to_owned(), json!(specs));
    }
    std::fs::write(out.join("tools.json"), serde_json::to_string_pretty(&scopes).unwrap() + "\n").unwrap();

    let sink = MemorySink::new();
    let attrs = [
        ("id", loa_core::Value::from("s1")),
        ("harness", loa_core::Value::from("omp")),
        ("node", loa_core::Value::from("key-laptop")),
        ("created", loa_core::Value::Int(100)),
    ];
    let root = Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "");
    let session = sink.commit_at(root, 1).unwrap().node;
    let tab = SurfaceInfo {
        uri: SurfaceUri::new("browser", "tab1").unwrap(),
        name: "Example".into(),
        width: 1280,
        height: 720,
        mode: SurfaceMode::Pixel,
        mime: None,
    };
    let doc = SurfaceInfo {
        uri: SurfaceUri::new("files", "report").unwrap(),
        name: "report.pdf".into(),
        width: 0,
        height: 0,
        mode: SurfaceMode::Document,
        mime: Some("application/pdf".into()),
    };
    let first = sink.commit_at(dom::opened(session, &tab, &Origin::Browser), 2).unwrap().node;
    sink.commit_at(dom::opened(session, &doc, &Origin::Mcp("files-server".into())), 3).unwrap();
    sink.commit_at(dom::closed(first), 4).unwrap();
    let mut bytes = Vec::new();
    for patch in sink.patches() {
        loa_bus::proto::Patch::from(&patch).encode_length_delimited(&mut bytes).unwrap();
    }
    std::fs::write(out.join("dom-patches.bin"), bytes).unwrap();
    std::fs::write(out.join("dom.xml"), loa_core::xml::to_string(&sink.dom())).unwrap();
}
