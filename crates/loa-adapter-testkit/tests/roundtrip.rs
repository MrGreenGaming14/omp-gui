//! Round trips, capability checks and the whole contract over the scripted
//! adapter's native format.

mod common;

use common::{Faulty, fixture, golden};
use loa_adapter::{Adapter, Capabilities, SessionConfig};
use loa_adapter_testkit::scenarios;
use loa_adapter_testkit::{
    Contract, Golden, LivePlan, Rule, ScriptedAdapter, check_capabilities, render_golden, round_trip, run_contract,
};
use loa_core::{ElementKind, attr};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn the_fixture_round_trips_and_matches_its_golden() {
    let adapter = ScriptedAdapter::default();
    let rt = round_trip(&adapter, &fixture("scripted/approval.jsonl")).unwrap();
    assert!(rt.is_clean(), "{rt}");
    assert!(rt.export_diffs.is_empty());
    // The unknown record survives as an <ext> and comes back on export.
    let ext = rt.imported.iter().find(|n| n.kind == ElementKind::Ext).unwrap();
    assert_eq!(ext.str_attr(attr::TYPE), Some("checkpoint"));
    if let Err(err) = Golden::from_env().check(&render_golden(&rt.imported), &golden("scripted_approval.golden")) {
        panic!("{err}");
    }
}

#[test]
fn an_export_that_drops_a_field_names_the_line_and_path() {
    fn drop_by_on_answers(text: String) -> String {
        text.lines()
            .map(|l| if l.contains("\"type\":\"answer\"") { l.replace(",\"by\":\"tai\"", "") } else { l.to_owned() })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }
    let mut adapter = Faulty::new(ScriptedAdapter::default());
    adapter.export_edit = Some(drop_by_on_answers);
    let rt = round_trip(&adapter, &fixture("scripted/approval.jsonl")).unwrap();
    let rules: Vec<Rule> = rt.violations.iter().map(|v| v.rule).collect();
    assert_eq!(rules, vec![Rule::Export, Rule::Reimport], "{rt}");
    let diffs = &rt.export_diffs[0].differences;
    assert_eq!(diffs.len(), 2, "{rt}");
    assert!(diffs[0].starts_with("line 6 $.by: missing from the export"), "{rt}");
    assert!(diffs[1].starts_with("line 13 $.by: missing from the export"), "{rt}");
    // The DOM diff points at the decision that lost its author.
    assert!(rt.reimport_diff.lines().any(|l| l.starts_with('-') && l.contains("by=\"tai\"")), "{rt}");
}

#[test]
fn an_export_that_loses_a_record_is_reported_as_missing() {
    fn drop_usage(text: String) -> String {
        text.lines().filter(|l| !l.contains("\"usage\"")).map(|l| format!("{l}\n")).collect()
    }
    let mut adapter = Faulty::new(ScriptedAdapter::default());
    adapter.export_edit = Some(drop_usage);
    let rt = round_trip(&adapter, &fixture("scripted/approval.jsonl")).unwrap();
    // Only the lost line is reported, not every line after it.
    assert_eq!(rt.export_diffs.len(), 1, "{rt}");
    let diffs = &rt.export_diffs[0].differences;
    assert_eq!(diffs.len(), 1, "{rt}");
    assert!(diffs[0].starts_with("line 9: missing from the export: {\"type\":\"usage\""), "{rt}");
}

#[test]
fn a_malformed_native_file_is_an_import_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.jsonl");
    std::fs::write(&path, "{\"type\":\"prompt\",\"ts\":1,\"text\":\"hi\"}\n").unwrap();
    let err = round_trip(&ScriptedAdapter::default(), &path).unwrap_err();
    assert!(err.to_string().contains("line 1"), "{err}");
}

#[test]
fn honest_capabilities_pass() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = ScriptedAdapter::default();
    let found = check_capabilities(&adapter, Some(&fixture("scripted/approval.jsonl")), dir.path());
    assert_eq!(found, vec![]);
}

#[test]
fn undeclared_import_and_export_that_work_are_caught() {
    let dir = tempfile::tempdir().unwrap();
    let mut adapter = Faulty::new(ScriptedAdapter::default());
    adapter.caps = Some(Capabilities { import: false, export: false, ..adapter.inner.capabilities() });
    let found = check_capabilities(&adapter, Some(&fixture("scripted/approval.jsonl")), dir.path());
    let details: Vec<&str> = found.iter().map(|v| v.detail.as_str()).collect();
    assert_eq!(found.len(), 4, "{details:?}");
    assert!(found.iter().all(|v| v.rule == Rule::Capability), "{details:?}");
    assert!(details.contains(&"import isn't declared but succeeded"), "{details:?}");
    assert!(details.contains(&"export isn't declared but succeeded"), "{details:?}");
}

#[test]
fn a_wrong_schema_version_is_caught() {
    let dir = tempfile::tempdir().unwrap();
    let mut adapter = Faulty::new(ScriptedAdapter::default());
    adapter.caps = Some(Capabilities { schema_version: 2, ..adapter.inner.capabilities() });
    let found = check_capabilities(&adapter, None, dir.path());
    assert_eq!(found.iter().map(|v| v.rule).collect::<Vec<_>>(), vec![Rule::SchemaVersion]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reference_adapter_passes_the_whole_contract() {
    let config = SessionConfig { session_id: "contract".to_owned(), node: "node-c".to_owned(), ..SessionConfig::default() };
    // A scripted adapter plays its turns from the start in each session, so
    // every scenario gets its own adapter and contract run.
    for scenario in scenarios::all() {
        let contract = Contract {
            fixtures: vec![fixture("scripted/approval.jsonl")],
            live: vec![LivePlan { name: scenario.name.to_owned(), config: config.clone(), actions: scenario.actions.clone() }],
            timeout: Duration::from_secs(10),
        };
        let report = run_contract(Arc::new(scenario.adapter()), &contract).await.unwrap();
        assert!(report.is_clean(), "{report}");
        assert_eq!(report.round_trips.len(), 1);
        assert_eq!(report.live.len(), 1);
    }
}
