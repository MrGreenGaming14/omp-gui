//! Native session discovery and recognition across the harness codecs.

use loa_harness::native;

#[test]
fn native_sessions_are_found_and_recognized() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let cases: &[(&str, &str, Option<&str>)] = &[
        (".omp/agent/sessions/-Projects-a/2026_x.jsonl", "{\"type\":\"title\",\"title\":\"Sorter\"}\n", Some(native::OMP)),
        (".claude/projects/-home-a/u.jsonl", "{\"type\":\"user\",\"sessionId\":\"u\"}\n", Some(native::CLAUDE)),
        (".codex/sessions/2026/11/19/rollout-1.jsonl", "{\"type\":\"session_meta\"}\n", Some(native::CODEX)),
        (".codex/sessions/2026/11/19/other.txt", "x", None),
        ("elsewhere/export.json", "{\"info\":{\"id\":\"ses_1\"},\"messages\":[]}", Some(native::OPENCODE)),
        ("elsewhere/omp.jsonl", "{\"type\":\"session\",\"version\":3}\n", Some(native::OMP)),
        ("elsewhere/rollout-9.jsonl", "{}\n", Some(native::CODEX)),
        ("elsewhere/notes.jsonl", "{\"a\":1}\n", None),
    ];
    for (path, text, _) in cases {
        std::fs::create_dir_all(h.join(path).parent().unwrap()).unwrap();
        std::fs::write(h.join(path), text).unwrap();
    }
    for (path, _, harness) in cases {
        assert_eq!(native::harness_of(&h.join(path)), *harness, "{path}");
    }
    let found = native::discover(h);
    let mut kinds: Vec<_> = found.iter().map(|s| (s.harness, s.project.as_deref(), s.title.as_deref())).collect();
    kinds.sort();
    assert_eq!(kinds, [("claude-code", Some("-home-a"), None), ("codex", None, None), ("omp", Some("-Projects-a"), Some("Sorter"))]);
}
