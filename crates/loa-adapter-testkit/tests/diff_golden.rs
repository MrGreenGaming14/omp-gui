//! The format aware diff and golden files.

use loa_adapter_testkit::diff::MAX_DIFFERENCES;
use loa_adapter_testkit::{Error, Format, Golden, diff_bytes, diff_paths, render_golden};
use loa_core::{Draft, ElementKind, MemorySink, NodeId, PatchSink, Role, Value, attr};

#[test]
fn json_ignores_key_order_and_whitespace() {
    let a = br#"{"a": 1, "b": {"c": [1, 2]}}"#;
    let b = br#"{"b":{"c":[1,2]},"a":1}"#;
    assert_eq!(diff_bytes(Format::Json, a, b), None);
}

#[test]
fn json_reports_the_path_of_each_change() {
    let a = br#"{"items": [{"name": "x", "n": 1}], "gone": true, "key with space": 1}"#;
    let b = br#"{"items": [{"name": "y", "n": 1}, 3], "new": null, "key with space": 2}"#;
    let diff = diff_bytes(Format::Json, a, b).unwrap();
    assert_eq!(
        diff.differences,
        vec![
            r#"$.gone: missing from the export (was true)"#,
            r#"$.items[0].name: expected "x", got "y""#,
            r#"$.items: 1 items in the original, 2 in the export"#,
            r#"$["key with space"]: expected 1, got 2"#,
            r#"$.new: only in the export (null)"#,
        ]
    );
}

#[test]
fn jsonl_reports_an_added_line_without_shifting() {
    let a = b"{\"n\":1}\n{\"n\":2}\n{\"n\":3}\n";
    let b = b"{\"n\":1}\n{\"n\":2}\n{\"extra\":true}\n{\"n\":3}\n";
    let diff = diff_bytes(Format::JsonLines, a, b).unwrap();
    assert_eq!(diff.differences, vec![r#"export line 3: not in the original: {"extra":true}"#]);
}

#[test]
fn jsonl_compares_changed_lines_structurally() {
    let a = b"{\"n\":1}\n{\"n\":2,\"s\":\"a\"}\n";
    let b = b"{\"n\":1}\n{\"s\":\"b\",\"n\":2}\n";
    let diff = diff_bytes(Format::JsonLines, a, b).unwrap();
    assert_eq!(diff.differences, vec![r#"line 2 $.s: expected "a", got "b""#]);
}

#[test]
fn a_long_report_lists_the_first_differences_and_counts_the_rest() {
    let a: String = (0..30).map(|i| format!("{{\"n\":{i}}}\n")).collect();
    let b: String = (0..30).map(|i| format!("{{\"n\":{}}}\n", i + 100)).collect();
    let diff = diff_bytes(Format::JsonLines, a.as_bytes(), b.as_bytes()).unwrap();
    assert_eq!(diff.differences.len(), MAX_DIFFERENCES);
    assert_eq!(diff.more, 30 - MAX_DIFFERENCES);
    assert_eq!(diff.differences[0], "line 1 $.n: expected 0, got 100");
}

#[test]
fn text_falls_back_to_a_unified_diff() {
    let diff = diff_bytes(Format::Text, b"one\ntwo\nthree\n", b"one\nTWO\nthree\n").unwrap();
    assert_eq!(diff.differences.len(), 1);
    assert!(diff.differences[0].contains("-two\n+TWO\n"), "{}", diff.differences[0]);
}

#[test]
fn directories_are_compared_file_by_file() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    std::fs::create_dir(a.path().join("sub")).unwrap();
    std::fs::create_dir(b.path().join("sub")).unwrap();
    std::fs::write(a.path().join("sub/meta.json"), r#"{"v":1}"#).unwrap();
    std::fs::write(b.path().join("sub/meta.json"), r#"{"v":2}"#).unwrap();
    std::fs::write(a.path().join("only-a.txt"), "x").unwrap();
    std::fs::write(b.path().join("only-b.txt"), "y").unwrap();
    let diffs = diff_paths(a.path(), b.path()).unwrap();
    let report: Vec<(String, Vec<String>)> =
        diffs.iter().map(|d| (d.path.display().to_string(), d.differences.clone())).collect();
    assert_eq!(
        report,
        vec![
            ("only-a.txt".to_owned(), vec!["missing from the export".to_owned()]),
            ("sub/meta.json".to_owned(), vec!["$.v: expected 1, got 2".to_owned()]),
            ("only-b.txt".to_owned(), vec!["only in the export".to_owned()]),
        ]
    );
}

fn session(sink: &MemorySink) -> NodeId {
    let attrs = [
        (attr::ID, Value::from("s")),
        (attr::HARNESS, Value::from("h")),
        (attr::NODE, Value::from("n")),
        (attr::CREATED, Value::Int(1)),
    ];
    sink.commit_at(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""), 1).unwrap().node
}

#[test]
fn renderings_do_not_depend_on_node_ids() {
    let a = MemorySink::new();
    let s = session(&a);
    let t = a.commit_at(Draft::insert(s, ElementKind::Turn, Vec::<(&str, Value)>::new(), ""), 2).unwrap().node;
    let call = a.commit_at(Draft::insert(t, ElementKind::Call, [(attr::TOOL, Value::from("bash"))], ""), 3).unwrap().node;
    let call_ref = Value::Int(i64::try_from(call.0).unwrap());
    a.commit_at(Draft::insert(call, ElementKind::Decision, [(attr::KIND, Value::from("approval")), (attr::CALL, call_ref)], "ls"), 4).unwrap();

    // Same content, but a removed node shifts every later id.
    let b = MemorySink::new();
    let s = session(&b);
    let scrap = b.commit_at(Draft::insert(s, ElementKind::Turn, Vec::<(&str, Value)>::new(), ""), 2).unwrap().node;
    b.commit_at(Draft::remove(scrap), 2).unwrap();
    let t = b.commit_at(Draft::insert(s, ElementKind::Turn, Vec::<(&str, Value)>::new(), ""), 2).unwrap().node;
    let call = b.commit_at(Draft::insert(t, ElementKind::Call, [(attr::TOOL, Value::from("bash"))], ""), 3).unwrap().node;
    let call_ref = Value::Int(i64::try_from(call.0).unwrap());
    b.commit_at(Draft::insert(call, ElementKind::Decision, [(attr::KIND, Value::from("approval")), (attr::CALL, call_ref)], "ls"), 4).unwrap();

    let rendered = a.with_dom(render_golden);
    assert_eq!(rendered, b.with_dom(render_golden));
    assert!(rendered.contains("<decision call=#3 kind=\"approval\">\n        \"ls\"\n"), "{rendered}");
}

#[test]
fn golden_files_fail_on_change_and_bless_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/s.golden");
    let sink = MemorySink::new();
    let s = session(&sink);
    let t = sink.commit_at(Draft::insert(s, ElementKind::Turn, Vec::<(&str, Value)>::new(), ""), 2).unwrap().node;
    let m = sink.commit_at(Draft::insert(t, ElementKind::Message, [(attr::ROLE, Value::from(Role::User))], "hello"), 3).unwrap().node;

    let missing = Golden::new(false).check(&sink.with_dom(render_golden), &path).unwrap_err();
    assert!(matches!(missing, Error::GoldenMissing { .. }), "{missing}");

    Golden::new(true).check(&sink.with_dom(render_golden), &path).unwrap();
    Golden::new(false).check(&sink.with_dom(render_golden), &path).unwrap();

    sink.commit(Draft::append(m, " world")).unwrap();
    let err = Golden::new(false).check(&sink.with_dom(render_golden), &path).unwrap_err();
    let Error::GoldenMismatch { diff, .. } = &err else { panic!("{err}") };
    assert!(diff.contains("-      \"hello\"\n+      \"hello world\"\n"), "{diff}");
}
