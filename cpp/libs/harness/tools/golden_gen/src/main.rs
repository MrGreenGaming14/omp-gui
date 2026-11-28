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
}
