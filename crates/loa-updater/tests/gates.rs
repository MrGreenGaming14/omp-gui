//! The pure gate checks, table driven, and `run_gates` over fake ports.

use async_trait::async_trait;
use loa_settings::updates::{GateCheck, Verdict};
use loa_updater::gates::*;
use semver::Version;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;

fn tools(edit: Value) -> Vec<ToolSchema> {
    let notebook = json!({"type": "object", "properties": {"cell": {"type": "integer"}}});
    vec![ToolSchema { name: "edit".into(), input_schema: edit }, ToolSchema { name: "notebook".into(), input_schema: notebook }]
}

fn old_edit() -> Value {
    let item = json!({"type": "object", "properties": {"line": {"type": "integer"}}, "required": ["line"]});
    let properties = json!({"path": {"type": "string"}, "content": {"type": "string"}, "edits": {"type": "array", "items": item}});
    json!({"type": "object", "properties": properties, "required": ["path"]})
}

fn used() -> UsedTools {
    [("edit".to_owned(), ["path", "content", "edits.line"].map(String::from).into())].into()
}

#[test]
fn tool_schema_diff() {
    type Change = fn(&mut Vec<ToolSchema>);
    let cases: &[(&str, Change, Verdict, &[&str])] = &[
        ("unchanged", |_| {}, Verdict::Pass, &[]),
        ("used tool removed", |t| drop(t.remove(0)), Verdict::Harmful, &["- edit: tool removed"]),
        ("unused tool removed", |t| drop(t.remove(1)), Verdict::Pass, &["- notebook: tool removed (unused)"]),
        ("field removed", |t| drop(t[0].input_schema["properties"].as_object_mut().unwrap().remove("content")), Verdict::Harmful, &["- edit.content: field removed"]),
        ("field renamed", |t| {
            let props = t[0].input_schema["properties"].as_object_mut().unwrap();
            let path = props.remove("path").unwrap();
            props.insert("file_path".into(), path);
            t[0].input_schema["required"] = json!(["file_path"]);
        }, Verdict::Harmful, &["- edit.path: field removed, likely renamed path -> file_path", "~ edit.file_path: now required"]),
        ("type changed", |t| t[0].input_schema["properties"]["content"] = json!({"type": "array"}), Verdict::Harmful, &[r#"~ edit.content: type "string" -> "array""#]),
        ("newly required", |t| {
            t[0].input_schema["properties"]["mode"] = json!({"type": "string"});
            t[0].input_schema["required"] = json!(["path", "mode"]);
        }, Verdict::Harmful, &["~ edit.mode: now required"]),
        ("nested changes", |t| {
            let item = &mut t[0].input_schema["properties"]["edits"]["items"];
            item["properties"]["line"] = json!({"type": "string"});
            item["properties"]["end"] = json!({"type": "integer"});
            item["required"] = json!(["line", "end"]);
        }, Verdict::Harmful, &[r#"~ edit.edits.line: type "integer" -> "string""#, "~ edit.edits.end: now required"]),
        ("additions", |t| {
            t[0].input_schema["properties"]["dry_run"] = json!({"type": "boolean"});
            t.push(ToolSchema { name: "search".into(), input_schema: json!({"type": "object"}) });
        }, Verdict::Pass, &["+ edit.dry_run: field added", "+ search: tool added"]),
        ("unused tool changed", |t| t[1].input_schema = json!({"type": "object", "properties": {"cell": {"type": "string"}, "k": {}}, "required": ["k"]}),
            Verdict::Pass, &["+ notebook.k: field added (required)"]),
    ];
    for (name, change, verdict, details) in cases {
        let mut new = tools(old_edit());
        change(&mut new);
        let result = check_tool_schemas(&tools(old_edit()), &new, &used());
        assert_eq!((result.verdict, result.details.as_slice()), (*verdict, &details.iter().map(|d| d.to_string()).collect::<Vec<_>>()[..]), "{name}");
    }
    let only_notebook = vec![tools(old_edit()).remove(1)];
    assert_eq!(check_tool_schemas(&only_notebook, &only_notebook, &used()).details, ["~ edit: unavailable in both versions"]);
}

#[test]
fn format_config_rpc_contract_and_changelog_checks() {
    let migrations: BTreeSet<(u32, u32)> = [(1, 2), (2, 3)].into();
    assert_eq!(check_session_format(1, 3, &migrations).details, ["~ 1 -> 3: migrated via 1 -> 2 -> 3"]);
    assert_eq!(check_session_format(3, 1, &migrations).verdict, Verdict::Harmful);
    let schema = ConfigSchema { keys: ["a".into()].into(), open: ["roles".into()].into(), renamed: [("b".into(), "c".into())].into() };
    let written: BTreeSet<String> = ["a", "roles.smol", "b", "d"].map(String::from).into();
    assert_eq!(check_config_schema(&written, &schema).details, ["- b: renamed to c", "- d: removed"]);
    assert!(check_rpc_protocol(&[1, 2], 1..=3).details.contains(&"negotiated: 2".to_owned()));
    assert_eq!(check_rpc_protocol(&[3], 1..=2).verdict, Verdict::Harmful);
    let failure = ContractFailure { test: "basic: leaf".into(), diff: "- a\n+ b".into() };
    assert_eq!(check_contract(&[failure]).details, ["basic: leaf:", "  - a", "  + b"]);
    let review = ChangelogReview { breaking: vec![], removed: vec!["drop `grep`".into()] };
    assert_eq!((check_changelog(&review).details, check_changelog(&Default::default()).verdict), (vec!["- removed: drop `grep`".to_owned()], Verdict::Pass));
    let v = |s: &str| Version::parse(s).unwrap();
    assert_eq!(changelog_text(&[(v("1.10.0"), "ten"), (v("1.2.0"), "two\n")]), "## 1.2.0\n\ntwo\n\n## 1.10.0\n\nten\n");
}

#[test]
fn reviews_parse_strictly() {
    assert_eq!(parse_review("  {\"breaking\": [\"a\"], \"removed\": []}\n").unwrap().breaking, ["a"]);
    assert_eq!(parse_review("```json\n{\"breaking\": [], \"removed\": [\"b\"]}\n```").unwrap().removed, ["b"]);
    for junk in ["{\"breaking\": [], \"removed\": [], \"notes\": []}", "{\"breaking\": []}", "Here: {\"breaking\": [], \"removed\": []}", "[[], []]", "{\"breaking\": [1], \"removed\": []}"] {
        assert!(parse_review(junk).is_err(), "{junk}");
    }
}

struct Fake {
    tools_fail: bool,
}

#[async_trait]
impl ToolCatalog for Fake {
    async fn tools(&self, _: &Path) -> Result<Vec<ToolSchema>, String> {
        if self.tools_fail { Err("rpc client crashed".into()) } else { Ok(tools(old_edit())) }
    }
}

#[async_trait]
impl FormatProbe for Fake {
    async fn session_version(&self, exe: &Path) -> Result<u32, String> {
        Ok(if exe.ends_with("new") { 2 } else { 1 })
    }
    async fn config_keys(&self, _: &Path) -> Result<ConfigSchema, String> {
        Ok(ConfigSchema { keys: OmpGates::default().written_keys, ..Default::default() })
    }
}

#[async_trait]
impl RpcProbe for Fake {
    async fn protocol_versions(&self, _: &Path) -> Result<Vec<u32>, String> {
        Ok(vec![1, 2])
    }
}

#[async_trait]
impl ContractRunner for Fake {
    async fn run(&self, _: &Path) -> Result<Vec<ContractFailure>, String> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl ChangelogSummarizer for Fake {
    async fn review(&self, _: &str) -> Result<ChangelogReview, String> {
        Ok(ChangelogReview::default())
    }
}

#[tokio::test]
async fn run_gates_runs_every_check_and_a_failing_port_fails_only_its_own() {
    let gates = OmpGates { used_tools: used(), ..OmpGates::default() };
    for tools_fail in [false, true] {
        let fake = Fake { tools_fail };
        let inputs =
            GateInputs { old_executable: Path::new("old"), new_executable: Path::new("new"), omp: Some((&fake, &gates)), changelog: Some((&fake, "- fix")) };
        let report = run_gates("omp", &Version::new(1, 0, 0), &Version::new(2, 0, 0), &inputs).await;
        let checks: Vec<GateCheck> = report.checks.iter().map(|c| c.check).collect();
        use GateCheck::*;
        assert_eq!(checks, [ToolSchemas, SessionFormat, ConfigSchema, RpcProtocol, ContractTests, Changelog]);
        let harmful: Vec<GateCheck> = report.harmful().map(|c| c.check).collect();
        assert_eq!(harmful, if tools_fail { vec![ToolSchemas] } else { vec![] }, "{report}");
        assert!(!tools_fail || report.checks[0].details[0].contains("current version: rpc client crashed"));
    }
}
