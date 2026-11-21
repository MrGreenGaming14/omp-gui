use super::*;

fn hunks(rows: &[Row]) -> Vec<(u32, u32, u32, u32)> { rows.iter().filter_map(|r| if let Row::Hunk(h) = r { Some((h.old_start, h.old_len, h.new_start, h.new_len)) } else { None }).collect() }
fn folds(rows: &[Row]) -> Vec<usize> { rows.iter().filter_map(|r| if let Row::Fold { hidden, .. } = r { Some(*hidden) } else { None }).collect() }
fn context(lines: std::ops::RangeInclusive<usize>) -> String { lines.map(|i| format!(" ctx {i}\n")).collect() }

const GIT: &str = "commit message
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,2 +10,3 @@ fn compute() {
     let a = 1;
-    let b = 2;
+    let b = 3;
+    let c = 4;
@@ -40,1 +41,1 @@ impl Thing {
-    old_tail()
+    new_tail()
\\ No newline at end of file
diff --git a/docs/old.md b/docs/new.md
rename from docs/old.md
rename to docs/new.md
diff --git a/added.txt b/added.txt
new file mode 100644
--- /dev/null
+++ b/added.txt
@@ -0,0 +1,2 @@
+first
+second
diff --git a/logo.png b/logo.png
deleted file mode 100644
Binary files a/logo.png and /dev/null differ
";

/// A diff, its files (status, path, added, removed, binary), its hunk ranges and folded gaps.
type Case<'a> = (&'a str, &'a [(FileStatus, &'a str, usize, usize, bool)], Vec<(u32, u32, u32, u32)>, Vec<usize>);

#[test]
fn parses_unified_diffs() {
    use FileStatus::*;
    let folded = format!("--- a\n+++ b\n@@ -1,12 +1,12 @@\n-x\n+y\n{}", context(2..=12));
    let cases: [Case; 3] = [
        (GIT, &[(Modified, "src/lib.rs", 3, 2, false), (Renamed, "docs/new.md", 0, 0, false), (Added, "added.txt", 2, 0, false), (Deleted, "logo.png", 0, 0, true)], vec![(10, 2, 10, 3), (40, 1, 41, 1)], vec![]),
        ("--- a.txt\t2024-01-01\n+++ b.txt\t2024-01-02\n@@ -1 +1 @@\n-x\n+y\n", &[(Renamed, "b.txt", 1, 1, false)], vec![(1, 1, 1, 1)], vec![]),
        (&folded, &[(Renamed, "b", 1, 1, false)], vec![(1, 4, 1, 4), (10, 3, 10, 3)], vec![5]),
    ];
    for (input, want, want_hunks, want_folds) in cases {
        let files = parse_unified(input).unwrap();
        assert_eq!(files.iter().map(|f| (f.status(), f.path(), f.added(), f.removed(), f.binary)).collect::<Vec<_>>(), want);
        let rows = files[0].unified_rows();
        assert_eq!((hunks(&rows), folds(&rows)), (want_hunks, want_folds), "{input}");
    }
    let lib = &parse_unified(GIT).unwrap()[0];
    let sections: Vec<_> = lib.unified_rows().iter().filter_map(|r| if let Row::Hunk(h) = r { Some(lib.section(h)) } else { None }).collect();
    assert_eq!(sections, ["fn compute() {", "impl Thing {"]);
    let line = |i: usize| (lib.lines()[i].old_no, lib.lines()[i].new_no, lib.line_text(i), lib.emphasis(i).iter().map(|r| &lib.line_text(i)[r.clone()]).collect::<Vec<_>>());
    assert_eq!([line(1), line(3)], [(Some(11), None, "    let b = 2;", vec!["2;"]), (None, Some(12), "    let c = 4;", vec![])]);
    let split = lib.split_rows().into_iter().filter_map(|r| if let Row::Pair { old, new } = r { Some((old, new)) } else { None }).collect::<Vec<_>>();
    assert_eq!(split[..3], [(Some(0), Some(0)), (Some(1), Some(2)), (None, Some(3))]);
    let errors = [
        ("--- a\n+++ b\n@@ -1,3 +1,3 @@\n a\n-b\n", ParseError::Truncated { line: 5, old: 1, new: 2 }),
        ("@@ -1 +1 @@\n-a\n+b\n", ParseError::Orphan { line: 1 }),
        ("--- a\n+++ b\n@@ -x +1 @@\n", ParseError::HunkHeader { line: 3 }),
        ("--- a\n+++ b\n@@ -1,1 +1,2 @@\n a\n a\n", ParseError::Overflow { line: 5 }),
    ];
    for (input, error) in errors { assert_eq!(parse_unified(input).unwrap_err(), error) }
}

#[test]
fn fifty_thousand_line_diff_builds_only_the_rows_in_view() {
    let patch = format!("--- a\n+++ b\n@@ -1,50000 +1,50000 @@\n{}-old\n+new\n{}", context(1..=24_999), context(25_001..=50_000));
    let mut state = State::new();
    let _ = state.set_files(parse_unified(&patch).unwrap());
    assert_eq!(state.rows[4], Row::Fold { fold: 0, hidden: 24_993 });
    let _ = state.update(Message::Expand(0));
    let _ = state.update(Message::Scrolled { offset: 20_000.0 * row_height(), height: 600.0 });
    let range = state.visible_rows();
    assert!(range.len() <= 600 / row_height() as usize + 2 * OVERSCAN + 1 && range.contains(&20_000), "{range:?}");
    assert_eq!((state.rows[20_000], state.selected().unwrap().line_text(19_999)), (Row::Line(19_999), "ctx 20000"));
}
