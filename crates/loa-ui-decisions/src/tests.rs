use super::*;
use iced::widget::text_editor::{Action, Edit, Motion};
use iced_test::simulator;
use loa_core::{Draft, ElementKind, Value, answer, attr};
use std::sync::Arc;

const DIFF: &str =
    "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n-pub fn old() {}\n+pub fn new() {}\n";

struct Fixture {
    dom: Dom,
    turn: NodeId,
    call: NodeId,
}

impl Fixture {
    fn new() -> Self {
        let mut dom = Dom::new();
        let root = dom
            .commit(
                Draft::insert(
                    NodeId::DOCUMENT,
                    ElementKind::Session,
                    [
                        (attr::ID, Value::from("s1")),
                        (attr::HARNESS, "omp".into()),
                        (attr::NODE, "node-a".into()),
                        (attr::CREATED, 0i64.into()),
                    ],
                    "",
                ),
                0,
            )
            .unwrap()
            .node;
        let turn = dom
            .commit(
                Draft::insert(
                    root,
                    ElementKind::Turn,
                    [(attr::STATUS, Value::from("waiting"))],
                    "",
                ),
                0,
            )
            .unwrap()
            .node;
        let call = dom
            .commit(
                Draft::insert(
                    turn,
                    ElementKind::Call,
                    [(attr::TOOL, Value::from("edit"))],
                    "",
                ),
                0,
            )
            .unwrap()
            .node;
        Self { dom, turn, call }
    }

    fn decision(
        &mut self,
        parent: NodeId,
        kind: &str,
        mut attrs: Vec<(&str, Value)>,
        body: &str,
    ) -> NodeId {
        attrs.push((attr::KIND, kind.into()));
        attrs.push((attr::STATUS, "waiting".into()));
        self.dom
            .commit(Draft::insert(parent, ElementKind::Decision, attrs, body), 0)
            .unwrap()
            .node
    }

    fn approval(&mut self) -> NodeId {
        let call = self.call;
        self.decision(call, "approval", vec![], DIFF)
    }

    fn question(&mut self, options: &[&str], multi: bool) -> NodeId {
        let turn = self.turn;
        let options = Value::List(options.iter().map(|o| Value::from(*o)).collect());
        self.decision(
            turn,
            "question",
            vec![(attr::OPTIONS, options), (attr::MULTI, multi.into())],
            "Which target?",
        )
    }

    fn set(&mut self, node: NodeId, name: &str, value: impl Into<Value>) {
        self.dom.commit(Draft::set(node, name, value), 0).unwrap();
    }

    fn state(&self, view_only: bool) -> State {
        let mut state = State::new(
            Viewer {
                by: "node-me".into(),
                view_only,
            },
            &Theme::Dark,
        );
        state.sync(&self.dom);
        state
    }
}

fn send(state: &mut State, message: Message) -> Option<Event> {
    state.update(message).1
}

fn answered(event: Option<Event>) -> (NodeId, Vec<String>, Option<String>) {
    match event {
        Some(Event::Answer { decision, answer }) => {
            assert_eq!(answer.by, "node-me");
            (decision, answer.values, answer.reason)
        }
        other => panic!("expected an answer, got {other:?}"),
    }
}

fn press(key: keyboard::Key) -> Message {
    let event = keyboard::Event::KeyPressed {
        modified_key: key.clone(),
        key,
        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::empty(),
        text: None,
        repeat: false,
    };
    Message::Shortcut(shortcut(&event).expect("a card shortcut"))
}

fn digit(n: &str) -> Message {
    press(keyboard::Key::Character(n.into()))
}

fn enter() -> Message {
    press(keyboard::Key::Named(key::Named::Enter))
}

fn esc() -> Message {
    press(keyboard::Key::Named(key::Named::Escape))
}

// #66 Approval card

#[test]
fn approval_buttons_answer_allow_once_and_allow_for_session() {
    let mut fx = Fixture::new();
    let a = fx.approval();
    let b = fx.approval();
    let mut state = fx.state(false);
    assert_eq!(
        answered(send(&mut state, Message::Choose(a, 0))),
        (a, vec![answer::ALLOW_ONCE.into()], None)
    );
    assert_eq!(
        answered(send(&mut state, Message::Choose(b, 1))),
        (b, vec![answer::ALLOW_SESSION.into()], None)
    );
    assert!(
        send(&mut state, Message::Choose(a, 0)).is_none(),
        "an answered card can't answer twice"
    );
}

#[test]
fn deny_sends_the_typed_reason_and_omits_a_blank_one() {
    let mut fx = Fixture::new();
    let a = fx.approval();
    let b = fx.approval();
    let mut state = fx.state(false);
    send(
        &mut state,
        Message::Text(a, "  touches prod config ".into()),
    );
    assert_eq!(
        answered(send(&mut state, Message::SubmitText(a))),
        (
            a,
            vec![answer::DENY.into()],
            Some("touches prod config".into())
        )
    );
    send(&mut state, Message::Text(b, "   ".into()));
    assert_eq!(
        answered(send(&mut state, Message::Choose(b, 2))),
        (b, vec![answer::DENY.into()], None)
    );
}

#[test]
fn approval_card_shows_tool_and_diff_and_clicks_answer() {
    let mut fx = Fixture::new();
    let a = fx.approval();
    let mut state = fx.state(false);
    let mut ui = simulator(state.view());
    ui.find("edit").expect("tool name");
    ui.find("+pub fn new() {}").expect("added line");
    ui.find("-pub fn old() {}").expect("removed line");
    ui.click("Allow for this session").unwrap();
    let messages: Vec<Message> = ui.into_messages().collect();
    let [message] = messages.as_slice() else {
        panic!("one click, one message: {messages:?}")
    };
    assert_eq!(
        answered(send(&mut state, message.clone())),
        (a, vec![answer::ALLOW_SESSION.into()], None)
    );
}

// #67 Question card

#[test]
fn single_choice_question_answers_the_clicked_label() {
    let mut fx = Fixture::new();
    let q = fx.question(&["staging", "production"], false);
    let mut state = fx.state(false);
    let mut ui = simulator(state.view());
    ui.find("Which target?").expect("question text");
    ui.click("production").unwrap();
    let message = ui.into_messages().next().expect("click message");
    assert_eq!(
        answered(send(&mut state, message)),
        (q, vec!["production".into()], None)
    );
}

#[test]
fn multi_choice_question_answers_every_ticked_option_in_order() {
    let mut fx = Fixture::new();
    let q = fx.question(&["lint", "test", "bench", "docs"], true);
    let mut state = fx.state(false);
    // docs on, lint on, bench on, bench off, test on.
    for i in [3, 0, 2, 2, 1] {
        assert!(
            send(&mut state, Message::Choose(q, i)).is_none(),
            "ticking does not submit"
        );
    }
    assert_eq!(
        answered(send(&mut state, Message::Submit(q))),
        (q, vec!["lint".into(), "test".into(), "docs".into()], None)
    );
}

#[test]
fn question_takes_a_typed_answer_but_not_an_empty_one() {
    let mut fx = Fixture::new();
    let q = fx.question(&["yes", "no"], false);
    let mut state = fx.state(false);
    send(&mut state, Message::Text(q, "  ".into()));
    assert!(send(&mut state, Message::SubmitText(q)).is_none());
    send(&mut state, Message::Text(q, "only on Fridays".into()));
    assert_eq!(
        answered(send(&mut state, Message::SubmitText(q))),
        (q, vec!["only on Fridays".into()], None)
    );
}

// #68 Plan review card

const PLAN: &str = "# Plan\n\n- step 1\n- step 2";

#[test]
fn plan_approve_and_reject_with_feedback() {
    let mut fx = Fixture::new();
    let turn = fx.turn;
    let p1 = fx.decision(turn, "plan", vec![], PLAN);
    let p2 = fx.decision(turn, "plan", vec![], PLAN);
    let mut state = fx.state(false);
    assert_eq!(
        answered(send(&mut state, Message::Choose(p1, 0))),
        (p1, vec![answer::APPROVE.into()], None)
    );
    send(&mut state, Message::Text(p2, "split step 2".into()));
    assert_eq!(
        answered(send(&mut state, Message::Choose(p2, 2))),
        (p2, vec![answer::REJECT.into()], Some("split step 2".into()))
    );
}

#[test]
fn plan_edit_returns_the_edited_text() {
    let mut fx = Fixture::new();
    let turn = fx.turn;
    let p = fx.decision(turn, "plan", vec![], PLAN);
    let mut state = fx.state(false);
    assert!(
        send(&mut state, Message::Choose(p, 1)).is_none(),
        "edit opens the editor"
    );
    {
        let mut ui = simulator(state.view());
        assert!(
            ui.find("Approve").is_err(),
            "the review buttons give way to the editor"
        );
        ui.click("Submit edited plan").unwrap();
        assert!(matches!(ui.into_messages().next(), Some(Message::Submit(id)) if id == p));
    }
    send(
        &mut state,
        Message::EditPlan(p, Action::Move(Motion::DocumentEnd)),
    );
    send(
        &mut state,
        Message::EditPlan(p, Action::Edit(Edit::Paste(Arc::new("\n- step 3".into())))),
    );
    assert!(
        send(&mut state, esc()).is_none(),
        "Esc leaves the editor instead of rejecting"
    );
    assert!(send(&mut state, Message::Choose(p, 1)).is_none());
    send(
        &mut state,
        Message::EditPlan(p, Action::Move(Motion::DocumentEnd)),
    );
    send(
        &mut state,
        Message::EditPlan(p, Action::Edit(Edit::Paste(Arc::new("\n- step 3".into())))),
    );
    assert_eq!(
        answered(send(&mut state, Message::Submit(p))),
        (
            p,
            vec![answer::EDIT.into()],
            Some(format!("{PLAN}\n- step 3"))
        )
    );
}

// #69 Form card

const FIELDS: &str = r#"{"type":"object","properties":{
    "name":{"type":"string","description":"Release name"},
    "count":{"type":"integer","default":2},
    "dry_run":{"type":"boolean","default":true},
    "channel":{"enum":["beta","stable"]}
},"required":["name","channel"]}"#;

#[test]
fn form_is_validated_before_submit_and_answers_one_json_object() {
    let mut fx = Fixture::new();
    let turn = fx.turn;
    let f = fx.decision(
        turn,
        "form",
        vec![(attr::FIELDS, FIELDS.into())],
        "Release settings",
    );
    let mut state = fx.state(false);
    send(
        &mut state,
        Message::Field(f, 1, FieldInput::Text("two".into())),
    );
    assert!(
        send(&mut state, Message::Submit(f)).is_none(),
        "missing and malformed fields block submit"
    );
    {
        let mut ui = simulator(state.view());
        ui.find("Required").expect("required error shown");
        ui.find("Enter a whole number")
            .expect("integer error shown");
        ui.find("Release name").expect("field description shown");
    }
    send(
        &mut state,
        Message::Field(f, 0, FieldInput::Text("v1.2".into())),
    );
    send(
        &mut state,
        Message::Field(f, 1, FieldInput::Text("3".into())),
    );
    send(&mut state, Message::Field(f, 3, FieldInput::Choice(1)));
    assert_eq!(
        answered(send(&mut state, Message::Submit(f))),
        (
            f,
            vec![r#"{"name":"v1.2","count":3,"dry_run":true,"channel":"stable"}"#.into()],
            None
        )
    );
}

/// The shape the omp adapter gives an `ask` with several questions.
const ASK_FIELDS: &str = r#"{"type":"object","properties":{
    "color":{"title":"Which color?","description":"Theme","type":"string","enum":["red","green"],"default":"green"},
    "langs":{"title":"Which languages?","type":"array","items":{"type":"string","enum":["rust","go","zig"]}},
    "note":{"title":"Anything else?","type":"string"}
},"required":["color","langs","note"]}"#;

#[test]
fn multi_select_form_fields_tick_options_and_answer_arrays() {
    let mut fx = Fixture::new();
    let turn = fx.turn;
    let f = fx.decision(turn, "form", vec![(attr::FIELDS, ASK_FIELDS.into())], "");
    let mut state = fx.state(false);
    {
        let mut ui = simulator(state.view());
        ui.find("Which languages? *").expect("multi field label");
        ui.click("zig").expect("an option checkbox");
    }
    send(&mut state, Message::Field(f, 1, FieldInput::Toggle(2)));
    send(&mut state, Message::Field(f, 1, FieldInput::Toggle(0)));
    send(&mut state, Message::Field(f, 2, FieldInput::Text("no".into())));
    send(&mut state, Message::Field(f, 1, FieldInput::Toggle(2)));
    send(&mut state, Message::Field(f, 1, FieldInput::Toggle(2)));
    assert_eq!(
        answered(send(&mut state, Message::Submit(f))),
        (f, vec![r#"{"color":"green","langs":["rust","zig"],"note":"no"}"#.into()], None)
    );
}

// #70 Shortcuts

#[test]
fn number_key_then_enter_confirms_and_esc_denies() {
    let mut fx = Fixture::new();
    let a = fx.approval();
    let b = fx.approval();
    let mut state = fx.state(false);
    assert!(
        send(&mut state, enter()).is_none(),
        "Enter with nothing picked does nothing"
    );
    assert!(
        send(&mut state, digit("2")).is_none(),
        "a number key only picks"
    );
    assert_eq!(
        answered(send(&mut state, enter())),
        (a, vec![answer::ALLOW_SESSION.into()], None)
    );
    assert_eq!(
        state.focused(),
        Some(b),
        "focus moves to the next waiting card"
    );
    assert_eq!(
        answered(send(&mut state, esc())),
        (b, vec![answer::DENY.into()], None)
    );
    assert_eq!(state.focused(), None);
}

#[test]
fn only_the_oldest_waiting_card_takes_keys() {
    let mut fx = Fixture::new();
    let q = fx.question(&["a", "b", "c"], false);
    let a = fx.approval();
    let mut state = fx.state(false);
    assert_eq!(state.focused(), Some(q));
    send(&mut state, digit("3"));
    assert_eq!(
        answered(send(&mut state, enter())),
        (q, vec!["c".into()], None)
    );
    assert!(
        send(&mut state, digit("9")).is_none(),
        "out of range numbers are ignored"
    );
    assert!(send(&mut state, enter()).is_none());
    assert!(send(&mut state, digit("1")).is_none());
    assert_eq!(
        answered(send(&mut state, enter())),
        (a, vec![answer::ALLOW_ONCE.into()], None)
    );
}

#[test]
fn esc_skips_questions_and_forms_and_rejects_plans() {
    let mut fx = Fixture::new();
    let turn = fx.turn;
    let q = fx.question(&["a", "b"], true);
    let f = fx.decision(turn, "form", vec![(attr::FIELDS, FIELDS.into())], "");
    let p = fx.decision(turn, "plan", vec![], PLAN);
    let mut state = fx.state(false);
    send(&mut state, digit("1"));
    assert_eq!(answered(send(&mut state, esc())), (q, vec![], None));
    assert_eq!(answered(send(&mut state, esc())), (f, vec![], None));
    assert_eq!(
        answered(send(&mut state, esc())),
        (p, vec![answer::REJECT.into()], None)
    );
}

#[test]
fn keys_typed_into_a_text_field_are_not_shortcuts() {
    let mut fx = Fixture::new();
    let a = fx.approval();
    let state = fx.state(false);
    let mut ui = simulator(state.view());
    assert_eq!(
        ui.tap_key(keyboard::Key::Character("1".into())),
        iced::event::Status::Ignored
    );
    ui.click(text_field_id(a)).unwrap();
    assert_eq!(
        ui.typewrite("1"),
        iced::event::Status::Captured,
        "the field consumes the digit"
    );
    assert_eq!(
        ui.tap_key(key::Named::Enter),
        iced::event::Status::Captured,
        "Enter submits the field"
    );
    assert_eq!(
        ui.tap_key(key::Named::Escape),
        iced::event::Status::Captured,
        "Esc unfocuses the field"
    );
    let messages: Vec<Message> = ui.into_messages().collect();
    assert!(
        matches!(messages.as_slice(), [Message::Text(id, t), Message::SubmitText(id2)] if *id == a && t == "1" && *id2 == a),
        "{messages:?}"
    );
}

#[test]
fn shortcuts_ignore_modified_keys() {
    let event = keyboard::Event::KeyPressed {
        key: keyboard::Key::Character("1".into()),
        modified_key: keyboard::Key::Character("1".into()),
        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::CTRL,
        text: None,
        repeat: false,
    };
    assert_eq!(shortcut(&event), None);
}

// View only peers and read only cards

#[test]
fn view_only_peers_see_a_card_they_cannot_answer() {
    let mut fx = Fixture::new();
    let a = fx.approval();
    let mut state = fx.state(true);
    assert_eq!(state.focused(), None);
    assert!(send(&mut state, Message::Choose(a, 0)).is_none());
    assert!(send(&mut state, Message::Text(a, "x".into())).is_none());
    let mut ui = simulator(state.view());
    ui.find("View only: you can't answer this prompt")
        .expect("view only notice");
    ui.click("Allow once").unwrap();
    assert_eq!(
        ui.into_messages().count(),
        0,
        "disabled buttons publish nothing"
    );
}

#[test]
fn answered_decisions_render_read_only_with_who_answered() {
    let mut fx = Fixture::new();
    let a = fx.approval();
    let q = fx.question(&["x", "y"], false);
    let mut state = fx.state(false);
    answered(send(&mut state, Message::Choose(a, 2)));
    assert_eq!(
        state.focused(),
        Some(q),
        "a card awaiting the DOM keeps no focus"
    );

    fx.set(a, attr::ANSWER, vec![answer::DENY.to_owned()]);
    fx.set(a, attr::BY, TIMEOUT_BY);
    fx.set(a, attr::STATUS, "answered");
    fx.set(q, attr::ANSWER, vec!["y".to_owned()]);
    fx.set(q, attr::BY, "node-bob");
    fx.set(q, attr::STATUS, "answered");
    state.sync(&fx.dom);
    assert_eq!(state.focused(), None);
    assert!(send(&mut state, Message::Choose(q, 0)).is_none());

    let mut ui = simulator(state.view());
    ui.find("Denied by timeout (default answer)")
        .expect("timeout answer");
    ui.find("y by node-bob").expect("peer answer");
    assert!(
        ui.find("Allow once").is_err(),
        "no buttons on a read only card"
    );
}

#[test]
fn a_refused_answer_reopens_the_card() {
    let mut fx = Fixture::new();
    let a = fx.approval();
    let mut state = fx.state(false);
    answered(send(&mut state, Message::Choose(a, 0)));
    assert!(send(&mut state, Message::Choose(a, 0)).is_none());
    state.answer_failed(a);
    assert_eq!(answered(send(&mut state, Message::Choose(a, 0))).0, a);
}
