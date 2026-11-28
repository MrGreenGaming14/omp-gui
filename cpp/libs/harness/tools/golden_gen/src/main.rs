use loa_harness::kit::{Framing, JsonRpc2, RpcError, residual};
use loa_harness::native;
use loa_core::Value;
use serde_json::{json, Value as Json};
use std::fmt::Write as _;
use std::os::unix::process::ExitStatusExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn errors() -> Vec<RpcError> {
    vec![
        RpcError::NotFound("omp".into()),
        RpcError::Io(std::io::Error::other("broken pipe")),
        RpcError::Exited { status: Some(std::process::ExitStatus::from_raw(3 << 8)), stderr: "boom".into() },
        RpcError::Exited { status: Some(std::process::ExitStatus::from_raw(9)), stderr: String::new() },
        RpcError::Exited { status: None, stderr: String::new() },
        RpcError::Version { offered: vec![3, 4], min: 1, max: 2 },
        RpcError::Version { offered: vec![], min: 1, max: 1 },
        RpcError::Protocol("bad frame".into()),
        RpcError::Command { command: "fork".into(), message: "busy".into(), code: Some("session_busy".into()) },
        RpcError::StartTimeout(Duration::from_secs(20)),
        RpcError::StartTimeout(Duration::from_millis(1500)),
        RpcError::StartTimeout(Duration::from_millis(250)),
        RpcError::Closed,
    ]
}

fn json_set(root: &mut serde_json::Map<String, Json>, path: &[&str], value: Option<&str>) {
    let (key, parents) = path.split_last().unwrap();
    let mut map = root;
    for parent in parents {
        if value.is_none() && !map.contains_key(*parent) { return; }
        let item = map.entry((*parent).to_owned()).or_insert_with(|| Json::Object(serde_json::Map::new()));
        if value.is_none() && !item.is_object() { return; }
        map = item.as_object_mut().unwrap();
    }
    match value {
        Some(v) => { map.insert((*key).to_owned(), Json::String(v.to_owned())); }
        None => { map.shift_remove(*key); }
    }
}

fn toml_set(doc: &mut toml_edit::DocumentMut, path: &[&str], setting: Option<&str>) {
    use toml_edit::{Item, Table, value};
    let (key, parents) = path.split_last().unwrap();
    let mut table: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for parent in parents {
        if setting.is_none() && !table.contains_key(parent) { return; }
        if !table.contains_key(parent) { table.insert(parent, Item::Table(Table::new())); }
        let item = table.get_mut(parent).unwrap();
        if setting.is_none() && !item.is_table_like() { return; }
        table = item.as_table_like_mut().unwrap();
    }
    match setting {
        Some(text) => {
            if let Some(existing) = table.get_mut(key).and_then(Item::as_value_mut) {
                let decor = existing.decor().clone();
                *existing = toml_edit::Value::from(text);
                *existing.decor_mut() = decor;
            } else { table.insert(key, value(text)); }
        }
        None => { table.remove(key); }
    }
}

async fn scripted() -> (String, String) {
    use loa_contracts::harness::{Adapter, Answer, Prompt, SessionConfig};
    use loa_core::{ElementKind, PatchSink};
    use loa_harness::testing::{ScriptedAdapter, WatchSink, render};
    use std::sync::Arc;
    let adapter = ScriptedAdapter::new("scripted");
    let sink = Arc::new(WatchSink::default());
    let config = SessionConfig { session_id: "s1".into(), node: "n1".into(), cwd: "/work".into(), project: Some("p".into()), ..Default::default() };
    let session = adapter.start(config, sink.clone() as Arc<dyn PatchSink>).await.unwrap();
    let prompt = |text: &str| Prompt { text: text.into(), by: "n1".into(), ..Default::default() };
    let waiting = || sink.inner.with_dom(|d| d.iter().filter(|n| n.kind == ElementKind::Decision).last().unwrap().id);
    session.send(prompt("hello")).await.unwrap();
    session.send(prompt("ask question")).await.unwrap();
    session.answer(waiting(), Answer { values: vec!["red".into()], reason: Some("why".into()), by: "n1".into() }).await.unwrap();
    session.send(prompt("ask multi")).await.unwrap();
    session.answer(waiting(), Answer { values: vec!["rust".into(), "zig".into()], reason: None, by: "n2".into() }).await.unwrap();
    session.send(prompt("ask plan")).await.unwrap();
    session.answer(waiting(), Answer { values: vec!["approve".into()], reason: None, by: "n1".into() }).await.unwrap();
    session.send(prompt("ask form")).await.unwrap();
    session.answer(waiting(), Answer { values: vec![r#"{"name":"r1","channel":"beta"}"#.into()], reason: None, by: "n1".into() }).await.unwrap();
    session.send(prompt("ask")).await.unwrap();
    session.answer(waiting(), Answer { values: vec!["allow_once".into()], reason: None, by: "n1".into() }).await.unwrap();
    session.send(prompt("work bash Build it")).await.unwrap();
    session.interrupt().await.unwrap();
    session.send(prompt("tool x {}")).await.unwrap();
    let text = sink.inner.with_dom(render);
    let mut rules = String::new();
    for v in sink.inner.with_dom(loa_harness::testing::check_dom) {
        writeln!(rules, "{:?} {}", v.rule, v.node.map_or(0, |n| n.0)).unwrap();
    }
    for v in loa_harness::testing::check_history(&sink.inner.patches()) {
        writeln!(rules, "history {:?} {}", v.rule, v.node.map_or(0, |n| n.0)).unwrap();
    }
    (text, rules)
}

fn main() {
    let out = std::path::PathBuf::from(std::env::var("LOA_GOLDEN_OUT").unwrap()).join("harness");
    std::fs::create_dir_all(&out).unwrap();
    // RpcError display texts, one per line.
    let mut text = String::new();
    for e in errors() { writeln!(text, "{e}").unwrap(); }
    std::fs::write(out.join("rpc_errors.txt"), text).unwrap();
    // JSON-RPC frames.
    let mut lines = String::new();
    for header in [true, false] {
        let f = JsonRpc2 { header };
        let mut push = |v: Json| { lines.push_str(&serde_json::to_string(&v).unwrap()); lines.push('\n'); };
        push(f.request(&json!(1), "initialize", json!({"protocolVersion": 1, "b": [true, null, 2.5, "é\n"]})));
        push(f.request(&json!("s-1"), "session/prompt", Json::Null));
        push(f.notification("initialized", Json::Null));
        push(f.notification("session/cancel", json!({"sessionId": "x"})));
        push(f.response(json!(7), "fs/read", Ok(json!({"content": "hi"}))));
        push(f.response(json!("r"), "fs/read", Err(RpcError::Command { command: "request".into(), message: "nope".into(), code: Some("-32601".into()) })));
        push(f.response(json!(8), "fs/read", Err(RpcError::Command { command: "request".into(), message: "str code".into(), code: Some("\"x\"".into()) })));
        for e in errors() { push(f.response(json!(9), "m", Err(e))); }
    }
    std::fs::write(out.join("jsonrpc.jsonl"), lines).unwrap();
    // Residual.
    let (joined, lengths) = residual::join_parts(["first", "", "trois é", "4"], "\n\n");
    let lengths: Vec<i64> = lengths.iter().map(|v: &Value| v.as_int().unwrap()).collect();
    let res = json!({"attr": residual::attr_name("claude-code"), "joined": joined, "lengths": lengths});
    std::fs::write(out.join("residual.json"), serde_json::to_string(&res).unwrap()).unwrap();
    // Discovery order with fixed mtimes.
    let home = std::env::temp_dir().join(format!("loa-golden-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    unsafe { std::env::remove_var("CODEX_HOME") };
    let files: &[(&str, &str, u64)] = &[
        (".omp/agent/sessions/-Projects-a/2026_x.jsonl", "{\"type\":\"title\",\"title\":\"Sorter\"}\n", 1_700_000_003_000),
        (".omp/agent/sessions/-Projects-a/2026_y.jsonl", "{\"type\":\"session\"}\n", 1_700_000_001_000),
        (".omp/agent/sessions/-Projects-b/b.jsonl", "", 1_700_000_002_000),
        (".omp/agent/sessions/top.jsonl", "{}\n", 1_700_000_009_000),
        (".claude/projects/-home-a/u.jsonl", "{\"type\":\"user\",\"sessionId\":\"u\"}\n", 1_700_000_002_000),
        (".claude/projects/-home-a/v.json", "{}", 1_700_000_005_000),
        (".codex/sessions/2026/11/19/rollout-1.jsonl", "{\"type\":\"session_meta\"}\n", 1_700_000_004_500),
        (".codex/sessions/2026/11/rollout-2.jsonl", "{}\n", 1_700_000_008_000),
        (".codex/sessions/2026/11/19/other.txt", "x", 1_700_000_004_000),
    ];
    for (path, body, ms) in files {
        let p = home.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(UNIX_EPOCH + Duration::from_millis(*ms)).unwrap();
    }
    let _ = SystemTime::now();
    let mut list = String::new();
    for s in native::discover(&home) {
        let rel = s.path.strip_prefix(&home).unwrap().to_string_lossy().into_owned();
        writeln!(list, "{}\t{}\t{}\t{}\t{}\t{}", s.harness, rel, s.project.unwrap_or_default(), s.title.unwrap_or_default(), s.modified, s.size).unwrap();
    }
    std::fs::write(out.join("discover.tsv"), list).unwrap();
    std::fs::remove_dir_all(&home).unwrap();
    // Config documents.
    let src = std::path::PathBuf::from(std::env::var("LOA_GOLDEN_OUT").unwrap()).join("harness");
    let text = std::fs::read_to_string(src.join("configdoc_in.json")).unwrap();
    let Json::Object(mut root) = serde_json::from_str(&text).unwrap() else { panic!() };
    for (path, v) in [(&["nested", "a"][..], Some("new")), (&["nested", "b"][..], None), (&["nested", "c"][..], Some("last")), (&["new", "deep", "k"][..], Some("v")), (&["missing", "x"][..], None), (&["z", "x"][..], None), (&["s"][..], Some("é/<>\"\n\u{1}"))] {
        json_set(&mut root, path, v);
    }
    let mut text = serde_json::to_string_pretty(&root).unwrap();
    text.push('\n');
    std::fs::write(out.join("configdoc_out.json"), text).unwrap();
    let text = std::fs::read_to_string(src.join("configdoc_in.toml")).unwrap();
    let mut doc: toml_edit::DocumentMut = text.parse().unwrap();
    let steps: &[(&[&str], Option<&str>)] = &[
        (&["model"], Some("gpt-5")),
        (&["profiles", "fast", "model_reasoning_effort"], Some("high")),
        (&["other", "name"], Some("new")),
        (&["approval"], None),
        (&["dotted", "key"], Some("d2")),
        (&["features", "web", "mode"], Some("on")),
        (&["profiles", "fast", "quote"], Some("a\"b")),
        (&["top"], Some("t")),
        (&["other", "x"], None),
        (&["missing", "k"], None),
        (&["other", "esc"], Some("tab\there\u{7f}")),
    ];
    for (path, v) in steps {
        toml_set(&mut doc, path, *v);
        eprintln!("--- {path:?} {v:?}\n{doc}");
    }
    std::fs::write(out.join("configdoc_out.toml"), doc.to_string()).unwrap();
    // A scripted session through every prompt kind, rendered.
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let (text, rules) = rt.block_on(scripted());
    std::fs::write(out.join("scripted.txt"), text).unwrap();
    std::fs::write(out.join("scripted_violations.txt"), rules).unwrap();
}
