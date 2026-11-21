use crate::decisions::desk::{self, Desk, Expired};
use crate::decisions::unattended::{self, Duration};
use crate::decisions::*;
use iced::Theme;
use iced::keyboard::{self, Modifiers, key::Named};
use iced::widget::text_editor::{Action, Edit, Motion};
use loa_core::testkit::DomFixture;
use loa_core::{Attrs, DecisionKind, Draft, ElementKind, NodeId, Value, answer, attrs};
use loa_view::session::Answer;
use loa_view::session::decisions::{DecisionDefaultView, DeskIntent, Intent, UnattendedView};
use loa_view::session::port::{Notice, Notifier, NotifyError, NotifyFuture};
use std::sync::Arc;

fn list(items: &[&str]) -> Value { Value::List(items.iter().map(|s| Value::from(*s)).collect()) }

/// Drops every notice.
struct NullNotifier;

impl Notifier for NullNotifier {
    fn notify(&self, _: &Notice) -> NotifyFuture { Box::pin(async { Ok(()) }) }
}

/// A session with one decision of `attrs` under a `bash` call.
fn fixture(attrs: Attrs, body: &str) -> (DomFixture, NodeId) {
    let mut f = DomFixture::session();
    f.turn().call("bash", attrs! {});
    let call = f.last;
    let id = f.add(call, ElementKind::Decision, attrs, body);
    (f, id)
}

fn state(f: &DomFixture, view_only: bool) -> State {
    let mut s = State::new(Viewer { by: "me".into(), view_only }, &Theme::Dark);
    s.sync(&f.dom);
    s
}

fn shows(s: &State, id: NodeId, label: &str) -> bool { iced_test::simulator(s.view_card(id).unwrap()).find(label).is_ok() }

#[test]
fn shortcuts_map_digits_enter_and_escape_but_not_chords() {
    let event = |e: iced::Event| match e {
        iced::Event::Keyboard(k) => k,
        _ => unreachable!(),
    };
    let none = Modifiers::empty();
    let cases = [
        (loa_ui_kit::testkit::char("1", none), Some(Shortcut::Pick(0))),
        (loa_ui_kit::testkit::char("9", none), Some(Shortcut::Pick(8))),
        (loa_ui_kit::testkit::char("0", none), None),
        (loa_ui_kit::testkit::char("1", Modifiers::CTRL), None),
        (loa_ui_kit::testkit::named(Named::Enter, none), Some(Shortcut::Confirm)),
        (loa_ui_kit::testkit::named(Named::Escape, none), Some(Shortcut::Dismiss)),
        (loa_ui_kit::testkit::named(Named::Escape, Modifiers::ALT), None),
    ];
    for (e, expected) in cases {
        assert_eq!(shortcut(&event(e.clone())), expected, "{e:?}");
    }
    assert!(matches!(event(loa_ui_kit::testkit::named(Named::Enter, none)), keyboard::Event::KeyPressed { .. }));
}

#[test]
fn every_kind_maps_clicks_keys_and_text_to_answers() {
    use Message as M;
    let form = r#"{"properties":{"tag":{"type":"string"},"env":{"enum":["dev","prod"]}},"required":["tag"]}"#;
    type Step = fn(NodeId) -> Message;
    type Expected<'a> = Option<(Vec<&'a str>, Option<&'a str>)>;
    let cases: Vec<(Attrs, &str, Vec<Step>, Expected)> = vec![
        (attrs! { KIND: "approval" }, "ls", vec![|d| M::Choose(d, 1)], Some((vec![answer::ALLOW_SESSION], None))),
        (attrs! { KIND: "approval" }, "ls", vec![|d| M::Text(d, " prod ".into()), |d| M::Choose(d, 2)], Some((vec![answer::DENY], Some("prod")))),
        (attrs! { KIND: "approval" }, "ls", vec![|_| M::Shortcut(Shortcut::Pick(0)), |_| M::Shortcut(Shortcut::Confirm)], Some((vec![answer::ALLOW_ONCE], None))),
        (attrs! { KIND: "approval" }, "ls", vec![|d| M::Text(d, "  ".into()), |_| M::Shortcut(Shortcut::Dismiss)], Some((vec![answer::DENY], None))),
        (attrs! { KIND: "question", OPTIONS: list(&["red", "blue"]) }, "Color?", vec![|d| M::Choose(d, 1)], Some((vec!["blue"], None))),
        (attrs! { KIND: "question", OPTIONS: list(&["a", "b", "c"]), MULTI: true }, "?", vec![|d| M::Choose(d, 2), |_| M::Shortcut(Shortcut::Pick(0)), |d| M::Submit(d)], Some((vec!["a", "c"], None))),
        (attrs! { KIND: "question" }, "?", vec![|d| M::SubmitText(d)], None),
        (attrs! { KIND: "question" }, "?", vec![|d| M::Text(d, "green".into()), |d| M::SubmitText(d)], Some((vec!["green"], None))),
        (attrs! { KIND: "question" }, "?", vec![|_| M::Shortcut(Shortcut::Dismiss)], Some((vec![], None))),
        (attrs! { KIND: "plan" }, "1. Do", vec![|d| M::Choose(d, 0)], Some((vec![answer::APPROVE], None))),
        (attrs! { KIND: "plan" }, "1. Do", vec![|d| M::Text(d, "split".into()), |d| M::Choose(d, 2)], Some((vec![answer::REJECT], Some("split")))),
        (attrs! { KIND: "plan" }, "1. Do", vec![|d| M::Choose(d, 1), |_| M::Shortcut(Shortcut::Dismiss), |d| M::Choose(d, 1)], None),
        (
            attrs! { KIND: "plan" },
            "1. Do",
            vec![|d| M::Choose(d, 1), |d| M::EditPlan(d, Action::Move(Motion::DocumentEnd)), |d| M::EditPlan(d, Action::Edit(Edit::Paste(Arc::new("\n2. Test".into())))), |d| M::Submit(d)],
            Some((vec![answer::EDIT], Some("1. Do\n2. Test"))),
        ),
        (attrs! { KIND: "form", FIELDS: form }, "", vec![|d| M::Submit(d)], None),
        (attrs! { KIND: "form", FIELDS: form }, "", vec![|d| M::Field(d, 0, FieldInput::Text("v1".into())), |d| M::Field(d, 1, FieldInput::Choice(1)), |d| M::Submit(d)], Some((vec![r#"{"tag":"v1","env":"prod"}"#], None))),
    ];
    for (n, (attrs, body, steps, expected)) in cases.into_iter().enumerate() {
        let (f, id) = fixture(attrs, body);
        let mut s = state(&f, false);
        let events: Vec<Intent> = steps.into_iter().filter_map(|step| s.update(step(id)).1).collect();
        let expected: Vec<Intent> = expected.into_iter().map(|(values, reason)| Intent::Answer { decision: id, answer: Answer { values: values.iter().map(|v| v.to_string()).collect(), reason: reason.map(str::to_owned), by: "me".into() } }).collect();
        assert_eq!(events, expected, "case {n}");
    }
}

#[test]
fn cards_lock_while_sending_and_read_back_who_answered() {
    let body = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-pub fn old() {}\n+pub fn new() {}\n";
    let (mut f, id) = fixture(attrs! { KIND: "approval" }, body);
    let mut s = state(&f, false);
    for label in ["omp wants to run", "bash", "+pub fn new() {}", "-pub fn old() {}", "Allow once"] {
        assert!(shows(&s, id, label), "{label}");
    }
    assert!(s.update(Message::Choose(id, 0)).1.is_some());
    assert!(s.update(Message::Choose(id, 0)).1.is_none(), "locked until the DOM records it");
    assert!(shows(&s, id, "Sending answer…"));
    s.answer_failed(id);
    assert_eq!(s.focused(), Some(id));
    let key = "k".repeat(40);
    f.commit(Draft::set(id, "status", "answered")).commit(Draft::set(id, "answer", list(&[answer::DENY]))).commit(Draft::set(id, "by", key.as_str()));
    s.sync(&f.dom);
    assert!(shows(&s, id, "Denied by kkkkkkkk") && s.focused().is_none());
    s.set_local_name(Some("laptop".into()));
    s.set_viewer(Viewer { by: key.clone(), view_only: false });
    assert!(shows(&s, id, "Denied by laptop"));
    let (f, id) = fixture(attrs! { KIND: "approval" }, "ls");
    let mut peer = state(&f, true);
    assert!(peer.update(Message::Choose(id, 0)).1.is_none() && peer.focused().is_none());
    assert!(shows(&peer, id, "View only: you can't answer this prompt"));
    assert_eq!(node_name("timeout", "me", None), TIMEOUT_BY);
}

#[test]
fn the_tracker_counts_notifies_and_fires_each_timeout_once() {
    let mut f = DomFixture::session();
    f.turn();
    let turn = f.last;
    let own = f.add(turn, ElementKind::Decision, attrs! { KIND: "approval", DEFAULT: list(&[answer::DENY]), TIMEOUT_MS: 5_000i64, CREATED: 1_000i64 }, "rm -rf /tmp/x");
    let question = f.add(turn, ElementKind::Decision, attrs! { KIND: "question", OPTIONS: list(&["red", "green"]) }, "Color?");
    f.add(turn, ElementKind::Decision, attrs! { KIND: "plan", DEFAULT: list(&[answer::EDIT]), TIMEOUT_MS: 1i64 }, "# P");
    let mut tracker = Tracker::default();
    assert!(tracker.observe(&f.dom, 4_000).is_empty(), "no notices in the foreground");
    assert_eq!((tracker.badge("s1"), tracker.next_deadline()), (3, Some(6_000)));
    assert!(tracker.tick(5_999).is_empty());
    let fired = tracker.tick(6_000);
    assert_eq!(fired, [Expired { session: "s1".into(), decision: own, answer: Answer { values: vec![answer::DENY.into()], reason: None, by: TIMEOUT_BY.into() } }]);
    assert!(tracker.tick(100_000).is_empty(), "each default fires once; a misfit default never");
    tracker.policy = Unattended(UnattendedView { question: Some(DecisionDefaultView { timeout_secs: 60, answer: "GREEN".into(), reason: None }), ..Default::default() });
    assert_eq!(tracker.next_deadline(), Some(64_000), "a policy set later counts from when the wait began");
    let fired = tracker.tick(64_000);
    assert_eq!((fired.len(), fired[0].decision, fired[0].answer.values.as_slice()), (1, question, ["green".to_owned()].as_slice()));
    assert!(tracker.tick(i64::MAX).is_empty());
    tracker.background = true;
    let call = f.add(turn, ElementKind::Call, attrs! { TOOL: "bash" }, "");
    f.add(call, ElementKind::Decision, attrs! { KIND: "approval" }, "cargo publish\n");
    let notices = tracker.observe(&f.dom, 70_000);
    assert_eq!(notices.iter().map(|n| (n.heading.clone(), n.summary.as_str())).collect::<Vec<_>>(), [("A session needs approval".to_owned(), "bash: cargo publish")]);
    assert!(tracker.observe(&f.dom, 70_001).is_empty());
    f.commit(Draft::set(own, "status", "answered"));
    tracker.observe(&f.dom, 70_002);
    assert_eq!(tracker.total(), 3);
    tracker.forget("s1");
    assert_eq!(tracker.total(), 0);
}

#[test]
fn unattended_policy_answers_per_kind_and_the_desk_applies_it() {
    let mut f = DomFixture::session();
    f.turn();
    let turn = f.last;
    let fields = r#"{"properties":{"env":{"enum":["dev","prod"],"default":"prod"},"n":{"type":"integer"}},"required":["env"]}"#;
    let ids: Vec<NodeId> = [attrs! { KIND: "approval" }, attrs! { KIND: "question", OPTIONS: list(&["red"]) }, attrs! { KIND: "plan" }, attrs! { KIND: "form", FIELDS: fields }].into_iter().map(|a| f.add(turn, ElementKind::Decision, a, "")).collect();
    let decision = |i: usize| Decision::from_dom(&f.dom, ids[i]).unwrap();
    let mut policy = Unattended::default();
    policy.update(unattended::Message::Enabled(true));
    let deny = policy.answer(&decision(0), "t").unwrap();
    assert_eq!((deny.values, deny.reason.as_deref()), (vec![answer::DENY.to_owned()], Some("Nobody answered within 5 min")));
    assert_eq!(policy.answer(&decision(2), "t").unwrap().values, [answer::REJECT]);
    assert!(policy.answer(&decision(1), "t").is_none() && policy.answer(&decision(3), "t").is_none());
    let edits = [(DecisionKind::Approval, answer::ALLOW_SESSION, answer::ALLOW_SESSION), (DecisionKind::Question, "RED", "red"), (DecisionKind::Question, "blue", "blue"), (DecisionKind::Form, r#"{"n":3}"#, r#"{"env":"prod","n":3}"#)];
    for (kind, configured, expected) in edits {
        policy.update(unattended::Message::Answer(kind, Some(configured.into())));
        let i = [DecisionKind::Approval, DecisionKind::Question, DecisionKind::Plan, DecisionKind::Form].iter().position(|k| *k == kind).unwrap();
        assert_eq!(policy.answer(&decision(i), "t").unwrap().values, [expected], "{kind:?}");
    }
    policy.0.plan = Some(DecisionDefaultView { timeout_secs: 5, answer: answer::EDIT.into(), reason: None });
    assert!(policy.answer(&decision(2), "t").is_none(), "hand-edited misfits are not applied");
    assert_eq!((Duration(30).to_string(), Duration(120).to_string(), Duration(90).to_string()), ("30 s".into(), "2 min".into(), "90 s".into()));

    let mut desk = Desk::new("me", &Theme::Dark, UnattendedView::default(), NullNotifier);
    let events = desk.update(desk::Message::Settings(unattended::Message::Answer(DecisionKind::Approval, Some(answer::ALLOW_ONCE.into()))));
    assert!(matches!(events.as_slice(), [DeskIntent::PolicyChanged(p)] if Unattended(p.clone()).timeout_ms(DecisionKind::Approval) == Some(300_000)));
    desk.update(desk::Message::Settings(unattended::Message::Timeout(DecisionKind::Approval, Duration(30))));
    assert!(desk.update(desk::Message::Settings(unattended::Message::Timeout(DecisionKind::Approval, Duration(30)))).is_empty(), "no change");
    desk.sync(&f.dom, 1_000);
    desk.set_active(Some("s1".into()));
    assert_eq!((desk.badge("s1"), desk.focused()), (4, Some(ids[0])));
    assert!(desk.update(desk::Message::Tick(30_999)).is_empty());
    let events = desk.update(desk::Message::Tick(31_000));
    assert_eq!(events, [DeskIntent::Answer { session: "s1".into(), decision: ids[0], answer: Answer { values: vec![answer::ALLOW_ONCE.into()], reason: None, by: TIMEOUT_BY.into() } }]);
    assert_eq!(desk.focused(), Some(ids[1]), "the sent card is locked");
    let events = desk.update(desk::Message::Card("s1".into(), Message::Choose(ids[1], 0)));
    assert!(matches!(events.as_slice(), [DeskIntent::Answer { session, answer, .. }] if session == "s1" && answer.values == ["red"] && answer.by == "me"));
    assert_eq!(desk.update(desk::Message::Notified(Err(NotifyError("no daemon".into())))), [DeskIntent::NotifyFailed("system notification failed: no daemon".into())]);
    let mut ui = iced_test::simulator(desk.settings_view());
    for label in ["Answer waiting prompts after a timeout", "Approvals", "Plan reviews", "Forms"] {
        assert!(ui.find(label).is_ok(), "{label}");
    }
}

#[test]
fn form_schemas_parse_validate_and_answer_typed_json() {
    use form::{FieldError, Form, FormError};
    let mut form = Form::parse(r#"{"properties":{"b":{"type":"string","default":"main"},"r":{"type":"integer"},"x":{"type":"number"},"f":{"type":"boolean"},"t":{"type":"array","items":{"enum":["a","b"]},"default":["b"]},"n":{"type":["string","null"]}},"required":["b","x","t"]}"#).unwrap();
    let cases: &[(usize, form::FieldValue, Option<FieldError>)] = &[(0, form::FieldValue::Text(" ".into()), Some(FieldError::Required)), (1, form::FieldValue::Text("2.5".into()), Some(FieldError::NotInteger)), (2, form::FieldValue::Text("abc".into()), Some(FieldError::NotNumber))];
    for (i, value, error) in cases {
        form.set(*i, value.clone());
        assert_eq!(form.submit(), None);
        assert_eq!(form.errors[*i], *error, "field {i}");
    }
    for (i, v) in [(0, "dev"), (1, "5"), (2, "0.5")] {
        form.set(i, form::FieldValue::Text(v.into()));
    }
    form.toggle(4, 0);
    assert_eq!(form.submit().as_deref(), Some(r#"{"b":"dev","r":5,"x":0.5,"f":false,"t":["a","b"]}"#));
    assert!(matches!(Form::parse("not json"), Err(FormError::Json(_))));
    assert_eq!(Form::parse(r#"{"type":"string"}"#), Err(FormError::NotObject));
    assert_eq!(Form::parse(r#"{"properties":{"t":{"type":"array"}}}"#), Err(FormError::Unsupported { field: "t".into(), ty: "array".into() }));
}
