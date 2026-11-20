use super::*;

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

fn replace_line(text: &str, line_no: usize, with: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(i, l)| if i + 1 == line_no { format!("{with}\n") } else { format!("{l}\n") })
        .collect()
}

fn hunks(rows: &[Row]) -> Vec<Hunk> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Hunk(h) => Some(*h),
            _ => None,
        })
        .collect()
}

fn header(h: &Hunk) -> (u32, u32, u32, u32) {
    (h.old_start, h.old_len, h.new_start, h.new_len)
}

#[test]
fn max_line_no_is_the_largest_numbered_line() {
    let old = numbered(9);
    let new = numbered(12);
    let file = FileDiff::from_texts(None, None, &old, &new);
    assert_eq!(file.max_line_no(), 12);
}

#[test]
fn one_change_gets_diff_u_hunk_and_folds_on_both_sides() {
    let old = numbered(20);
    let new = replace_line(&old, 10, "line ten");
    let file = FileDiff::from_texts(Some("a.txt".into()), Some("a.txt".into()), &old, &new);
    assert_eq!((file.added(), file.removed()), (1, 1));

    let rows = file.unified_rows();
    // Lines 1..=6 and 14..=20 fold away; 7..=13 stay with the change.
    assert_eq!(rows[0], Row::Fold { fold: 0, hidden: 6 });
    assert_eq!(hunks(&rows).iter().map(header).collect::<Vec<_>>(), vec![(7, 7, 7, 7)]);
    assert_eq!(rows.last(), Some(&Row::Fold { fold: 1, hidden: 7 }));
    let shown: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            Row::Line(i) => Some(file.line_text(*i)),
            _ => None,
        })
        .collect();
    assert_eq!(shown, ["line 7", "line 8", "line 9", "line 10", "line ten", "line 11", "line 12", "line 13"]);
}

#[test]
fn changed_word_is_emphasized_and_unrelated_lines_are_not() {
    let file = FileDiff::from_texts(None, None, "let total = count + 1;\n", "let total = count + 2;\n");
    let old = file.lines().iter().position(|l| l.tag == Tag::Delete).unwrap();
    let new = file.lines().iter().position(|l| l.tag == Tag::Insert).unwrap();
    let pick = |i: usize| -> Vec<&str> {
        file.emphasis(i).iter().map(|r| &file.line_text(i)[r.clone()]).collect()
    };
    assert_eq!(pick(old), ["1;"]);
    assert_eq!(pick(new), ["2;"]);

    let unrelated = FileDiff::from_texts(None, None, "fn main() {}\n", "use std::io::Read as _;\n");
    assert!(unrelated.lines().iter().enumerate().all(|(i, _)| unrelated.emphasis(i).is_empty()));
}

#[test]
fn emphasis_ranges_are_byte_ranges_on_multibyte_text() {
    let Intra { old, new } = intra_line("naïve café tea", "naïve café coffee").unwrap();
    assert_eq!(&"naïve café tea"[old[0].clone()], "tea");
    assert_eq!(&"naïve café coffee"[new[0].clone()], "coffee");
}

#[test]
fn expanding_the_fold_between_two_changes_merges_their_hunks() {
    let old = numbered(30);
    let new = replace_line(&replace_line(&old, 5, "five"), 20, "twenty");
    let mut file = FileDiff::from_texts(None, None, &old, &new);
    let rows = file.unified_rows();
    // Line 1..=4 before the first change: hiding one line would gain nothing.
    assert_eq!(hunks(&rows).iter().map(header).collect::<Vec<_>>(), vec![(1, 8, 1, 8), (17, 7, 17, 7)]);
    let between = rows
        .iter()
        .find_map(|r| match r {
            Row::Fold { fold, hidden } if *hidden == 8 => Some(*fold),
            _ => None,
        })
        .expect("lines 9..=16 fold between the changes");

    assert!(file.expand(between));
    assert!(!file.expand(between), "a fold expands once");
    let rows = file.unified_rows();
    assert_eq!(hunks(&rows).iter().map(header).collect::<Vec<_>>(), vec![(1, 23, 1, 23)]);
}

#[test]
fn short_gaps_between_changes_stay_in_one_hunk() {
    let old = numbered(12);
    let new = replace_line(&replace_line(&old, 3, "three"), 9, "nine");
    let file = FileDiff::from_texts(None, None, &old, &new);
    let rows = file.unified_rows();
    // 5 unchanged lines between the changes: fewer than two contexts plus MIN_FOLD.
    assert_eq!(hunks(&rows).len(), 1);
    assert!(!rows.iter().any(|r| matches!(r, Row::Fold { .. })));
}

#[test]
fn split_mode_pairs_replaced_lines_and_pads_the_shorter_side() {
    let file = FileDiff::from_texts(None, None, "keep\nold\n", "keep\nnew one\nnew two\nnew three\n");
    let pairs: Vec<(Option<&str>, Option<&str>)> = file
        .split_rows()
        .iter()
        .filter_map(|r| match r {
            Row::Pair { old, new } => Some((old.map(|i| file.line_text(i)), new.map(|i| file.line_text(i)))),
            _ => None,
        })
        .collect();
    assert_eq!(
        pairs,
        [
            (Some("keep"), Some("keep")),
            (Some("old"), Some("new one")),
            (None, Some("new two")),
            (None, Some("new three")),
        ]
    );
}

#[test]
fn added_and_deleted_files_have_empty_side_headers() {
    let added = FileDiff::from_texts(None, Some("new.rs".into()), "", "a\nb\n");
    assert_eq!(added.status(), FileStatus::Added);
    assert_eq!(hunks(&added.unified_rows()).iter().map(header).collect::<Vec<_>>(), vec![(0, 0, 1, 2)]);
    let deleted = FileDiff::from_texts(Some("old.rs".into()), None, "a\n", "");
    assert_eq!(deleted.status(), FileStatus::Deleted);
    assert_eq!(deleted.path(), "old.rs");
    assert_eq!(hunks(&deleted.unified_rows()).iter().map(header).collect::<Vec<_>>(), vec![(1, 1, 0, 0)]);
}

const GIT_DIFF: &str = "\
commit message text that is not part of any file
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,4 +10,5 @@ fn compute() {
     let a = 1;
-    let b = 2;
+    let b = 3;
+    let c = 4;
 
     a + b
@@ -40,2 +41,2 @@ impl Thing {
-    old_tail()
+    new_tail()
 }
\\ No newline at end of file
diff --git a/docs/old.md b/docs/new.md
similarity index 90%
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

#[test]
fn parses_multi_file_git_diff_with_numbers_and_sections() {
    let files = parse_unified(GIT_DIFF).unwrap();
    let summary: Vec<(FileStatus, &str, usize, usize, bool)> =
        files.iter().map(|f| (f.status(), f.path(), f.added(), f.removed(), f.binary)).collect();
    assert_eq!(
        summary,
        [
            (FileStatus::Modified, "src/lib.rs", 3, 2, false),
            (FileStatus::Renamed, "docs/new.md", 0, 0, false),
            (FileStatus::Added, "added.txt", 2, 0, false),
            (FileStatus::Deleted, "logo.png", 0, 0, true),
        ]
    );

    let lib = &files[0];
    let rows = lib.unified_rows();
    let hs = hunks(&rows);
    assert_eq!(hs.iter().map(header).collect::<Vec<_>>(), vec![(10, 4, 10, 5), (40, 2, 41, 2)]);
    assert_eq!(lib.section(&hs[0]), "fn compute() {");
    assert_eq!(lib.section(&hs[1]), "impl Thing {");
    let numbers: Vec<(Option<u32>, Option<u32>, &str)> =
        lib.lines().iter().enumerate().map(|(i, l)| (l.old_no, l.new_no, lib.line_text(i))).collect();
    assert_eq!(numbers[1], (Some(11), None, "    let b = 2;"));
    assert_eq!(numbers[3], (None, Some(12), "    let c = 4;"));
    assert_eq!(numbers[4], (Some(12), Some(13), ""));
    assert_eq!(numbers[7], (None, Some(41), "    new_tail()"));
    let b = 1;
    let emphasized: Vec<&str> = lib.emphasis(b).iter().map(|r| &lib.line_text(b)[r.clone()]).collect();
    assert_eq!(emphasized, ["2;"]);
}

#[test]
fn plain_diff_u_and_errors() {
    let plain = "--- a.txt\t2024-01-01 00:00:00\n+++ b.txt\t2024-01-02 00:00:00\n@@ -1 +1 @@\n-x\n+y\n";
    let files = parse_unified(plain).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!((files[0].old_path.as_deref(), files[0].new_path.as_deref()), (Some("a.txt"), Some("b.txt")));

    let truncated = "--- a\n+++ b\n@@ -1,3 +1,3 @@\n a\n-b\n";
    assert_eq!(parse_unified(truncated).unwrap_err(), ParseError::Truncated { line: 5, old: 1, new: 2 });
    let orphan = "@@ -1 +1 @@\n-a\n+b\n";
    assert_eq!(parse_unified(orphan).unwrap_err(), ParseError::Orphan { line: 1 });
    let bad = "--- a\n+++ b\n@@ -x +1 @@\n";
    assert_eq!(parse_unified(bad).unwrap_err(), ParseError::HunkHeader { line: 3 });
}

#[test]
fn parsed_hunk_with_long_context_folds_its_middle_only() {
    let mut text = String::from("--- a\n+++ b\n@@ -1,12 +1,12 @@\n-x\n+y\n");
    for i in 2..=12 {
        text.push_str(&format!(" ctx {i}\n"));
    }
    let file = &parse_unified(&text).unwrap()[0];
    let rows = file.unified_rows();
    // Context after the change keeps 3 lines; the tail edge is a hunk edge and keeps 3 too.
    assert!(rows.contains(&Row::Fold { fold: 0, hidden: 5 }));
    assert_eq!(hunks(&rows).iter().map(header).collect::<Vec<_>>(), vec![(1, 4, 1, 4), (10, 3, 10, 3)]);
}

#[test]
fn fifty_thousand_line_diff_builds_only_the_rows_in_view() {
    let old = numbered(50_000);
    let new = replace_line(&old, 25_000, "the middle");
    let mut state = State::new();
    let _ = state.set_files(vec![FileDiff::from_texts(None, None, &old, &new)]);
    let fold = match state.rows()[0] {
        Row::Fold { fold, hidden } => {
            assert_eq!(hidden, 24_996);
            fold
        }
        other => panic!("expected a fold first, got {other:?}"),
    };
    let _ = state.update(Message::Expand(fold));
    assert!(state.rows().len() > 25_000);

    let _ = state.update(Message::Scrolled { offset: 20_000.0 * style::ROW_HEIGHT, height: 600.0 });
    let range = state.visible_rows();
    assert!(range.len() <= 600 / style::ROW_HEIGHT as usize + 2 * OVERSCAN + 1, "{range:?}");
    assert!(range.contains(&20_000));
    let file = state.selected().unwrap();
    assert_eq!(state.rows()[20_000], Row::Line(19_999));
    assert_eq!(file.line_text(19_999), "line 20000");
}

#[test]
fn mode_switch_file_select_and_open() {
    let mut state = State::new();
    let _ = state.set_files(vec![
        FileDiff::from_texts(Some("a".into()), Some("a".into()), "1\n", "2\n"),
        FileDiff::from_texts(Some("gone".into()), None, "x\n", ""),
    ]);
    assert!(state.rows().iter().any(|r| matches!(r, Row::Line(_))));
    let _ = state.update(Message::SetMode(Mode::Split));
    assert!(state.rows().iter().all(|r| matches!(r, Row::Hunk(_) | Row::Pair { .. })));

    let _ = state.update(Message::SelectFile(1));
    assert_eq!(state.update(Message::Open).1, Some(Event::OpenFile("gone".into())));
    assert_eq!(state.rows()[1], Row::Pair { old: Some(0), new: None });
    let _ = state.update(Message::SelectFile(7));
    assert_eq!(state.selected().map(FileDiff::path), Some("gone"));
}
