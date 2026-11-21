//! The decisions area driven through `App::update` over the fake harness.

use std::sync::Arc;

use iced::widget::text_editor::{Action, Edit, Motion};
use loa_bus::Policy;
use loa_core::{DecisionStatus, PatchSink};
use loa_ui_session::decisions::{Decision, FieldInput, Shortcut, desk};

use super::*;
use crate::app::tests::{active, deliver, escape, fake_app, prompt, update};
use crate::app::{App, Message as AppMessage};
use crate::session::{Change, Feed};

/// Starts a session and sends `text`, which makes the fake harness wait on
/// a decision. Returns the session, its feed and the waiting card.
fn ask(app: &mut App, text: &str) -> (SessionId, loa_bus::Subscription, NodeId) {
    update(app, AppMessage::NewSession);
    let session = app.ctx.sessions.active().expect("started");
    let id = session.id().clone();
    let (_, mut feed) = session.writer().subscribe(Policy::every_patch(1024));
    prompt(app, text);
    deliver(app, &id, &mut feed);
    let decision = app.areas.decisions.focused().expect("the waiting card takes the keys");
    (id, feed, decision)
}

/// Sends `text` to the active session and returns its new waiting card.
fn ask_again(app: &mut App, id: &SessionId, feed: &mut loa_bus::Subscription, text: &str) -> NodeId {
    prompt(app, text);
    deliver(app, id, feed);
    app.areas.decisions.focused().expect("the waiting card takes the keys")
}

fn desk_message(message: desk::Message) -> AppMessage {
    AppMessage::Decisions(Message::Desk(message))
}

fn card(app: &mut App, id: &SessionId, message: loa_ui_session::decisions::Message) {
    update(app, desk_message(desk::Message::Card(id.to_string(), message)));
}

fn key(app: &mut App, shortcut: Shortcut) {
    update(app, desk_message(desk::Message::Shortcut(shortcut)));
}

/// The decision as the session's harness recorded it.
fn recorded(app: &App, id: &SessionId, decision: NodeId) -> Decision {
    let session = app.ctx.sessions.get(id.as_str()).expect("open");
    session.writer().with_dom(|dom| Decision::from_dom(dom, decision)).expect("a decision")
}

fn turns(app: &App, id: &SessionId) -> usize {
    app.ctx.sessions.get(id.as_str()).expect("open").writer().with_dom(|dom| crate::session::dom_turns(dom).count())
}

/// Opens the palette, types `query` and runs the first match.
fn palette(app: &mut App, query: &str) {
    update(app, AppMessage::Palette(palette::Message::Open));
    update(app, AppMessage::Palette(palette::Message::QueryChanged(query.to_owned())));
    update(app, AppMessage::Palette(palette::Message::Submit));
}

#[test]
fn an_approval_answered_by_number_key_and_enter_runs_and_records_who_answered() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let (id, mut feed, decision) = ask(&mut app, "ask to run ls");
    assert_eq!(app.areas.decisions.desk.badge(id.as_str()), 1);
    assert_eq!(app.ctx.shell.sidebar().session(id.as_str()).map(|s| s.waiting), Some(true), "sidebar badge");
    assert!(app.ctx.shell.topbar().breadcrumb().is_some_and(|crumb| crumb.waiting), "top bar badge");
    assert!(app.areas.decisions.card(id.as_str(), decision).is_some(), "drawn inline");

    key(&mut app, Shortcut::Pick(0));
    assert_eq!(recorded(&app, &id, decision).status, DecisionStatus::Waiting, "a number key only picks");
    key(&mut app, Shortcut::Confirm);
    deliver(&mut app, &id, &mut feed);
    let answered = recorded(&app, &id, decision);
    assert_eq!(answered.answer, [loa_core::answer::ALLOW_ONCE]);
    assert_eq!(answered.by.as_deref(), Some(app.ctx.services.node.key.as_str()), "answered by this node");
    assert!(app.areas.decisions.focused().is_none());
    assert_eq!(app.areas.decisions.desk.badge(id.as_str()), 0);
    assert_eq!(app.ctx.shell.sidebar().session(id.as_str()).map(|s| s.waiting), Some(false));
    assert!(!app.ctx.composer.is_busy(), "the call ran and the turn ended");
}

#[test]
fn a_denied_approval_carries_the_typed_reason() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let (id, mut feed, decision) = ask(&mut app, "ask to run rm");
    card(&mut app, &id, loa_ui_session::decisions::Message::Text(decision, "touches prod".into()));
    card(&mut app, &id, loa_ui_session::decisions::Message::Choose(decision, 2));
    deliver(&mut app, &id, &mut feed);
    let answered = recorded(&app, &id, decision);
    assert_eq!(answered.answer, [loa_core::answer::DENY]);
    assert_eq!(answered.reason.as_deref(), Some("touches prod"));

    let next = ask_again(&mut app, &id, &mut feed, "ask again");
    card(&mut app, &id, loa_ui_session::decisions::Message::Choose(next, 1));
    deliver(&mut app, &id, &mut feed);
    assert_eq!(recorded(&app, &id, next).answer, [loa_core::answer::ALLOW_SESSION]);
}

#[test]
fn a_prompt_typed_while_an_approval_waits_is_queued_and_sent_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let (id, mut feed, decision) = ask(&mut app, "ask to run ls");
    prompt(&mut app, "and then summarize");
    deliver(&mut app, &id, &mut feed);
    assert_eq!(turns(&app, &id), 1, "queued while the turn waits");
    assert!(app.areas.decisions.focused().is_some(), "the card still waits");

    card(&mut app, &id, loa_ui_session::decisions::Message::Choose(decision, 0));
    deliver(&mut app, &id, &mut feed);
    deliver(&mut app, &id, &mut feed);
    assert_eq!(turns(&app, &id), 2, "the queued prompt went out once the turn ended");
    let last = app.ctx.sessions.get(id.as_str()).unwrap().writer().with_dom(|dom| {
        let turn = crate::session::dom_turns(dom).last().unwrap().id;
        dom.children_of_kind(turn, ElementKind::Message).next().map(|m| m.text.clone())
    });
    assert_eq!(last.as_deref(), Some("and then summarize"));
}

#[test]
fn questions_take_an_option_several_picks_or_typed_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let (id, mut feed, q) = ask(&mut app, "ask question");
    card(&mut app, &id, loa_ui_session::decisions::Message::Choose(q, 1));
    deliver(&mut app, &id, &mut feed);
    assert_eq!(recorded(&app, &id, q).answer, ["green"]);

    let multi = ask_again(&mut app, &id, &mut feed, "ask multi");
    key(&mut app, Shortcut::Pick(0));
    key(&mut app, Shortcut::Pick(2));
    card(&mut app, &id, loa_ui_session::decisions::Message::Submit(multi));
    deliver(&mut app, &id, &mut feed);
    assert_eq!(recorded(&app, &id, multi).answer, ["rust", "zig"]);

    let typed = ask_again(&mut app, &id, &mut feed, "ask question");
    card(&mut app, &id, loa_ui_session::decisions::Message::Text(typed, "blue".into()));
    card(&mut app, &id, loa_ui_session::decisions::Message::SubmitText(typed));
    deliver(&mut app, &id, &mut feed);
    assert_eq!(recorded(&app, &id, typed).answer, ["blue"]);
}

#[test]
fn plan_reviews_approve_edit_or_reject_with_feedback() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let (id, mut feed, approve) = ask(&mut app, "ask plan");
    card(&mut app, &id, loa_ui_session::decisions::Message::Choose(approve, 0));
    deliver(&mut app, &id, &mut feed);
    assert_eq!(recorded(&app, &id, approve).answer, [loa_core::answer::APPROVE]);

    let edit = ask_again(&mut app, &id, &mut feed, "ask plan");
    card(&mut app, &id, loa_ui_session::decisions::Message::Choose(edit, 1));
    card(&mut app, &id, loa_ui_session::decisions::Message::EditPlan(edit, Action::Move(Motion::DocumentEnd)));
    card(&mut app, &id, loa_ui_session::decisions::Message::EditPlan(edit, Action::Edit(Edit::Paste(Arc::new("\n3. Add a test".into())))));
    card(&mut app, &id, loa_ui_session::decisions::Message::Submit(edit));
    deliver(&mut app, &id, &mut feed);
    let edited = recorded(&app, &id, edit);
    assert_eq!(edited.answer, [loa_core::answer::EDIT]);
    assert!(edited.reason.as_deref().is_some_and(|plan| plan.ends_with("2. Fix the bug\n3. Add a test")), "{:?}", edited.reason);

    let reject = ask_again(&mut app, &id, &mut feed, "ask plan");
    card(&mut app, &id, loa_ui_session::decisions::Message::Text(reject, "split step 2".into()));
    card(&mut app, &id, loa_ui_session::decisions::Message::Choose(reject, 2));
    deliver(&mut app, &id, &mut feed);
    let rejected = recorded(&app, &id, reject);
    assert_eq!((rejected.answer.as_slice(), rejected.reason.as_deref()), ([loa_core::answer::REJECT.to_owned()].as_slice(), Some("split step 2")));
}

#[test]
fn a_form_is_answered_as_one_json_object() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let (id, mut feed, form) = ask(&mut app, "ask form");
    card(&mut app, &id, loa_ui_session::decisions::Message::Submit(form));
    assert_eq!(recorded(&app, &id, form).status, DecisionStatus::Waiting, "required fields block submit");
    card(&mut app, &id, loa_ui_session::decisions::Message::Field(form, 0, FieldInput::Text("v1.2".into())));
    card(&mut app, &id, loa_ui_session::decisions::Message::Field(form, 1, FieldInput::Choice(1)));
    card(&mut app, &id, loa_ui_session::decisions::Message::Submit(form));
    deliver(&mut app, &id, &mut feed);
    assert_eq!(recorded(&app, &id, form).answer, [r#"{"name":"v1.2","channel":"stable"}"#]);
}

#[test]
fn esc_closes_a_screen_over_the_card_before_it_denies_the_card() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let (id, mut feed, decision) = ask(&mut app, "ask to run ls");
    palette(&mut app, "Unattended prompt settings");
    assert_eq!(app.ctx.screen(), Some(AreaId::Decisions));
    key(&mut app, Shortcut::Pick(0));
    key(&mut app, Shortcut::Confirm);
    update(&mut app, AppMessage::Keyboard(escape()));
    assert_eq!(app.ctx.screen(), None, "Esc closed the screen");
    deliver(&mut app, &id, &mut feed);
    assert_eq!(recorded(&app, &id, decision).status, DecisionStatus::Waiting, "keys don't reach a hidden card");

    update(&mut app, AppMessage::Keyboard(escape()));
    deliver(&mut app, &id, &mut feed);
    assert_eq!(recorded(&app, &id, decision).answer, [loa_core::answer::DENY], "then Esc denies");
}

#[test]
fn new_waiting_cards_raise_a_system_notification_only_while_the_window_is_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    update(&mut app, AppMessage::NewSession);
    let session = app.ctx.sessions.active().unwrap();
    let id = session.id().clone();
    let (_, mut feed) = session.writer().subscribe(Policy::every_patch(1024));
    // The notification tasks each waiting card yields, without running them.
    let notifications = |app: &mut App, feed: &mut loa_bus::Subscription, text: &str| {
        prompt(app, text);
        let mut tasks = 0;
        while let Ok(event) = feed.try_recv() {
            if let Change::Patch(patch, _) = app.ctx.sessions.feed(id.as_str(), Feed::Event(event)) {
                let event = SessionEvent::Patch(id.clone(), patch);
                tasks += app.areas.decisions.on_session_event(&event, &mut app.ctx).units();
            }
        }
        tasks
    };
    assert_eq!(notifications(&mut app, &mut feed, "ask while focused"), 0);
    let first = app.areas.decisions.focused().unwrap();
    update(&mut app, desk_message(desk::Message::Focused(false)));
    let answer = loa_ui_session::decisions::Message::Choose(first, 0);
    update(&mut app, desk_message(desk::Message::Card(id.to_string(), answer)));
    deliver(&mut app, &id, &mut feed);
    assert!(!app.ctx.composer.is_busy(), "the first turn ended");
    assert_eq!(notifications(&mut app, &mut feed, "ask in the background"), 1, "one notification for the new card");
    update(&mut app, desk_message(desk::Message::Focused(true)));
}

#[test]
fn a_plan_review_the_app_raised_is_answered_by_the_sessions_area_not_the_harness() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    update(&mut app, AppMessage::NewSession);
    let session = app.ctx.sessions.active().unwrap();
    let id = session.id().clone();
    let writer = Arc::clone(session.writer());
    let root = session.dom().root().unwrap().id;
    let (_, mut feed) = writer.subscribe(Policy::every_patch(64));
    let attrs: [(&str, loa_core::Value); 2] =
        [("kind", loa_core::DecisionKind::Plan.into()), ("status", DecisionStatus::Waiting.into())];
    let plan = writer.commit(loa_core::Draft::insert(root, ElementKind::Decision, attrs, "# Plan")).unwrap().node;
    deliver(&mut app, &id, &mut feed);
    assert_eq!(app.areas.decisions.focused(), Some(plan));

    key(&mut app, Shortcut::Pick(0));
    key(&mut app, Shortcut::Confirm);
    deliver(&mut app, &id, &mut feed);
    assert_eq!(recorded(&app, &id, plan).status, DecisionStatus::Waiting, "the harness never saw it");
    assert!(app.ctx.notice().is_some_and(|notice| notice.contains("plan")), "the sessions area took it: {:?}", app.ctx.notice());
}

#[test]
fn a_view_only_session_shows_cards_nobody_here_can_answer() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let (id, mut feed, decision) = ask(&mut app, "ask to run ls");
    update(&mut app, AppMessage::Decisions(Message::ViewOnly(id.to_string(), true)));
    assert!(app.areas.decisions.focused().is_none(), "no keys for a card that can't be answered");
    assert!(app.areas.decisions.card(id.as_str(), decision).is_some(), "still drawn");
    card(&mut app, &id, loa_ui_session::decisions::Message::Choose(decision, 0));
    deliver(&mut app, &id, &mut feed);
    assert_eq!(recorded(&app, &id, decision).status, DecisionStatus::Waiting);
    assert_eq!(app.ctx.shell.sidebar().session(id.as_str()).map(|s| s.waiting), Some(true));
}

#[test]
fn unattended_prompts_are_turned_on_in_the_palette_saved_and_answer_after_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    palette(&mut app, "Unattended prompts: deny approvals");
    let saved = app.ctx.services.config.load().unwrap().unattended;
    assert_eq!(saved.approval.as_ref().map(|d| (d.answer.as_str(), d.timeout_secs)), Some((loa_core::answer::DENY, 300)));
    assert_eq!(saved.plan.as_ref().map(|d| d.answer.as_str()), Some(loa_core::answer::REJECT));

    // A shorter timeout on the settings screen.
    palette(&mut app, "Unattended prompt settings");
    assert_eq!(app.ctx.screen(), Some(AreaId::Decisions));
    let _ = app.view();
    let shorter = unattended::Message::Timeout(loa_core::DecisionKind::Approval, unattended::Duration(30));
    update(&mut app, desk_message(desk::Message::Settings(shorter)));
    let saved = app.ctx.services.config.load().unwrap().unattended;
    assert_eq!(saved.approval.map(|d| d.timeout_secs), Some(30));
    update(&mut app, AppMessage::Decisions(Message::CloseSettings));
    assert_eq!(app.ctx.screen(), None);

    let (id, mut feed, decision) = ask(&mut app, "ask while away");
    assert!(app.areas.decisions.desk.subscription().units() > 0);
    update(&mut app, desk_message(desk::Message::Tick(desk::now_ms() + 29_000)));
    deliver(&mut app, &id, &mut feed);
    assert_eq!(recorded(&app, &id, decision).status, DecisionStatus::Waiting, "not yet");
    update(&mut app, desk_message(desk::Message::Tick(desk::now_ms() + 31_000)));
    deliver(&mut app, &id, &mut feed);
    let answered = recorded(&app, &id, decision);
    assert_eq!(answered.answer, [loa_core::answer::DENY]);
    assert_eq!(answered.by.as_deref(), Some(TIMEOUT_BY));
    assert_eq!(answered.reason.as_deref(), Some("Nobody answered within 30 s"));
    assert!(!app.ctx.composer.is_busy());

    // The setting survives a restart, and turns off from the palette.
    drop(app);
    let mut app = fake_app(dir.path(), &["fake-a"]);
    assert!(app.areas.decisions.desk.policy().enabled());
    palette(&mut app, "Unattended prompts: wait for someone");
    assert_eq!(app.ctx.services.config.load().unwrap().unattended, Default::default());
    assert!(active(&app).is_none());
}
