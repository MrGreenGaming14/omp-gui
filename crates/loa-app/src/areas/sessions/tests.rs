use std::sync::Arc;

use iced::keyboard::{Key, Modifiers};
use loa_bus::Listing;
use loa_core::{Dom, Draft, ElementKind, JobKind, JobStatus, NodeId, PatchSink, Role, TurnStatus, Value, attr};
use loa_view::session::transcript;

use crate::app::Message;
use crate::app::windows::Detached;
use crate::testing::{FakeHarness, TestApp};

fn subagent_dom(id: &str, text: &str) -> Dom {
    let mut dom = Dom::new();
    let root = dom
        .commit(
            Draft::insert(
                NodeId::DOCUMENT,
                ElementKind::Session,
                [(attr::ID, Value::from(id)), (attr::HARNESS, "fake".into()), (attr::NODE, "testhost".into()), (attr::CREATED, 0_i64.into())],
                "",
            ),
            0,
        )
        .unwrap()
        .node;
    let turn = dom.commit(Draft::insert(root, ElementKind::Turn, [(attr::STATUS, Value::from(TurnStatus::Done))], ""), 0).unwrap().node;
    dom.commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from(Role::Assistant))], text), 0).unwrap();
    dom
}

fn add_job(test: &TestApp, parent_id: &str, kind: JobKind, attrs: Vec<(&str, Value)>) -> NodeId {
    let session = test.app.ctx.runtime.sessions().get(parent_id).expect("parent session open");
    let writer = Arc::clone(session.writer());
    let root = session.dom().root().expect("session root").id;
    let turn = writer.commit(Draft::insert(root, ElementKind::Turn, [(attr::STATUS, Value::from(TurnStatus::Running))], "")).unwrap().node;
    let mut job_attrs = vec![(attr::KIND, Value::from(kind)), (attr::STATUS, Value::from(JobStatus::Running))];
    job_attrs.extend(attrs);
    writer.commit(Draft::insert(turn, ElementKind::Job, job_attrs, "")).unwrap().node
}

fn view_shows(test: &TestApp, text: &str) -> bool {
    iced_test::Simulator::with_size(iced::Settings::default(), iced::Size::new(1280.0, 800.0), test.app.view()).find(text).is_ok()
}

fn click_produces_no_messages(test: &TestApp, label: &str) -> bool {
    let mut ui = iced_test::Simulator::with_size(iced::Settings::default(), iced::Size::new(1280.0, 800.0), test.app.view());
    match ui.click(label) {
        Ok(_) => ui.into_messages().next().is_none(),
        Err(_) => true,
    }
}

fn stored_session_ids(test: &TestApp) -> Vec<String> {
    test.app
        .ctx
        .runtime
        .library()
        .store()
        .list()
        .unwrap()
        .into_iter()
        .filter_map(|listing| match listing {
            Listing::Session(info) => Some(info.id),
            Listing::Unreadable { .. } => None,
        })
        .collect()
}

#[test]
fn slash_commands_branch_switch_mode_export_and_delete_sessions() {
    let mut test = TestApp::new();
    let first = test.session("hello");
    test.prompt("/fork");
    test.until("the fork is active", |app| app.ctx.runtime.sessions().active_id().is_some_and(|id| id.as_str() != first));
    let fork = test.active().unwrap().to_owned();
    let root = test.app.ctx.runtime.sessions().active().unwrap().dom().root().unwrap().str_attr(loa_core::attr::PARENT_SESSION).map(str::to_owned);
    assert_eq!(root.as_deref(), Some(first.as_str()), "the fork links its parent");

    test.send(Message::OpenRecent(first.clone()));
    let turn = loa_runtime::session::dom_turns(test.app.ctx.runtime.sessions().active().unwrap().dom()).last().expect("a turn").id;
    test.send(Message::area(super::Message::Chat(transcript::Intent::Rewind(turn))));
    test.until("the rewind is active", |app| app.ctx.runtime.sessions().active_id().is_some_and(|id| id.as_str() != fork && id.as_str() != first));
    let title = test.app.ctx.runtime.sessions().active().unwrap().summary().map(|summary| summary.title.clone());
    assert!(title.is_some_and(|title| title.ends_with("(rewound)")), "the rewound branch is named after its source");

    test.prompt("/compact");
    assert!(test.notice().starts_with("Compaction needs"), "no small model in tests: {}", test.notice());
    test.prompt("/plan");
    assert!(test.notice().contains("plan mode"), "a harness without read-only tools refuses: {}", test.notice());
    test.prompt("/model");
    assert!(test.notice().starts_with("Name the model"), "{}", test.notice());
    test.prompt("/export-xml");
    assert!(test.notice().starts_with("Exported to"), "{}", test.notice());

    test.send(Message::area(super::Message::Delete(fork.clone())));
    assert!(test.app.ctx.shell.sidebar().session(&fork).is_none(), "deleted from Recents");
    assert!(test.app.ctx.runtime.library().store().list().unwrap().iter().all(|listing| !format!("{listing:?}").contains(&fork)));
}

#[test]
fn a_compacted_session_takes_over_what_was_typed_in_its_source() {
    let mut test = TestApp::new();
    let paste = |test: &mut TestApp, path: &str| drop(test.app.ctx.composer.attach_image(loa_ui_session::composer::pasted(path.into(), None)));
    let source = test.session("hello");
    test.prompt("/fork");
    test.until("the fork is active", |app| app.ctx.runtime.sessions().active_id().is_some_and(|id| id.as_str() != source));
    let compacted = test.active().unwrap().to_owned();
    test.type_text("typed in the fork");
    paste(&mut test, "fork.png");
    test.send(Message::OpenRecent(source.clone()));
    test.type_text("keep me");
    paste(&mut test, "source.png");

    // Tests have no small model to summarize with: the fork stands in for the compacted session.
    test.area_mut::<super::State>().compacted.insert(compacted.clone(), source.clone());
    test.send(Message::OpenRecent(compacted));
    assert!(test.app.ctx.runtime.sessions().get(&source).is_none(), "the source closed");
    assert_eq!(test.app.ctx.composer.text(), "keep me [Image #2] \n\ntyped in the fork [Image #1] ", "both texts stay, their images numbered apart");
    test.send(Message::Composer(loa_ui_session::composer::Message::Submit));
    test.until("the merged prompt ran", |app| !app.ctx.composer.is_busy());
    assert_eq!(test.harness.seen().prompts.last().map(String::as_str), Some("keep me [Image #2] \n\ntyped in the fork [Image #1]"));
}

#[test]
fn agents_and_jobs_filter_by_kind_and_transcript_avoids_recents_and_runtime_sessions() {
    let mut test = TestApp::with(FakeHarness::new("fake").subagent_messages());
    let file = test.dir.path().join("alpha-child.txt");
    std::fs::write(&file, "first child line\nsecond child line\n").unwrap();
    let parent = test.session("hello");

    add_job(&test, &parent, JobKind::Shell, vec![(attr::COMMAND, "cargo test".into())]);
    let alpha = add_job(
        &test,
        &parent,
        JobKind::Subagent,
        vec![(attr::ID, "alpha".into()), (attr::TITLE, "Alpha".into()), (attr::SESSION, file.display().to_string().into())],
    );
    add_job(&test, &parent, JobKind::Subagent, vec![(attr::ID, "beta".into()), (attr::TITLE, "Beta".into())]);
    test.until("jobs split into Agents and Background Jobs", |app| {
        let s = app.areas.get::<super::State>();
        s.agents.jobs().count() == 2 && s.jobs.jobs().count() == 1
    });

    test.command("tab.jobs");
    let area = test.area::<super::State>();
    let mut jobs = iced_test::Simulator::with_size(iced::Settings::default(), iced::Size::new(1280.0, 800.0), area.jobs.view());
    assert!(jobs.find("cargo test").is_ok());
    assert!(jobs.find("Alpha").is_err());
    assert!(jobs.find("Beta").is_err());
    drop(jobs);

    test.key(Key::Character("a".into()), Modifiers::ALT);
    assert_eq!(test.app.ctx.shell.active_tab(), Some("agents"));
    let area = test.area::<super::State>();
    let mut agents = iced_test::Simulator::with_size(iced::Settings::default(), iced::Size::new(1280.0, 800.0), area.agents.view());
    assert!(agents.find("Alpha").is_ok());
    assert!(agents.find("Beta").is_ok());
    assert!(agents.find("cargo test").is_err());
    drop(agents);
    assert!(view_shows(&test, "Transcript"));

    test.send(Message::area(super::Message::Agents(loa_ui_panel::agents::Message::Detach(alpha))));
    assert!(
        test.app.windows.detached.iter().any(|(_, what)| matches!(what, Detached::Transcript(k) if k == &file.display().to_string())),
        "Transcript opens a detached read-only window; notice: {}", test.notice()
    );
    assert_eq!(test.app.ctx.runtime.sessions().iter().count(), 1, "no runtime session created for child");
    assert_eq!(stored_session_ids(&test), vec![parent.clone()], "no library session created for child");
    assert_eq!(
        test.app.ctx.shell.sidebar().recents().iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec![parent.as_str()],
        "no recents entry added for child"
    );

    let test = test.reopen();
    assert_eq!(
        test.app.ctx.shell.sidebar().recents().iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec![parent.as_str()],
        "child never appears in recents after restart"
    );
}

#[test]
fn subagent_tabs_isolate_alpha_beta_messages_polling_and_errors() {
    let harness = FakeHarness::new("fake")
        .subagent_messages()
        .subagent_transcript("alpha", subagent_dom("alpha", "Alpha initial"), true)
        .subagent_transcript("beta", subagent_dom("beta", "Beta initial"), true);
    let mut test = TestApp::with(harness);
    let parent = test.session("hello");
    let alpha_node = add_job(&test, &parent, JobKind::Subagent, vec![(attr::ID, "alpha".into()), (attr::TITLE, "Alpha".into())]);
    let beta_node = add_job(&test, &parent, JobKind::Subagent, vec![(attr::ID, "beta".into()), (attr::TITLE, "Beta".into())]);
    test.until("subagent jobs registered", |app| app.areas.get::<super::State>().agents.jobs().count() == 2);

    test.key(Key::Character("a".into()), Modifiers::ALT);
    test.send(Message::area(super::Message::Agents(loa_ui_panel::agents::Message::Open(alpha_node))));
    assert_eq!(test.area::<super::State>().subagents.selected_of(&parent), Some("alpha"));
    assert!(view_shows(&test, "Alpha initial"));
    assert!(!view_shows(&test, "Beta initial"));
    assert_eq!(test.harness.seen().subagent_transcripts, vec!["alpha"]);

    test.send(Message::area(super::Message::SubagentDraft(parent.clone(), "alpha".into(), "draft for alpha".into())));
    test.send(Message::area(super::Message::SubagentSelect(None)));
    test.send(Message::area(super::Message::Agents(loa_ui_panel::agents::Message::Open(beta_node))));
    assert_eq!(test.area::<super::State>().subagents.selected_of(&parent), Some("beta"));
    assert!(view_shows(&test, "Beta initial"));
    assert!(!view_shows(&test, "Alpha initial"));
    assert_eq!(test.area::<super::State>().subagents.parents[&parent].children["beta"].draft, "");

    test.send(Message::area(super::Message::SubagentDraft(parent.clone(), "beta".into(), "hello beta".into())));
    test.click("Send");
    assert_eq!(test.harness.seen().subagent_messages, vec![("beta".to_owned(), "hello beta".to_owned())]);
    assert!(view_shows(&test, "hello beta"));
    assert_eq!(test.area::<super::State>().subagents.parents[&parent].children["beta"].draft, "");
    assert_eq!(test.area::<super::State>().subagents.parents[&parent].children["alpha"].draft, "draft for alpha");

    test.send(Message::area(super::Message::SubagentSelect(Some("alpha".into()))));
    assert!(view_shows(&test, "Alpha initial"));
    assert!(!view_shows(&test, "hello beta"));

    let before_tick = test.harness.seen().subagent_transcripts.len();
    test.send(Message::area(super::Message::Tick(1_000)));
    assert_eq!(
        &test.harness.seen().subagent_transcripts[before_tick..],
        &["alpha".to_owned()],
        "1s poll fetches only the selected addressable subagent"
    );

    // Terminal job status triggers a fresh transcript fetch and stops polling once not addressable.
    test.harness.set_subagent_transcript("alpha", subagent_dom("alpha", "Alpha finished"), false);
    let writer = Arc::clone(test.app.ctx.runtime.sessions().get(&parent).unwrap().writer());
    writer.commit(Draft::set(alpha_node, attr::STATUS, JobStatus::Done)).unwrap();
    test.until("terminal status fetches alpha transcript", |app| {
        !app.areas.get::<super::State>().subagents.parents[&parent].children["alpha"].addressable
    });
    assert!(view_shows(&test, "Alpha finished"));
    assert!(view_shows(&test, "alpha has finished, so OMP no longer accepts messages for it. Its conversation stays readable here."));
    let after_terminal = test.harness.seen().subagent_transcripts.len();
    test.send(Message::area(super::Message::Tick(2_000)));
    assert_eq!(test.harness.seen().subagent_transcripts.len(), after_terminal, "non-addressable selected tab is not polled");

    // Failed message_subagent restores draft and shows error.
    test.send(Message::area(super::Message::SubagentSelect(Some("beta".into()))));
    test.harness.set_subagent_error("beta", None, Some("delivery failed".into()));
    test.send(Message::area(super::Message::SubagentDraft(parent.clone(), "beta".into(), "retry beta".into())));
    test.click("Send");
    let error = test.area::<super::State>().subagents.parents[&parent].children["beta"].send_error.as_deref().unwrap();
    assert!(error.contains("delivery failed"));
    assert!(view_shows(&test, error));
    assert_eq!(test.area::<super::State>().subagents.parents[&parent].children["beta"].draft, "retry beta");

    assert_eq!(test.app.ctx.runtime.sessions().iter().count(), 1);
    assert_eq!(stored_session_ids(&test), vec![parent]);
}

#[test]
fn subagent_tab_explains_disabled_and_unsupported_states() {
    // Missing Feature::SubagentMessages capability.
    let mut unsupported = TestApp::with(FakeHarness::new("fake"));
    let p1 = unsupported.session("hello");
    add_job(&unsupported, &p1, JobKind::Subagent, vec![(attr::ID, "alpha".into()), (attr::TITLE, "Alpha".into())]);
    unsupported.until("alpha appears", |app| app.areas.get::<super::State>().agents.jobs().count() == 1);
    unsupported.key(Key::Character("a".into()), Modifiers::ALT);
    unsupported.click("Open");
    assert!(view_shows(&unsupported, "Subagent messaging not supported"));
    assert!(unsupported.harness.seen().subagent_transcripts.is_empty());
    assert!(click_produces_no_messages(&unsupported, "Send"));
    unsupported.send(Message::area(super::Message::SubagentSend(p1, "alpha".into())));
    assert!(unsupported.harness.seen().subagent_messages.is_empty());

    // Absent running harness (e.g. reopened historical session).
    let mut with_cap = TestApp::with(FakeHarness::new("fake").subagent_messages());
    let p2 = with_cap.session("hello");
    add_job(&with_cap, &p2, JobKind::Subagent, vec![(attr::ID, "alpha".into()), (attr::TITLE, "Alpha".into())]);
    with_cap.until("alpha appears", |app| app.areas.get::<super::State>().agents.jobs().count() == 1);

    let mut reopened = with_cap.reopen();
    reopened.send(Message::OpenRecent(p2));
    assert!(reopened.app.ctx.runtime.sessions().active().unwrap().harness().is_none());
    reopened.key(Key::Character("a".into()), Modifiers::ALT);
    reopened.click("Open");
    assert!(view_shows(&reopened, "Parent session is not running"));
    assert!(reopened.harness.seen().subagent_transcripts.is_empty());
    assert!(click_produces_no_messages(&reopened, "Send"));
}

#[test]
fn alt_a_preserves_child_selection_across_tabs_and_sessions_and_cleans_up_on_delete() {
    let harness = FakeHarness::new("fake")
        .subagent_messages()
        .subagent_transcript("alpha", subagent_dom("alpha", "Alpha DOM"), true)
        .subagent_transcript("beta", subagent_dom("beta", "Beta DOM"), true);
    let mut test = TestApp::with(harness);

    let p1 = test.session("one");
    let alpha = add_job(&test, &p1, JobKind::Subagent, vec![(attr::ID, "alpha".into()), (attr::TITLE, "Alpha".into())]);
    let beta = add_job(&test, &p1, JobKind::Subagent, vec![(attr::ID, "beta".into()), (attr::TITLE, "Beta".into())]);
    test.until("two subagents", |app| app.areas.get::<super::State>().agents.jobs().count() == 2);

    test.key(Key::Character("a".into()), Modifiers::ALT);
    test.send(Message::area(super::Message::Agents(loa_ui_panel::agents::Message::Open(alpha))));
    test.send(Message::area(super::Message::SubagentSelect(None)));
    test.send(Message::area(super::Message::Agents(loa_ui_panel::agents::Message::Open(beta))));
    test.send(Message::area(super::Message::SubagentSelect(Some("alpha".into()))));
    assert_eq!(test.area::<super::State>().subagents.selected_of(&p1), Some("alpha"));

    // Switching panel tabs and returning via Alt+A preserves selected child tab.
    test.command("tab.files");
    assert_eq!(test.app.ctx.shell.active_tab(), Some("files"));
    test.key(Key::Character("a".into()), Modifiers::ALT);
    assert_eq!(test.app.ctx.shell.active_tab(), Some("agents"));
    assert_eq!(test.area::<super::State>().subagents.selected_of(&p1), Some("alpha"));

    // Switching to another parent session isolates its tabs and keeps p1's selection intact.
    test.command("session.new");
    test.until("second session active", |app| app.ctx.runtime.sessions().active_id().is_some_and(|id| id.as_str() != p1));
    let p2 = test.active().unwrap().to_owned();
    assert!(!test.area::<super::State>().subagents.parents.contains_key(&p2));
    test.key(Key::Character("a".into()), Modifiers::ALT);
    assert!(view_shows(&test, "No subagents"));

    test.send(Message::OpenRecent(p1.clone()));
    test.key(Key::Character("a".into()), Modifiers::ALT);
    assert_eq!(test.area::<super::State>().subagents.selected_of(&p1), Some("alpha"));
    assert!(view_shows(&test, "Alpha DOM"));

    // Deleting p1 drops all child subtabs for p1.
    test.send(Message::area(super::Message::Delete(p1.clone())));
    assert!(!test.area::<super::State>().subagents.parents.contains_key(&p1));
}

#[test]
fn a_failed_first_read_keeps_polling_and_a_closed_tab_ignores_late_results() {
    let harness = FakeHarness::new("fake").subagent_messages().subagent_transcript("alpha", subagent_dom("alpha", "Alpha live"), true);
    harness.set_subagent_error("alpha", Some("rpc hiccup".into()), None);
    let mut test = TestApp::with(harness);
    let parent = test.session("hello");
    let alpha = add_job(&test, &parent, JobKind::Subagent, vec![(attr::ID, "alpha".into()), (attr::TITLE, "Alpha".into())]);
    test.until("subagent job registered", |app| app.areas.get::<super::State>().agents.jobs().count() == 1);

    // The first read fails, but the job still runs, so the tab keeps polling instead of
    // calling the subagent finished.
    test.send(Message::area(super::Message::Agents(loa_ui_panel::agents::Message::Open(alpha))));
    test.until("the failed read settles", |app| app.areas.get::<super::State>().subagents.parents[&parent].children["alpha"].fetch_error.is_some());
    assert!(!view_shows(&test, "alpha has finished, so OMP no longer accepts messages for it. Its conversation stays readable here."));
    test.harness.set_subagent_error("alpha", None, None);
    test.send(Message::area(super::Message::Tick(1_000)));
    test.until("the next poll reads the transcript", |app| app.areas.get::<super::State>().subagents.parents[&parent].children["alpha"].addressable);
    assert!(view_shows(&test, "Alpha live"));

    // A send result from a tab that was closed never lands on the tab reopened for the same subagent.
    let stale = test.area::<super::State>().subagents.parents[&parent].children["alpha"].generation;
    test.send(Message::area(super::Message::SubagentClose("alpha".into())));
    test.send(Message::area(super::Message::Agents(loa_ui_panel::agents::Message::Open(alpha))));
    test.until("the reopened tab loads", |app| app.areas.get::<super::State>().subagents.parents[&parent].children["alpha"].dom.is_some());
    test.send(Message::area(super::Message::SubagentMessageSent {
        parent: parent.clone(),
        child: "alpha".into(),
        generation: stale,
        text: "old draft".into(),
        result: Err(super::subagents::SendError::NotAddressable("gone".into())),
    }));
    let tab = &test.area::<super::State>().subagents.parents[&parent].children["alpha"];
    assert_ne!(tab.generation, stale);
    assert_eq!((tab.draft.as_str(), tab.addressable, tab.send_error.as_deref()), ("", true, None));
}
