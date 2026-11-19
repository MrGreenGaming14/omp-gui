//! End to end: an MCP client spawns the `loa-omp-bridge` binary, which runs the
//! installed omp against its loopback provider. No model is contacted; omp's
//! HOME and agent directory are temp directories.

use std::process::Stdio;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use serde_json::{Map, Value, json};
use tokio::process::Command;

fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `[path#TAG]` header of a hashline read.
fn header(read: &str) -> &str {
    let end = read.find(']').expect("hashline header") + 1;
    &read[..end]
}

#[tokio::test(flavor = "multi_thread")]
async fn harness_reads_edits_and_searches_through_omp() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("notes.txt"), "alpha\nbeta\ngamma\n").unwrap();
    std::fs::write(work.join("lib.rs"), "fn total() -> u32 {\n    let sum = 1 + 2;\n    sum\n}\n").unwrap();

    let mut bridge = Command::new(env!("CARGO_BIN_EXE_loa-omp-bridge"));
    bridge
        .arg("--state-dir")
        .arg(temp.path().join("state"))
        .arg("--cwd")
        .arg(&work)
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut child = bridge.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stdin = child.stdin.take().unwrap();
    let client = ().serve((stdout, stdin)).await.unwrap();

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(names, ["read", "edit", "ast_grep", "ast_edit", "lsp", "debug"]);
    let edit_schema = &tools[1].input_schema;
    assert!(
        edit_schema["properties"].get("input").is_some(),
        "edit schema comes from omp: {edit_schema:?}"
    );

    let read = client
        .call_tool(CallToolRequestParams::new("read").with_arguments(args(json!({"path": "notes.txt"}))))
        .await
        .unwrap();
    assert_eq!(read.is_error, Some(false), "{}", text(&read));
    let read_text = text(&read);
    assert!(read_text.contains("2:beta"), "{read_text}");
    let stale_header = header(&read_text).to_owned();

    let patch = format!("{stale_header}\nPUT 2.=2:\n+BETA\n");
    let edit = client
        .call_tool(CallToolRequestParams::new("edit").with_arguments(args(json!({"input": patch}))))
        .await
        .unwrap();
    assert_eq!(edit.is_error, Some(false), "{}", text(&edit));
    assert_eq!(std::fs::read_to_string(work.join("notes.txt")).unwrap(), "alpha\nBETA\ngamma\n");

    let replay = format!("{stale_header}\nPUT 1.=1:\n+ALPHA\n");
    let rejected = client
        .call_tool(CallToolRequestParams::new("edit").with_arguments(args(json!({"input": replay}))))
        .await
        .unwrap();
    assert_eq!(rejected.is_error, Some(true), "a stale tag must be refused: {}", text(&rejected));
    assert_eq!(std::fs::read_to_string(work.join("notes.txt")).unwrap(), "alpha\nBETA\ngamma\n");

    let search = client
        .call_tool(
            CallToolRequestParams::new("ast_grep")
                .with_arguments(args(json!({"pat": "let $NAME = $VALUE", "path": "lib.rs"}))),
        )
        .await
        .unwrap();
    assert_eq!(search.is_error, Some(false), "{}", text(&search));
    assert!(text(&search).contains("let sum = 1 + 2;"), "{}", text(&search));

    client.cancel().await.unwrap();
    child.wait().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_call_leaves_the_next_call_correct() {
    use loa_omp_bridge::{Error, OmpBridgeConfig, OmpExecutor, OutputBlock, ToolExecutor};
    use tokio_util::sync::CancellationToken;

    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("one.txt"), "first file\n").unwrap();
    std::fs::write(work.join("two.txt"), "second file\n").unwrap();

    let mut config = OmpBridgeConfig::new(&work, temp.path().join("state"));
    config.tools = vec!["read".into()];
    config.env.push(("HOME".into(), home.into_os_string()));
    let executor = OmpExecutor::start(config).await.unwrap();

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let first = executor
        .call("read", args(json!({"path": "one.txt"})), cancelled)
        .await;
    assert!(matches!(first, Err(Error::Cancelled)), "{first:?}");

    let second = executor
        .call("read", args(json!({"path": "two.txt"})), CancellationToken::new())
        .await
        .unwrap();
    let [OutputBlock::Text(text)] = second.content.as_slice() else {
        panic!("unexpected output {second:?}");
    };
    assert!(text.contains("second file"), "the next call must get its own result: {text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn omp_reports_its_default_tools_for_the_tools_panel() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = loa_omp_bridge::OmpBridgeConfig::new(temp.path(), temp.path().join("state"));
    config.env.push(("HOME".into(), temp.path().into()));
    let tools = loa_omp_bridge::list_omp_tools(&config).await.unwrap();
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();
    for expected in ["bash", "read", "edit"] {
        assert!(names.contains(&expected), "{expected} missing from {names:?}");
    }
    assert!(tools.iter().all(|tool| !tool.description.is_empty()), "every tool is described");
}
