use super::*;
use iced::widget::text;
use iced_test::simulator;
use loa_core::{Draft, Value};

/// A session DOM and a transcript fed every patch committed to it.
struct Session {
    dom: Dom,
    chat: State,
    session: NodeId,
}

impl Session {
    fn new() -> Self {
        let mut dom = Dom::new();
        let mut chat = State::new(&Theme::Dark);
        let attrs: [(&str, Value); 4] = [
            ("id", "s1".into()),
            ("harness", "omp".into()),
            ("node", "n1".into()),
            ("created", Value::Int(0)),
        ];
        let patch = dom
            .commit(
                Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""),
                0,
            )
            .unwrap();
        chat.apply(&patch, &dom);
        Self {
            dom,
            chat,
            session: patch.node,
        }
    }

    fn commit(&mut self, draft: Draft) -> NodeId {
        let patch = self.dom.commit(draft, 0).unwrap();
        self.chat.apply(&patch, &self.dom);
        patch.node
    }

    fn insert(
        &mut self,
        parent: NodeId,
        kind: ElementKind,
        attrs: &[(&str, Value)],
        text: &str,
    ) -> NodeId {
        self.commit(Draft::insert(parent, kind, attrs.iter().cloned(), text))
    }

    fn turn(&mut self) -> NodeId {
        self.insert(
            self.session,
            ElementKind::Turn,
            &[("status", "running".into())],
            "",
        )
    }

    fn user(&mut self, turn: NodeId, text: &str) -> NodeId {
        self.insert(turn, ElementKind::Message, &[("role", "user".into())], text)
    }

    fn call(
        &mut self,
        turn: NodeId,
        tool: &str,
        started: i64,
        ended: i64,
        input: &str,
        result: &str,
    ) -> NodeId {
        let attrs = [
            ("tool", Value::from(tool)),
            ("status", "done".into()),
            ("started", Value::Int(started)),
            ("ended", Value::Int(ended)),
        ];
        let call = self.insert(turn, ElementKind::Call, &attrs, "");
        self.insert(call, ElementKind::Input, &[], input);
        self.insert(call, ElementKind::Result, &[], result);
        call
    }

    fn rows(&self) -> Vec<(ItemKind, NodeId, Vec<NodeId>)> {
        self.chat
            .items
            .iter()
            .map(|i| (i.kind, i.key, i.calls.clone()))
            .collect()
    }

    /// Messages a click on the text `label` produces.
    fn click(&self, label: &str) -> Vec<Message> {
        let mut ui = simulator(self.view());
        ui.click(label)
            .unwrap_or_else(|e| panic!("click {label:?}: {e}"));
        ui.into_messages().collect()
    }

    fn shows(&self, label: &str) -> bool {
        simulator(self.view()).find(label).is_ok()
    }

    fn view(&self) -> Element<'_, Message> {
        self.chat.view(
            &self.dom,
            |m| m,
            |slot| text(format!("card for {}", slot.node)).into(),
        )
    }

    fn send(&mut self, message: Message) -> Option<Event> {
        self.chat.update(message).1
    }
}

// #55 Streaming markdown with incremental parsing

#[test]
fn streaming_feeds_each_chunk_to_the_parser_once() {
    let mut s = Session::new();
    let turn = s.turn();
    s.user(turn, "explain");
    let reply = s.insert(
        turn,
        ElementKind::Message,
        &[("role", "assistant".into())],
        "",
    );
    let mut full = String::new();
    for i in 0..400 {
        let chunk = match i % 4 {
            0 => format!("## Part {i}\n\n"),
            1 => format!("Some **bold {i}** and `code` "),
            2 => "- item one\n- item two\n\n".to_owned(),
            _ => "```rust\nfn main() {}\n```\n\n".to_owned(),
        };
        full.push_str(&chunk);
        s.commit(Draft::append(reply, chunk));
        if i == 200 {
            // A structural change elsewhere in the same turn rebuilds the
            // turn's rows; the reply must keep its parse.
            s.commit(Draft::insert_at(
                turn,
                0,
                ElementKind::Ext,
                [("harness", Value::from("omp"))],
                "x",
            ));
        }
    }
    let streamed = &s.chat.streamed[&reply];
    // Re-parsing the whole message per token would feed ~N²/2 bytes.
    assert_eq!(streamed.fed(), full.len());
    let whole = markdown::Content::parse(&full);
    assert_eq!(
        format!("{:?}", streamed.items()),
        format!("{:?}", whole.items())
    );
}

#[test]
fn replacing_a_message_text_reparses_it() {
    let mut s = Session::new();
    let turn = s.turn();
    let reply = s.insert(
        turn,
        ElementKind::Message,
        &[("role", "assistant".into())],
        "first draft",
    );
    s.commit(Draft::set_text(reply, "# Final"));
    let streamed = &s.chat.streamed[&reply];
    assert_eq!(streamed.fed(), "# Final".len());
    assert!(matches!(streamed.items(), [markdown::Item::Heading(..)]));
}

#[test]
fn thinking_is_collapsed_until_clicked() {
    let mut s = Session::new();
    let turn = s.turn();
    let thought = s.insert(
        turn,
        ElementKind::Message,
        &[
            ("role", "assistant".into()),
            ("kind", "thinking".into()),
            ("status", "done".into()),
        ],
        "weighing the options",
    );
    assert!(s.shows("Thought"));
    assert!(!s.shows("weighing the options"));
    assert_eq!(s.click("Thought"), vec![Message::Toggle(thought)]);
    s.send(Message::Toggle(thought));
    assert!(s.shows("weighing the options"));
}

// #56 Collapsed tool rows that expand on click

#[test]
fn consecutive_reads_collapse_into_one_row_that_expands_on_click() {
    let mut s = Session::new();
    let turn = s.turn();
    let a = s.call(
        turn,
        "read",
        1_000,
        4_000,
        r#"{"path":"a.rs"}"#,
        "fn a() {}",
    );
    let b = s.call(
        turn,
        "read",
        2_000,
        9_000,
        r#"{"path":"b.rs"}"#,
        "fn b() {}",
    );
    let c = s.call(
        turn,
        "read",
        3_000,
        13_000,
        r#"{"path":"c.rs"}"#,
        "fn c() {}",
    );
    assert_eq!(s.rows(), vec![(ItemKind::Calls, a, vec![a, b, c])]);
    assert!(s.shows("Read 3 files"));
    assert!(s.shows("12s"));
    assert!(!s.shows("fn b() {}"));

    let clicked = s.click("Read 3 files");
    assert_eq!(clicked, vec![Message::Toggle(a)]);
    s.send(Message::Toggle(a));
    assert!(s.shows(r#"{"path":"b.rs"}"#));
    assert!(s.shows("fn b() {}"));

    s.send(Message::Toggle(a));
    assert!(!s.shows("fn b() {}"));
}

#[test]
fn a_failed_call_is_marked_on_the_collapsed_row_and_in_its_details() {
    let mut s = Session::new();
    let turn = s.turn();
    let call = s.insert(
        turn,
        ElementKind::Call,
        &[("tool", "bash".into()), ("title", "cargo test".into())],
        "",
    );
    s.commit(Draft::insert(
        call,
        ElementKind::Result,
        [("is_error", Value::Bool(true))],
        "exit 101",
    ));
    s.commit(Draft::set(call, "status", "error"));
    assert!(s.shows("Ran cargo test"));
    assert!(s.shows("1 failed"));
    s.send(Message::Toggle(call));
    assert!(s.shows("Error"));
    assert!(s.shows("exit 101"));
}

#[test]
fn a_call_joining_a_streaming_group_updates_the_same_row() {
    let mut s = Session::new();
    let turn = s.turn();
    let first = s.call(turn, "grep", 0, 10, "{}", "");
    assert!(s.shows("Searched 1 pattern"));
    s.call(turn, "grep", 0, 10, "{}", "");
    assert_eq!(s.chat.items.len(), 1);
    assert_eq!(s.chat.items[0].key, first);
    assert!(s.shows("Searched 2 patterns"));
}

#[test]
fn jobs_todos_compactions_and_ext_get_compact_rows() {
    let mut s = Session::new();
    let turn = s.turn();
    let job = s.insert(
        turn,
        ElementKind::Job,
        &[
            ("kind", "shell".into()),
            ("status", "running".into()),
            ("command", "npm run dev".into()),
        ],
        "",
    );
    s.commit(Draft::append(job, "listening on 3000\n"));
    s.insert(
        s.session,
        ElementKind::Todo,
        &[("state", "in_progress".into())],
        "Write the parser",
    );
    s.insert(
        s.session,
        ElementKind::Compaction,
        &[("covers", Value::List(vec![Value::Int(2), Value::Int(3)]))],
        "sum",
    );
    s.insert(
        turn,
        ElementKind::Ext,
        &[("harness", "claude".into()), ("type", "hook".into())],
        "raw data",
    );

    assert!(s.shows("npm run dev"));
    assert!(s.shows("running…"));
    assert!(!s.shows("listening on 3000\n"));
    s.send(Message::Toggle(job));
    assert!(s.shows("listening on 3000\n"));

    assert!(s.shows("Write the parser"));
    assert!(s.shows("◐"));
    assert!(s.shows("Compacted 2 turns"));
    assert!(s.shows("hook"));
    assert!(!s.shows("raw data"));
}

#[test]
fn decisions_leave_a_slot_for_the_app_card() {
    let mut s = Session::new();
    let turn = s.turn();
    let call = s.insert(turn, ElementKind::Call, &[("tool", "bash".into())], "");
    let decision = s.insert(
        call,
        ElementKind::Decision,
        &[("kind", "approval".into())],
        "rm -rf target",
    );
    assert_eq!(
        s.rows().last().map(|r| (r.0, r.1)),
        Some((ItemKind::Decision, decision))
    );
    assert!(s.shows(&format!("card for {decision}")));
    assert!(!s.shows("rm -rf target"));
}

#[test]
fn clicking_a_user_message_asks_for_a_rewind_to_it() {
    let mut s = Session::new();
    let turn = s.turn();
    let prompt = s.user(turn, "fix the build");
    let clicked = s.click("fix the build");
    assert_eq!(clicked, vec![Message::Rewind(prompt)]);
    assert_eq!(s.send(Message::Rewind(prompt)), Some(Event::Rewind(prompt)));
}

// Incremental updates must land on the same rows a fresh load produces.

#[test]
fn incremental_rows_match_a_fresh_load_after_mixed_patches() {
    let mut s = Session::new();
    let t1 = s.turn();
    s.user(t1, "one");
    let r1 = s.call(t1, "read", 0, 1, "{}", "a");
    s.call(t1, "read", 0, 1, "{}", "b");
    let t2 = s.turn();
    s.user(t2, "two");
    let reply = s.insert(
        t2,
        ElementKind::Message,
        &[("role", "assistant".into())],
        "hi",
    );
    s.insert(
        s.session,
        ElementKind::Digest,
        &[("source_seq", Value::Int(1))],
        "digest",
    );
    // Inserts that are not at the end of the document.
    s.commit(Draft::insert_at(
        t1,
        1,
        ElementKind::Call,
        [("tool", Value::from("read"))],
        "",
    ));
    s.insert(t2, ElementKind::Call, &[("tool", "bash".into())], "");
    s.insert(t2, ElementKind::Call, &[("tool", "bash".into())], "");
    let t3 = s.turn();
    s.user(t3, "three");
    s.commit(Draft::set(r1, "tool", "edit"));
    s.commit(Draft::set(reply, "kind", "thinking"));
    s.commit(Draft::remove(t2));
    s.insert(t3, ElementKind::Todo, &[], "later");

    let fresh = State::from_dom(&s.dom, &Theme::Dark);
    assert_eq!(s.chat.items, fresh.items);
    for (index, row) in s.chat.items.iter().enumerate() {
        assert_eq!(s.chat.pos[&row.key], index);
    }
    assert_eq!(s.chat.heights.len(), s.chat.items.len());
    let total: f64 = (0..fresh.heights.len())
        .map(|i| f64::from(fresh.heights.get(i).unwrap()))
        .sum();
    assert!((s.chat.heights.total() - total).abs() < 1e-3);
}

// #57 Virtualized variable height transcript

fn long_session(turns: usize) -> Session {
    let mut s = Session::new();
    for i in 0..turns {
        let turn = s.turn();
        s.user(turn, &format!("message {i}"));
    }
    s
}

#[test]
fn a_huge_transcript_builds_only_the_rows_around_the_viewport() {
    let mut s = long_session(10_000);
    assert_eq!(s.chat.items.len(), 10_000);

    // Following: the newest rows are built, the oldest are not.
    let window = s.chat.window();
    assert!(window.len() < 100, "built {} rows", window.len());
    assert!(s.shows("message 9999"));
    assert!(!s.shows("message 0"));
    assert!(!s.shows("message 5000"));

    // Scrolled to the middle: rows there are built, the ends are not.
    let middle = s.chat.heights.prefix(5_000) as f32;
    let content = s.chat.heights.total() as f32;
    s.send(Message::Scrolled(ScrollMetrics {
        offset: content - 700.0,
        viewport: 700.0,
        content,
    }));
    s.send(Message::Scrolled(ScrollMetrics {
        offset: middle,
        viewport: 700.0,
        content,
    }));
    assert!(!s.chat.is_following());
    let window = s.chat.window();
    assert!(window.contains(&5_000), "{window:?}");
    assert!(window.len() < 100, "built {} rows", window.len());
    assert!(s.shows("message 5000"));
    assert!(!s.shows("message 9999"));
    assert!(!s.shows("message 0"));

    // A token appended far below changes one row, not the window.
    let last = s.chat.items[9_999].key;
    s.commit(Draft::append(last, " more"));
    assert_eq!(s.chat.window(), window);
}

#[test]
fn measured_heights_replace_estimates_and_size_the_spacers() {
    let mut s = long_session(50);
    let key = s.chat.items[10].key;
    let before = s.chat.heights.prefix(11);
    let estimate = f64::from(s.chat.heights.get(10).unwrap());
    s.send(Message::Measured { key, height: 300.0 });
    assert!((s.chat.heights.prefix(11) - (before - estimate + 300.0)).abs() < 1e-3);
    assert_eq!(s.chat.heights.get(10), Some(300.0));
    // The measurement survives the turn's rows being rebuilt.
    let turn = s.chat.items[10].top;
    s.insert(turn, ElementKind::Ext, &[("harness", "omp".into())], "");
    s.commit(Draft::insert_at(
        turn,
        0,
        ElementKind::Ext,
        [("harness", Value::from("omp"))],
        "",
    ));
    assert_eq!(s.chat.heights.get(s.chat.pos[&key]), Some(300.0));
}

#[test]
fn the_harness_command_list_is_no_transcript_row() {
    let mut s = Session::new();
    let list = [("harness", Value::from("omp")), ("type", Value::from(COMMANDS_EXT))];
    s.insert(s.session, ElementKind::Ext, &list, r#"{"commands":[]}"#);
    assert!(s.chat.items.is_empty());
    let notice = [("harness", Value::from("omp")), ("type", Value::from("notice"))];
    s.insert(s.session, ElementKind::Ext, &notice, "{}");
    assert_eq!(s.chat.items.len(), 1, "other harness events stay rows");
}

#[test]
fn a_row_growing_above_the_viewport_keeps_the_reading_position() {
    let mut s = long_session(200);
    let content = s.chat.heights.total() as f32;
    s.send(Message::Scrolled(ScrollMetrics {
        offset: content - 500.0,
        viewport: 500.0,
        content,
    }));
    let reading = s.chat.heights.prefix(150) as f32;
    s.send(Message::Scrolled(ScrollMetrics {
        offset: reading,
        viewport: 500.0,
        content,
    }));
    let first_visible = s.chat.heights.find(f64::from(reading)).unwrap();

    let key = s.chat.items[3].key;
    let old = s.chat.heights.get(3).unwrap();
    s.send(Message::Measured {
        key,
        height: old + 120.0,
    });
    assert_eq!(s.chat.scroll.metrics.offset, reading + 120.0);
    assert_eq!(
        s.chat.heights.find(f64::from(s.chat.scroll.metrics.offset)),
        Some(first_visible)
    );
}

// #58 Auto scroll only when already at the bottom

#[test]
fn scrolling_up_pins_the_view_and_returning_to_the_bottom_follows_again() {
    let mut s = long_session(300);
    let content = s.chat.heights.total() as f32;
    let bottom = ScrollMetrics {
        offset: content - 600.0,
        viewport: 600.0,
        content,
    };
    s.send(Message::Scrolled(bottom));
    assert!(s.chat.is_following());
    assert!(!s.shows("Jump to latest ↓"));

    // Content grows under a still viewport: still following.
    s.send(Message::Scrolled(ScrollMetrics {
        content: content + 400.0,
        ..bottom
    }));
    assert!(s.chat.is_following());
    s.send(Message::Scrolled(ScrollMetrics {
        offset: content - 200.0,
        content: content + 400.0,
        ..bottom
    }));
    assert!(s.chat.is_following());

    // The user scrolls up: pinned, with a way back.
    let pinned = ScrollMetrics {
        offset: 1_000.0,
        viewport: 600.0,
        content: content + 400.0,
    };
    s.send(Message::Scrolled(pinned));
    assert!(!s.chat.is_following());
    assert!(s.shows("Jump to latest ↓"));
    let window = s.chat.window();

    // New content arrives: the view stays where the user is.
    let turn = s.turn();
    s.user(turn, "new prompt");
    s.send(Message::Scrolled(ScrollMetrics {
        content: content + 800.0,
        ..pinned
    }));
    assert!(!s.chat.is_following());
    assert_eq!(s.chat.window(), window);
    assert!(!s.shows("new prompt"));

    // Scrolling back down to the end follows again.
    s.send(Message::Scrolled(ScrollMetrics {
        offset: content + 200.0,
        viewport: 600.0,
        content: content + 800.0,
    }));
    assert!(s.chat.is_following());
    assert!(s.shows("new prompt"));
}

#[test]
fn jump_to_latest_resumes_following() {
    let mut s = long_session(300);
    let content = s.chat.heights.total() as f32;
    s.send(Message::Scrolled(ScrollMetrics {
        offset: content - 600.0,
        viewport: 600.0,
        content,
    }));
    s.send(Message::Scrolled(ScrollMetrics {
        offset: 0.0,
        viewport: 600.0,
        content,
    }));
    assert!(!s.chat.is_following());
    assert_eq!(s.click("Jump to latest ↓"), vec![Message::JumpToLatest]);
    s.send(Message::JumpToLatest);
    assert!(s.chat.is_following());
    assert!(s.shows("message 299"));
}
