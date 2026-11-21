use super::*;

fn numbered(n: usize) -> String { (1..=n).map(|i| format!("line {i}\n")).collect() }
fn replace(text: &str, no: usize, with: &str) -> String { text.lines().enumerate().map(|(i, l)| format!("{}\n", if i + 1 == no { with } else { l })).collect() }
fn hunks(rows: &[Row]) -> Vec<(u32, u32, u32, u32)> { rows.iter().filter_map(|r| if let Row::Hunk(h) = r { Some((h.old_start, h.old_len, h.new_start, h.new_len)) } else { None }).collect() }
fn folds(rows: &[Row]) -> Vec<usize> { rows.iter().filter_map(|r| if let Row::Fold { hidden, .. } = r { Some(*hidden) } else { None }).collect() }

fn emphasized(f: &FileDiff, tag: Tag) -> Vec<&str> {
    let i = f.lines().iter().position(|l| l.tag == tag).unwrap();
    f.emphasis(i).iter().map(|r| &f.line_text(i)[r.clone()]).collect()
}

#[test]
fn text_diffs_fold_context_like_diff_u() {
    let n20 = numbered(20);
    let n30 = numbered(30);
    let n12 = numbered(12);
    // (old, new, hunks, folds)
    let cases = [
        (n20.clone(), replace(&n20, 10, "line ten"), vec![(7, 7, 7, 7)], vec![6, 7]),
        (n30.clone(), replace(&replace(&n30, 5, "five"), 20, "twenty"), vec![(1, 8, 1, 8), (17, 7, 17, 7)], vec![8, 7]),
        (n12.clone(), replace(&replace(&n12, 3, "three"), 9, "nine"), vec![(1, 12, 1, 12)], vec![]),
        (String::new(), "a\nb\n".into(), vec![(0, 0, 1, 2)], vec![]),
        ("a\n".into(), String::new(), vec![(1, 1, 0, 0)], vec![]),
    ];
    for (old, new, want_hunks, want_folds) in cases {
        let rows = FileDiff::from_texts(None, None, &old, &new).unified_rows();
        assert_eq!((hunks(&rows), folds(&rows)), (want_hunks, want_folds));
    }
    let mut f = FileDiff::from_texts(None, None, &n30, &replace(&replace(&n30, 5, "five"), 20, "twenty"));
    assert!(f.expand(0) && !f.expand(0), "a fold expands once");
    assert_eq!(hunks(&f.unified_rows()), [(1, 23, 1, 23)], "the fold between two changes merges their hunks");
    let pairs: Vec<_> = FileDiff::from_texts(None, None, "keep\nold\n", "keep\nnew one\nnew two\n").split_rows().into_iter().filter_map(|r| if let Row::Pair { old, new } = r { Some((old.is_some(), new.is_some())) } else { None }).collect();
    assert_eq!(pairs, [(true, true), (true, true), (false, true)]);
}

#[test]
fn changed_words_are_byte_ranges() {
    let f = FileDiff::from_texts(None, None, "let total = count + 1;\n", "let total = count + 2;\n");
    assert_eq!((emphasized(&f, Tag::Delete), emphasized(&f, Tag::Insert)), (vec!["1;"], vec!["2;"]));
    let unrelated = FileDiff::from_texts(None, None, "fn main() {}\n", "use std::io::Read as _;\n");
    assert!((0..unrelated.lines().len()).all(|i| unrelated.emphasis(i).is_empty()));
    let Intra { old, new } = intra_line("naïve café tea", "naïve café coffee").unwrap();
    assert_eq!((&"naïve café tea"[old[0].clone()], &"naïve café coffee"[new[0].clone()]), ("tea", "coffee"));
}

const GIT_DIFF: &str = "commit message text that is not part of any file
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
fn parses_git_and_plain_diffs_and_reports_errors() {
    let files = parse_unified(GIT_DIFF).unwrap();
    let summary: Vec<_> = files.iter().map(|f| (f.status(), f.path(), f.added(), f.removed(), f.binary)).collect();
    use FileStatus::*;
    assert_eq!(summary, [(Modified, "src/lib.rs", 3, 2, false), (Renamed, "docs/new.md", 0, 0, false), (Added, "added.txt", 2, 0, false), (Deleted, "logo.png", 0, 0, true)]);
    let lib = &files[0];
    let rows = lib.unified_rows();
    assert_eq!(hunks(&rows), [(10, 4, 10, 5), (40, 2, 41, 2)]);
    let sections: Vec<_> = rows.iter().filter_map(|r| if let Row::Hunk(h) = r { Some(lib.section(h)) } else { None }).collect();
    assert_eq!(sections, ["fn compute() {", "impl Thing {"]);
    let numbers: Vec<_> = lib.lines().iter().enumerate().map(|(i, l)| (l.old_no, l.new_no, lib.line_text(i))).collect();
    assert_eq!([numbers[1], numbers[3], numbers[4], numbers[7]], [(Some(11), None, "    let b = 2;"), (None, Some(12), "    let c = 4;"), (Some(12), Some(13), ""), (None, Some(41), "    new_tail()")]);
    assert_eq!(emphasized(lib, Tag::Delete), ["2;"]);

    let plain = &parse_unified("--- a.txt\t2024-01-01\n+++ b.txt\t2024-01-02\n@@ -1 +1 @@\n-x\n+y\n").unwrap()[0];
    assert_eq!((plain.old_path.as_deref(), plain.new_path.as_deref()), (Some("a.txt"), Some("b.txt")));
    let errors = [
        ("--- a\n+++ b\n@@ -1,3 +1,3 @@\n a\n-b\n", ParseError::Truncated { line: 5, old: 1, new: 2 }),
        ("@@ -1 +1 @@\n-a\n+b\n", ParseError::Orphan { line: 1 }),
        ("--- a\n+++ b\n@@ -x +1 @@\n", ParseError::HunkHeader { line: 3 }),
    ];
    for (input, error) in errors { assert_eq!(parse_unified(input).unwrap_err(), error) }
    let mut long = String::from("--- a\n+++ b\n@@ -1,12 +1,12 @@\n-x\n+y\n");
    (2..=12).for_each(|i| long.push_str(&format!(" ctx {i}\n")));
    let rows = parse_unified(&long).unwrap()[0].unified_rows();
    assert_eq!((hunks(&rows), folds(&rows)), (vec![(1, 4, 1, 4), (10, 3, 10, 3)], vec![5]), "hunk edges keep their context");
}

#[test]
fn fifty_thousand_line_diff_builds_only_the_rows_in_view() {
    let old = numbered(50_000);
    let mut state = State::new();
    let _ = state.set_files(vec![FileDiff::from_texts(None, None, &old, &replace(&old, 25_000, "the middle"))]);
    assert_eq!(state.rows()[0], Row::Fold { fold: 0, hidden: 24_996 });
    let _ = state.update(Message::Expand(0));
    let _ = state.update(Message::Scrolled { offset: 20_000.0 * row_height(), height: 600.0 });
    let range = state.visible_rows();
    assert!(range.len() <= 600 / row_height() as usize + 2 * OVERSCAN + 1 && range.contains(&20_000), "{range:?}");
    assert_eq!((state.rows()[20_000], state.selected().unwrap().line_text(19_999)), (Row::Line(19_999), "line 20000"));
}

#[test]
fn mode_switch_file_select_open_and_smoke_render() {
    let mut state = State::new();
    let _ = state.set_files(vec![FileDiff::from_texts(Some("a".into()), Some("a".into()), "1\n", "2\n"), FileDiff::from_texts(Some("gone".into()), None, "x\n", "")]);
    let _ = state.update(Message::SetMode(Mode::Split));
    assert!(state.rows().iter().all(|r| matches!(r, Row::Hunk(_) | Row::Pair { .. })));
    let _ = (state.update(Message::SelectFile(1)), state.update(Message::SelectFile(7)));
    assert_eq!(state.update(Message::Open).1, Some(Event::OpenFile("gone".into())));
    let ui = iced_test::Simulator::with_size(iced::Settings::default(), iced::Size::new(360.0, 400.0), state.view());
    drop(ui);
}
