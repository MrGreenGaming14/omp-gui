use crate::transcript::*;
use iced::widget::{markdown, text};
use iced::{Element, Theme};
use loa_core::{Attrs, Dom, Draft, ElementKind as K, NodeId, Value, attrs};
use loa_view::session::transcript::Intent;

/// A session DOM and a transcript fed every patch committed to it.
struct S {
    dom: Dom,
    chat: State,
    session: NodeId,
}

impl S {
    fn new() -> Self {
        let mut s = Self { dom: Dom::new(), chat: State::new(&Theme::Dark), session: NodeId::DOCUMENT };
        s.session = s.add(NodeId::DOCUMENT, K::Session, attrs! { ID: "s1", HARNESS: "omp", NODE: "n1", CREATED: 0i64 }, "");
        s
    }
    fn commit(&mut self, draft: Draft) -> NodeId {
        let patch = self.dom.commit(draft, 0).unwrap();
        self.chat.apply(&patch, &self.dom);
        patch.node
    }
    fn add(&mut self, parent: NodeId, kind: K, attrs: Attrs, text: &str) -> NodeId { self.commit(Draft::insert(parent, kind, attrs, text)) }
    fn turn(&mut self) -> NodeId { self.add(self.session, K::Turn, attrs! {}, "") }
    fn msg(&mut self, turn: NodeId, role: &str, text: &str) -> NodeId { self.add(turn, K::Message, attrs! { ROLE: role }, text) }
    fn call(&mut self, turn: NodeId, tool: &str, span: (i64, i64), result: &str) -> NodeId {
        let call = self.add(turn, K::Call, attrs! { TOOL: tool, STATUS: "done", STARTED: span.0, ENDED: span.1 }, "");
        self.add(call, K::Input, attrs! {}, "{}");
        self.add(call, K::Result, attrs! {}, result);
        call
    }
    fn view(&self) -> Element<'_, Message> { self.chat.view(&self.dom, |m| m, |slot| text(format!("card for {}", slot.node)).into()) }
    fn shows(&self, label: &str) -> bool { iced_test::simulator(self.view()).find(label).is_ok() }
    fn send(&mut self, m: Message) -> Option<Intent> { self.chat.update(m).1 }
    fn scroll(&mut self, offset: f32, viewport: f32) {
        let content = self.chat.heights.total() as f32;
        self.send(Message::Scrolled(ScrollMetrics { offset, viewport, content }));
    }
}

fn long(turns: usize) -> S {
    let mut s = S::new();
    for i in 0..turns {
        let t = s.turn();
        s.msg(t, "user", &format!("message {i}"));
    }
    s
}

#[test]
fn streaming_feeds_each_chunk_to_the_parser_once() {
    let mut s = S::new();
    let t = s.turn();
    let reply = s.msg(t, "assistant", "");
    // The reply is parsed when it is first drawn, then fed each chunk.
    drop(s.view());
    let mut full = String::new();
    for i in 0..200 {
        let chunk = ["## Part\n\n", "Some **bold** and `code` ", "- one\n- two\n\n", "```rust\nfn main() {}\n```\n\n"][i % 4];
        full.push_str(chunk);
        s.commit(Draft::append(reply, chunk));
        if i == 100 {
            // A structural change elsewhere in the turn rebuilds its rows; the reply keeps its parse.
            s.commit(Draft::insert_at(t, 0, K::Ext, attrs! { HARNESS: "omp" }, "x"));
        }
    }
    let streamed = s.chat.streamed[&reply].get().expect("drawn, so parsed");
    assert_eq!(streamed.fed, full.len(), "re-parsing per token would feed ~N²/2 bytes");
    let whole = markdown::Content::parse(&full.replace('\n', "  \n"));
    assert_eq!(format!("{:?}", streamed.items()), format!("{:?}", whole.items()));
    s.commit(Draft::set_text(reply, "# Final"));
    assert!(s.chat.streamed[&reply].get().is_none(), "the old parse is dropped");
    drop(s.view());
    let streamed = s.chat.streamed[&reply].get().expect("drawn, so parsed");
    assert_eq!(streamed.fed, "# Final".len());
    assert!(matches!(streamed.items(), [markdown::Item::Heading(..)]));
}

#[test]
fn copying_fenced_code_keeps_indent_blank_lines_and_author_spaces() {
    for (source, expected) in [
        ("Example:\n\n```text\n    first  \n\n\tsecond\n```\n", "    first  \n\n\tsecond\n"),
        (" ```text\n a\n\n b\n ```", "a\n\nb\n"),
        ("```sh\nprintf x |", "printf x |"),
    ] {
        let mut s = S::new();
        let t = s.turn();
        s.msg(t, "assistant", source);
        let copies = loa_ui_kit::testkit::click_label(s.view(), "Copy code block");
        assert_eq!(copies, [Message::Copy(expected.into())]);
    }
}

#[test]
fn rows_collapse_expand_and_report_clicks() {
    let mut s = S::new();
    let t = s.turn();
    let prompt = s.msg(t, "user", "fix the build");
    let a = s.call(t, "read", (1_000, 4_000), "fn a() {}");
    s.call(t, "read", (2_000, 9_000), "fn b() {}");
    s.call(t, "read", (3_000, 13_000), "fn c() {}");
    let thought = s.add(t, K::Message, attrs! { ROLE: "assistant", KIND: "thinking", STATUS: "done" }, "weighing");
    let bash = s.add(t, K::Call, attrs! { TOOL: "bash", TITLE: "cargo test", STATUS: "error" }, "");
    s.add(bash, K::Result, attrs! { IS_ERROR: true }, "exit 101");
    let decision = s.add(bash, K::Decision, attrs! { KIND: "approval" }, "rm -rf target");
    s.add(s.session, K::Ext, attrs! { HARNESS: "omp", TYPE: COMMANDS_EXT }, "{}");
    s.add(s.session, K::Todo, attrs! { STATE: "in_progress" }, "Write the parser");
    s.add(s.session, K::Compaction, attrs! { COVERS: Value::List(vec![Value::Int(2), Value::Int(3)]) }, "sum");
    let kinds: Vec<ItemKind> = s.chat.items.iter().map(|i| i.kind).collect();
    use ItemKind::*;
    assert_eq!(kinds, [User, Calls, Thinking, Calls, Decision, Todo, Compaction], "the command list is no row");
    for label in ["Read 3 files", "12s", "Thought", "Ran cargo test", "1 failed", "◐", "Compacted 2 turns", &format!("card for {decision}")] {
        assert!(s.shows(label), "{label}");
    }
    for hidden in ["fn b() {}", "weighing", "rm -rf target", "exit 101"] {
        assert!(!s.shows(hidden), "{hidden}");
    }
    let click = |s: &S, label| loa_ui_kit::testkit::click_label(s.view(), label);
    assert_eq!(click(&s, "Read 3 files"), [Message::Toggle(a)]);
    assert!(click(&s, "fix the build").is_empty(), "text no longer rewinds on click");
    assert_eq!(click(&s, "Rewind"), [Message::Rewind(prompt)]);
    assert_eq!(click(&s, "Copy message"), [Message::Copy("fix the build".into())]);
    assert_eq!(s.send(Message::Rewind(prompt)), Some(Intent::Rewind(prompt)));
    for key in [a, thought, bash] {
        s.send(Message::Toggle(key));
    }
    for label in ["fn b() {}", "weighing", "Error", "exit 101"] {
        assert!(s.shows(label), "{label}");
    }
}

#[test]
fn tool_verbs_read_like_claude_code() {
    let cases: &[(&str, usize, &str)] = &[("Read", 3, "Read 3 files"), ("edit", 1, "Edited 1 file"), ("grep", 2, "Searched 2 patterns"), ("web_search", 2, "Searched the web for 2 queries"), ("mystery", 1, "mystery"), ("mystery", 2, "mystery 2 calls")];
    for &(tool, n, label) in cases {
        let mut s = S::new();
        let t = s.turn();
        for _ in 0..n {
            s.call(t, tool, (0, 1), "");
        }
        assert_eq!(s.chat.items.len(), 1, "{tool}: one group");
        assert!(s.shows(label), "{label}");
    }
}

#[test]
fn incremental_rows_match_a_fresh_load_after_mixed_patches() {
    let mut s = S::new();
    let t1 = s.turn();
    s.msg(t1, "user", "one");
    let r1 = s.call(t1, "read", (0, 1), "a");
    s.call(t1, "read", (0, 1), "b");
    let t2 = s.turn();
    s.msg(t2, "user", "two");
    let reply = s.msg(t2, "assistant", "hi");
    s.add(s.session, K::Digest, attrs! { SOURCE_SEQ: 1i64 }, "digest");
    s.commit(Draft::insert_at(t1, 1, K::Call, attrs! { TOOL: "read" }, ""));
    s.add(t2, K::Call, attrs! { TOOL: "bash" }, "");
    s.add(t2, K::Call, attrs! { TOOL: "bash" }, "");
    let t3 = s.turn();
    s.msg(t3, "user", "three");
    s.commit(Draft::set(r1, "tool", "edit"));
    s.commit(Draft::set(reply, "kind", "thinking"));
    s.commit(Draft::remove(t2));
    s.add(t3, K::Todo, attrs! {}, "later");
    let fresh = State::from_dom(&s.dom, &Theme::Dark);
    assert_eq!(s.chat.items, fresh.items);
    assert!(s.chat.items.iter().enumerate().all(|(i, row)| s.chat.pos[&row.key] == i));
    assert!((s.chat.heights.total() - fresh.heights.total()).abs() < 1e-3);
}

#[test]
fn a_huge_transcript_builds_only_the_rows_around_the_viewport() {
    let mut s = long(10_000);
    let window = s.chat.window();
    assert!(window.len() < 100, "built {} rows", window.len());
    assert!(s.shows("message 9999") && !s.shows("message 0"));
    let content = s.chat.heights.total() as f32;
    s.scroll(content - 700.0, 700.0);
    s.scroll(s.chat.heights.prefix(5_000) as f32, 700.0);
    assert!(!s.chat.is_following());
    let window = s.chat.window();
    assert!(window.contains(&5_000) && window.len() < 100, "{window:?}");
    assert!(s.shows("message 5000") && !s.shows("message 9999"));
    let last = s.chat.items[9_999].key;
    s.commit(Draft::append(last, " more"));
    assert_eq!(s.chat.window(), window, "a token far below leaves the window alone");
}

#[test]
fn a_row_growing_above_the_viewport_keeps_the_reading_position() {
    let mut s = long(200);
    let content = s.chat.heights.total() as f32;
    s.scroll(content - 500.0, 500.0);
    let reading = s.chat.heights.prefix(150) as f32;
    s.scroll(reading, 500.0);
    let first_visible = s.chat.heights.find(f64::from(reading));
    let key = s.chat.items[3].key;
    let old = s.chat.heights.get(3).unwrap();
    s.send(Message::Measured { key, height: old + 120.0 });
    assert_eq!(s.chat.scroll.metrics.offset, reading + 120.0);
    assert_eq!(s.chat.heights.find(f64::from(s.chat.scroll.metrics.offset)), first_visible);
    // The measurement survives the turn's rows being rebuilt.
    let turn = s.chat.items[3].top;
    s.commit(Draft::insert_at(turn, 0, K::Ext, attrs! { HARNESS: "omp" }, ""));
    assert_eq!(s.chat.heights.get(s.chat.pos[&key]), Some(old + 120.0));
}

#[test]
fn scrolling_up_pins_the_view_and_jump_to_latest_follows_again() {
    let mut s = long(300);
    let content = s.chat.heights.total() as f32;
    s.scroll(content - 600.0, 600.0);
    assert!(s.chat.is_following() && !s.shows("Jump to latest ↓"));
    s.send(Message::Scrolled(ScrollMetrics { offset: content - 600.0, viewport: 600.0, content: content + 400.0 }));
    assert!(s.chat.is_following(), "content growing under a still viewport keeps following");
    s.scroll(1_000.0, 600.0);
    assert!(!s.chat.is_following());
    let window = s.chat.window();
    let t = s.turn();
    s.msg(t, "user", "new prompt");
    assert_eq!(s.chat.window(), window);
    assert!(!s.shows("new prompt"));
    assert_eq!(loa_ui_kit::testkit::click_label(s.view(), "Jump to latest ↓"), [Message::JumpToLatest]);
    s.send(Message::JumpToLatest);
    assert!(s.chat.is_following() && s.shows("new prompt"));
}

#[test]
fn another_session_starts_following_and_its_first_scroll_report_keeps_it() {
    let mut s = long(300);
    let content = s.chat.heights.total() as f32;
    let at = |offset| Message::Scrolled(ScrollMetrics { offset, viewport: 600.0, content });
    s.send(at(content - 600.0));
    s.send(at(1_000.0));
    assert!(!s.chat.is_following());
    // Reloading the same session (a resync) keeps the reading position.
    s.chat.load(&s.dom);
    assert!(!s.chat.is_following());
    // Another session follows its newest rows, even when the scrollable then reports a smaller
    // offset left over from the first one.
    s.commit(Draft::set(s.session, "id", "s2"));
    s.chat.load(&s.dom);
    assert!(s.chat.is_following());
    s.send(Message::Scrolled(ScrollMetrics { offset: 200.0, viewport: 600.0, content }));
    assert!(s.chat.is_following(), "the old offset is not a scroll up");
    assert!(!s.shows("Jump to latest ↓"));
}

#[test]
fn window_shift_keeps_row_state() {
    // Rows keep their widget state when the window of built rows slides, so only rows new to
    // the window report a size. Paired by position, every row took another row's state and
    // every sensor reported again.
    let mut s = long(300);
    let content = s.chat.heights.total() as f32;
    // Pinned at the top, where the scrollable starts too.
    s.send(Message::Scrolled(ScrollMetrics { offset: 1_000.0, viewport: 700.0, content }));
    s.send(Message::Scrolled(ScrollMetrics { offset: 0.0, viewport: 700.0, content }));
    assert!(!s.chat.is_following());

    let mut frames = loa_ui_kit::testkit::Frames::new();
    let mut frame = |s: &mut S, events: &[iced::Event]| -> Vec<NodeId> {
        let messages = frames.frame(s.view(), events);
        let keys = messages.iter().filter_map(|m| if let Message::Measured { key, .. } = m { Some(*key) } else { None }).collect();
        messages.into_iter().for_each(|m| drop(s.send(m)));
        keys
    };
    let mut reported = std::collections::HashSet::new();
    for _ in 0..3 {
        reported.extend(frame(&mut s, &[]));
    }
    assert!(!reported.is_empty(), "the first rows report their size");
    let before = s.chat.window();

    let wheel = iced::Event::Mouse(iced::mouse::Event::WheelScrolled { delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -1_500.0 } });
    let mut after_shift = frame(&mut s, &[wheel]);
    for _ in 0..3 {
        after_shift.extend(frame(&mut s, &[]));
    }
    let after = s.chat.window();
    assert!(after.start > before.start, "the window slid down: {before:?} -> {after:?}");
    assert!(!after_shift.is_empty(), "rows new to the window report their size");
    let again: Vec<_> = after_shift.iter().filter(|key| reported.contains(*key)).collect();
    assert!(again.is_empty(), "rows that stayed in the window reported again: {again:?}");
}

#[test]
fn a_new_sessions_first_reply_renders_frame_after_frame_without_a_state_mix_up() {
    // A first reply adds rows while the window shifts; `keyed_column` once paired a row's sensor
    // with a spacer's state and panicked with "Downcast on stateless state".
    let mut s = S::new();
    let step = |f: fn(&mut S, NodeId)| -> loa_ui_kit::testkit::Step<S> {
        Box::new(move |s: &mut S| {
            let last = s.dom.children(s.session).last().map_or(s.session, |n| n.id);
            f(s, last)
        })
    };
    let steps = vec![
        step(|s, _| {
            let t = s.turn();
            s.msg(t, "user", "Reply with pong");
        }),
        step(|s, t| {
            s.msg(t, "assistant", "po");
        }),
        step(|s, t| {
            s.call(t, "read", (1, 2), "file");
            s.msg(t, "assistant", "pong");
        }),
        step(|s, _| {
            let t = s.turn();
            s.msg(t, "user", "Now ping");
            s.msg(t, "assistant", "ping");
        }),
    ];
    loa_ui_kit::testkit::run_frames(&mut s, S::view, |s, m| drop(s.send(m)), steps);
    assert_eq!(s.chat.items.iter().filter(|i| i.kind == ItemKind::User).count(), 2);
}
