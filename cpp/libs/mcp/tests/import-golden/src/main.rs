// Writes `mcp/import.expected.json`: what the Rust import readers read from the fixtures under `mcp/`.
// Run: cargo run -- <golden dir>/mcp > <golden dir>/mcp/import.expected.json
use loa_mcp::import::{self, App, CodexToml, ImportSource, JsonServers};
use std::path::PathBuf;
fn main() {
    let g = PathBuf::from(std::env::args().nth(1).unwrap());
    let files: Vec<(&str, Box<dyn ImportSource>)> = vec![
        ("home/.claude.json", Box::new(JsonServers { app: App::Claude, path: g.join("home/.claude.json"), project: Some("/work/app".into()) })),
        ("project/.mcp.json", Box::new(JsonServers { app: App::Claude, path: g.join("project/.mcp.json"), project: None })),
        ("home/.cursor/mcp.json", Box::new(JsonServers { app: App::Cursor, path: g.join("home/.cursor/mcp.json"), project: None })),
        ("project/.cursor/mcp.json", Box::new(JsonServers { app: App::Cursor, path: g.join("project/.cursor/mcp.json"), project: None })),
        ("home/.codex/config.toml", Box::new(CodexToml(g.join("home/.codex/config.toml")))),
    ];
    let mut out = Vec::new();
    for (rel, file) in &files {
        let found = file.read().unwrap();
        let entries: Vec<_> = found.into_iter().map(|f| match f.spec {
            Ok(spec) => serde_json::json!({"name": f.name, "spec": spec}),
            Err(e) => serde_json::json!({"name": f.name, "error": e}),
        }).collect();
        out.push(serde_json::json!({"file": rel, "app": format!("{:?}", file.app()), "entries": entries}));
    }
    let existing = vec![];
    let all: Vec<_> = files.iter().flat_map(|(_, f)| f.read().unwrap()).collect();
    let report = import::plan_found(&existing, all);
    let plan: Vec<_> = report.entries.iter().map(|e| match &e.outcome {
        import::Outcome::Imported(_) => serde_json::json!({"name": e.name, "outcome": "imported"}),
        import::Outcome::Duplicate { existing } => serde_json::json!({"name": e.name, "outcome": "duplicate", "existing": existing}),
        import::Outcome::Unsupported { .. } => serde_json::json!({"name": e.name, "outcome": "unsupported"}),
    }).collect();
    let doc = serde_json::json!({"sources": out, "plan": plan});
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}
