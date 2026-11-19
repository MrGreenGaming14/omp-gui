use loa_updater::gates::{
    ChangelogGate, ChangelogReview, ChangelogSummarizer, ConfigSchema, ContractFailure,
    ContractRunner, FormatGate, FormatProbe, GateCheck, GateInputs, OmpReadyProbe, RpcGate,
    RpcProbe, ToolCatalog, ToolGate, ToolSchema, UsedTools, Verdict, check_tool_schemas,
    run_gates,
};
use semver::Version;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn tool(name: &str, schema: serde_json::Value) -> ToolSchema {
    ToolSchema { name: name.to_string(), input_schema: schema }
}

fn old_tools() -> Vec<ToolSchema> {
    vec![
        tool(
            "edit",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "file"},
                    "content": {"type": "string"},
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {"line": {"type": "integer"}},
                            "required": ["line"]
                        }
                    }
                },
                "required": ["path"]
            }),
        ),
        tool("notebook", json!({"type": "object", "properties": {"cell": {"type": "integer"}}})),
    ]
}

fn used() -> UsedTools {
    [("edit".to_string(), ["path", "content", "edits.line"].map(String::from).into())].into()
}

fn edit_schema_with(f: impl FnOnce(&mut serde_json::Value)) -> Vec<ToolSchema> {
    let mut tools = old_tools();
    f(&mut tools[0].input_schema);
    tools
}

#[test]
fn removed_used_tool_is_harmful() {
    let new = vec![old_tools().remove(1)];
    let r = check_tool_schemas(&old_tools(), &new, &used());
    assert_eq!(r.verdict, Verdict::Harmful);
    assert!(r.details.contains(&"- edit: tool removed".to_string()), "{:?}", r.details);
}

#[test]
fn removed_unused_tool_passes() {
    let new = vec![old_tools().remove(0)];
    let r = check_tool_schemas(&old_tools(), &new, &used());
    assert_eq!(r.verdict, Verdict::Pass, "{:?}", r.details);
}

#[test]
fn used_tool_missing_from_both_versions_passes() {
    let tools = vec![old_tools().remove(1)];
    let r = check_tool_schemas(&tools, &tools, &used());
    assert_eq!(r.verdict, Verdict::Pass, "{:?}", r.details);
    assert!(r.details.contains(&"~ edit: unavailable in both versions".to_string()), "{:?}", r.details);
}

#[test]
fn renamed_field_is_reported_as_rename() {
    let new = edit_schema_with(|s| {
        let props = s["properties"].as_object_mut().unwrap();
        let path = props.remove("path").unwrap();
        props.insert("file_path".into(), path);
        s["required"] = json!(["file_path"]);
    });
    let r = check_tool_schemas(&old_tools(), &new, &used());
    assert_eq!(r.verdict, Verdict::Harmful);
    assert!(
        r.details.contains(&"- edit.path: field removed, likely renamed path -> file_path".to_string()),
        "{:?}",
        r.details
    );
    assert!(!r.details.iter().any(|l| l.starts_with("+ edit.file_path")), "{:?}", r.details);
}

#[test]
fn removed_field_without_match_is_plain_removal() {
    let new = edit_schema_with(|s| {
        s["properties"].as_object_mut().unwrap().remove("content");
    });
    let r = check_tool_schemas(&old_tools(), &new, &used());
    assert_eq!(r.verdict, Verdict::Harmful);
    assert_eq!(r.details, vec!["- edit.content: field removed"]);
}

#[test]
fn newly_required_field_is_harmful() {
    let new = edit_schema_with(|s| {
        s["properties"]["mode"] = json!({"type": "string"});
        s["required"] = json!(["path", "mode"]);
    });
    let r = check_tool_schemas(&old_tools(), &new, &used());
    assert_eq!(r.verdict, Verdict::Harmful);
    assert_eq!(r.details, vec!["~ edit.mode: now required"]);
}

#[test]
fn type_change_is_harmful() {
    let new = edit_schema_with(|s| {
        s["properties"]["content"] = json!({"type": "array"});
    });
    let r = check_tool_schemas(&old_tools(), &new, &used());
    assert_eq!(r.verdict, Verdict::Harmful);
    assert_eq!(r.details, vec![r#"~ edit.content: type "string" -> "array""#]);
}

#[test]
fn nested_field_changes_are_checked() {
    let new = edit_schema_with(|s| {
        let item = &mut s["properties"]["edits"]["items"];
        item["properties"]["line"] = json!({"type": "string"});
        item["properties"]["end"] = json!({"type": "integer"});
        item["required"] = json!(["line", "end"]);
    });
    let r = check_tool_schemas(&old_tools(), &new, &used());
    assert_eq!(r.verdict, Verdict::Harmful);
    assert!(r.details.contains(&r#"~ edit.edits.line: type "integer" -> "string""#.to_string()));
    assert!(r.details.contains(&"~ edit.edits.end: now required".to_string()), "{:?}", r.details);
}

#[test]
fn additions_pass_with_plus_lines() {
    let mut new = edit_schema_with(|s| {
        s["properties"]["dry_run"] = json!({"type": "boolean"});
    });
    new.push(tool("search", json!({"type": "object", "properties": {}})));
    let r = check_tool_schemas(&old_tools(), &new, &used());
    assert_eq!(r.verdict, Verdict::Pass);
    assert!(r.details.contains(&"+ edit.dry_run: field added".to_string()), "{:?}", r.details);
    assert!(r.details.contains(&"+ search: tool added".to_string()), "{:?}", r.details);
}

#[test]
fn unused_tool_changes_never_harm() {
    let mut new = old_tools();
    new[1].input_schema = json!({
        "type": "object",
        "properties": {"cell": {"type": "string"}, "kernel": {"type": "string"}},
        "required": ["kernel"]
    });
    let r = check_tool_schemas(&old_tools(), &new, &used());
    assert_eq!(r.verdict, Verdict::Pass, "{:?}", r.details);
}

fn script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("omp");
    loa_exec::write_executable(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    path
}

#[tokio::test]
async fn ready_probe_reads_frame_and_kills_child() {
    let dir = tempfile::tempdir().unwrap();
    let exe = script(
        dir.path(),
        r#"printf '%s\n' '{"type":"ready","protocolVersion":1,"supportedProtocolVersions":[1,2],"maxFrameBytes":1048576}'
exec sleep 30"#,
    );
    let probe = OmpReadyProbe::new(Duration::from_secs(20));
    let started = Instant::now();
    let versions = probe.protocol_versions(&exe).await.unwrap();
    assert_eq!(versions, vec![1, 2]);
    assert!(started.elapsed() < Duration::from_secs(15), "probe waited for the child to exit");
}

#[tokio::test]
async fn ready_probe_rejects_garbage() {
    let dir = tempfile::tempdir().unwrap();
    let exe = script(dir.path(), "echo 'Welcome to omp!'\nexec sleep 30");
    let probe = OmpReadyProbe::new(Duration::from_secs(20));
    assert!(probe.protocol_versions(&exe).await.is_err());
}

#[tokio::test]
async fn ready_probe_passes_rpc_args() {
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args");
    let exe = script(
        dir.path(),
        &format!(
            "echo \"$@\" > '{}'\nprintf '%s\\n' '{{\"type\":\"ready\",\"protocolVersion\":1}}'\nexec sleep 30",
            args_file.display()
        ),
    );
    let probe = OmpReadyProbe::new(Duration::from_secs(20));
    assert_eq!(probe.protocol_versions(&exe).await.unwrap(), vec![1]);
    assert_eq!(std::fs::read_to_string(&args_file).unwrap().trim(), "--mode rpc --no-session");
}

struct FakeCatalog {
    fail: bool,
}

#[async_trait::async_trait]
impl ToolCatalog for FakeCatalog {
    async fn tools(&self, _: &Path) -> Result<Vec<ToolSchema>, String> {
        if self.fail { Err("rpc client crashed".to_string()) } else { Ok(old_tools()) }
    }
}

struct FakeFormats;

#[async_trait::async_trait]
impl FormatProbe for FakeFormats {
    async fn session_version(&self, executable: &Path) -> Result<u32, String> {
        Ok(if executable.ends_with("new") { 4 } else { 3 })
    }
    async fn config_keys(&self, _: &Path) -> Result<ConfigSchema, String> {
        Ok(ConfigSchema {
            keys: ["modelRoles.smol".to_string()].into(),
            open: Default::default(),
            renamed: Default::default(),
        })
    }
}

struct FakeRpc;

#[async_trait::async_trait]
impl RpcProbe for FakeRpc {
    async fn protocol_versions(&self, _: &Path) -> Result<Vec<u32>, String> {
        Ok(vec![1, 2])
    }
}

struct FakeContract;

#[async_trait::async_trait]
impl ContractRunner for FakeContract {
    async fn run(&self, _: &Path) -> Result<Vec<ContractFailure>, String> {
        Ok(Vec::new())
    }
}

struct FakeSummarizer;

#[async_trait::async_trait]
impl ChangelogSummarizer for FakeSummarizer {
    async fn review(&self, _: &str) -> Result<ChangelogReview, String> {
        Ok(ChangelogReview::default())
    }
}

async fn report(catalog: &FakeCatalog, rpc: bool) -> loa_updater::gates::GateReport {
    let used = used();
    let migrations: BTreeSet<(u32, u32)> = [(3, 4)].into();
    let written: BTreeSet<String> = ["modelRoles.smol".to_string()].into();
    let inputs = GateInputs {
        old_executable: Path::new("/versions/omp/old"),
        new_executable: Path::new("/versions/omp/new"),
        tools: Some(ToolGate { catalog, used: &used }),
        formats: Some(FormatGate {
            probe: &FakeFormats,
            migrations: &migrations,
            written_keys: &written,
        }),
        rpc: rpc.then_some(RpcGate { probe: &FakeRpc, supported: 1..=2 }),
        contract: Some(&FakeContract),
        changelog: Some(ChangelogGate { summarizer: &FakeSummarizer, text: "## 2.0.0\n\n- fix" }),
    };
    run_gates("omp", &Version::new(1, 0, 0), &Version::new(2, 0, 0), &inputs).await
}

#[tokio::test]
async fn all_clean_ports_give_clean_report() {
    let r = report(&FakeCatalog { fail: false }, true).await;
    assert!(r.is_clean(), "{r}");
    let checks: Vec<GateCheck> = r.checks.iter().map(|c| c.check).collect();
    assert_eq!(
        checks,
        vec![
            GateCheck::ToolSchemas,
            GateCheck::SessionFormat,
            GateCheck::ConfigSchema,
            GateCheck::RpcProtocol,
            GateCheck::ContractTests,
            GateCheck::Changelog,
        ]
    );
}

#[tokio::test]
async fn failing_port_marks_only_its_check() {
    let r = report(&FakeCatalog { fail: true }, false).await;
    assert!(!r.is_clean());
    let harmful: Vec<GateCheck> = r.harmful().map(|c| c.check).collect();
    assert_eq!(harmful, vec![GateCheck::ToolSchemas]);
    let tools = &r.checks[0];
    assert!(tools.details[0].starts_with("check could not run:"), "{:?}", tools.details);
    assert!(tools.details[0].contains("rpc client crashed"));
    assert_eq!(r.checks.len(), 5, "RPC check is skipped when not applicable");
    assert!(r.checks.iter().all(|c| c.check != GateCheck::RpcProtocol));
}
