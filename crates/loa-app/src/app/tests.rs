//! Routing tables (keymap chord → command, command id → owner, `/` name →
//! area) and the app's session lifecycle.

use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};
use loa_ui_shell::palette::{Platform, Scope};

use loa_core::{DecisionKind, DecisionStatus, Draft, ElementKind, PatchSink, TurnStatus, Value, attr};
use super::keys::APP_COMMANDS;
use super::*;
use super::lifecycle::{session_running, turn_status};
use crate::area::{Area, AreaMsg, find_command, find_slash};
use crate::areas::{decisions, harness, models, peers, projects, sandbox, sessions, surfaces, tools, updater, workflows};
use crate::testing::TestApp;

/// Whether some table owns command `id`: the app's, else its area's.
fn owned(id: &str) -> bool {
    let areas: [fn(&str) -> bool; 11] = [
        |id| find_command(harness::State::COMMANDS, id).is_some(),
        |id| find_command(sessions::State::COMMANDS, id).is_some(),
        |id| find_command(decisions::State::COMMANDS, id).is_some(),
        |id| find_command(tools::State::COMMANDS, id).is_some(),
        |id| find_command(surfaces::State::COMMANDS, id).is_some(),
        |id| find_command(models::State::COMMANDS, id).is_some(),
        |id| find_command(projects::State::COMMANDS, id).is_some(),
        |id| find_command(peers::State::COMMANDS, id).is_some(),
        |id| find_command(updater::State::COMMANDS, id).is_some(),
        |id| find_command(sandbox::State::COMMANDS, id).is_some(),
        |id| find_command(workflows::State::COMMANDS, id).is_some(),
    ];
    find_command(APP_COMMANDS, id).is_some() || areas.iter().any(|owns| owns(id))
}

#[test]
fn keymap_chords_run_app_commands() {
    let keymap = palette::Keymap::defaults(Platform::Other);
    let char = |c: &str, m| loa_ui_kit::testkit::key(Key::Character(c.into()), m, None);
    let named = |n, m| loa_ui_kit::testkit::named(n, m);
    let keyboard = |event| match event {
        iced::Event::Keyboard(event) => event,
        _ => unreachable!("a key event"),
    };
    let table = [
        (char("k", Modifiers::CTRL), "palette.toggle"),
        (char("b", Modifiers::CTRL), "panel.toggle"),
        (char("a", Modifiers::ALT), "panel.agents"),
        (char("n", Modifiers::CTRL), "session.new"),
        (char("l", Modifiers::CTRL), "composer.focus"),
        (char("/", Modifiers::CTRL), "keymap.show"),
        (char("f", Modifiers::CTRL | Modifiers::SHIFT), "sidebar.search"),
        (char("3", Modifiers::CTRL), "session.goto.3"),
        (char("2", Modifiers::CTRL | Modifiers::ALT), "panel.tab.2"),
        (named(Named::Tab, Modifiers::CTRL), "session.next"),
        (named(Named::Tab, Modifiers::CTRL | Modifiers::SHIFT), "session.previous"),
        (named(Named::PageDown, Modifiers::CTRL), "project.tab.next"),
        (named(Named::Escape, Modifiers::empty()), "session.interrupt"),
    ];
    for (event, id) in table {
        assert_eq!(keymap.resolve(&keyboard(event), Scope::Global).map(palette::CommandId::as_str), Some(id));
        assert!(find_command(APP_COMMANDS, id).is_some(), "{id} has no command");
    }
}

#[test]
fn every_listed_command_has_an_owner_and_the_palette_keeps_its_order() {
    let mut test = TestApp::new();
    test.session("hello");
    test.send(Message::Palette(palette::Message::Open));
    let commands = test.app.palette.commands().to_vec();
    for command in commands.iter().filter(|command| command.enabled) {
        assert!(owned(command.id.as_str()), "nobody runs {}", command.id.as_str());
    }
    let session: Vec<&str> = commands.iter().filter(|c| c.section == "Session").map(|c| c.title.as_str()).collect();
    let expected = ["Pin session", "Import native session…", "Fork session", "Compact session", "Enter plan mode", "Delete session"];
    assert_eq!(&session[..expected.len()], expected);
    for id in ["panel.toggle", "tab.files", "harness.recheck", "sandbox.settings", "surfaces.browser", "models.settings"] {
        test.app.ctx.clear_notice();
        test.command(id);
        assert!(!test.notice().starts_with("Unknown command"), "{id}: {}", test.notice());
    }
    test.command("nobody.here");
    assert_eq!(test.notice(), "Unknown command nobody.here");
}

#[test]
fn slash_names_route_to_their_area_and_list_in_order() {
    let table = [("/new", Some("session.new")), ("/model a/b", None), ("/plan", None), ("/import", None), ("/nope", None)];
    for (text, app) in table {
        assert_eq!(find_slash(APP_COMMANDS, text).map(|(spec, _)| spec.id), app, "{text}");
    }
    let session = [("/model a/b", "sessions.model", "a/b"), ("/compact keep it ", "sessions.compact", "keep it"), ("/role smol", "sessions.role", "smol"), ("/normal", "sessions.mode.normal", "")];
    for (text, id, args) in session {
        assert_eq!(find_slash(sessions::State::COMMANDS, text).map(|(spec, args)| (spec.id, args)), Some((id, args)));
    }
    assert!(find_slash(sessions::State::COMMANDS, "/compacted").is_none() && find_slash(sessions::State::COMMANDS, "compact").is_none());
    let test = TestApp::new();
    let names: Vec<&str> = test.app.ctx.slash.iter().map(|command| command.name.as_str()).collect();
    assert_eq!(names, ["new", "model", "role", "compact", "fork", "plan", "normal", "export", "export-xml", "import"]);
}

#[test]
fn a_message_for_the_wrong_area_is_reported_not_run() {
    let mut test = TestApp::new();
    test.send(Message::Area("tools", AreaMsg::new(harness::Message::Recheck)));
    assert!(test.notice().contains("The tools area can't handle Recheck"), "{}", test.notice());
}

#[test]
fn sessions_run_on_the_harness_stream_their_feed_and_survive_a_restart() {
    let mut test = TestApp::new();
    let id = test.session("hello");
    assert_eq!(test.harness.seen().prompts, ["hello"]);
    assert_eq!(test.app.ctx.shell.sidebar().active(), Some(id.as_str()));
    test.prompt("ask");
    test.until("the approval waits", |app| turn_status(app.ctx.runtime.sessions().active().unwrap().dom()) == Some("Waiting"));
    // Esc dismisses the focused card first, then interrupts the turn.
    test.key(Key::Named(Named::Escape), Modifiers::empty());
    assert_eq!(test.harness.seen().interrupts, 0);
    test.key(Key::Named(Named::Escape), Modifiers::empty());
    assert_eq!(test.harness.seen().interrupts, 1, "Esc interrupts the harness");
    let config = test.harness.seen().configs.remove(0);
    assert_eq!(config.session_id, id);
    assert!(config.host_tools.iter().any(|tool| tool.name == loa_projects::PROJECT_SEARCH), "areas contribute host tools");

    let test = test.reopen();
    let recents: Vec<_> = test.app.ctx.shell.sidebar().recents().iter().map(|summary| summary.id.clone()).collect();
    assert!(recents.contains(&id), "Recents reload the session: {recents:?} {}", test.notice());
}

#[test]
fn every_session_keeps_its_own_unsent_text() {
    let mut test = TestApp::new();
    let first = test.session("hello");
    test.prompt("ask");
    test.until("the approval waits", |app| turn_status(app.ctx.runtime.sessions().active().unwrap().dom()) == Some("Waiting"));
    test.prompt("queued");
    test.type_text("typed");
    assert_eq!(test.app.ctx.composer.queue().count(), 1);

    test.command("session.new");
    test.until("the new session is active", |app| app.ctx.runtime.sessions().active_id().is_some_and(|id| id.as_str() != first));
    let second = test.active().unwrap().to_owned();
    let composer = &test.app.ctx.composer;
    assert_eq!((composer.text(), composer.queue().count(), composer.is_busy()), (String::new(), 0, false), "a new session starts empty");
    test.type_text("second");

    test.send(Message::OpenRecent(first.clone()));
    assert_eq!(test.app.ctx.composer.text(), "queued\n\ntyped", "the queue went back into the first session's own draft");
    assert!(test.app.ctx.composer.is_busy(), "its turn still runs");
    test.send(Message::OpenRecent(second.clone()));
    assert_eq!(test.app.ctx.composer.text(), "second");
    test.send(Message::Composer(composer::Message::Undo));
    assert_eq!(test.app.ctx.composer.text(), "second", "undo can't reach another session's text");

    test.send(Message::area(sessions::Message::Delete(second.clone())));
    assert_eq!((test.active(), test.app.ctx.composer.text()), (None, String::new()), "the center without a session has a draft of its own");
    assert!(!test.app.sessions.drafts.contains_key(&Some(SessionId::from(second.as_str()))), "a deleted session's draft is dropped");
    test.type_text("center");
    test.send(Message::OpenRecent(first.clone()));
    assert_eq!(test.app.ctx.composer.text(), "queued\n\ntyped");
    test.send(Message::area(sessions::Message::Delete(first)));
    assert_eq!(test.app.ctx.composer.text(), "center");
}

#[test]
fn what_is_typed_while_a_session_starts_from_the_center_stays_with_it() {
    let mut test = TestApp::new();
    test.type_text("hello");
    let start = test.update(Message::Composer(composer::Message::Submit));
    assert!(test.app.ctx.composer.is_busy() && test.active().is_none(), "the session is starting");
    test.prompt("queued");
    test.type_text("more");

    test.run(start);
    assert!(test.active().is_some(), "the session runs");
    let composer = &test.app.ctx.composer;
    assert_eq!((composer.text(), composer.queue().count()), ("more".to_owned(), 1), "the started session's composer keeps both");
}

#[test]
fn a_session_opened_while_the_center_starts_one_shows_its_own_draft() {
    let mut test = TestApp::new();
    let first = test.session("hello");
    test.type_text("first notes");
    test.command("session.new");
    test.until("the new session is active", |app| app.ctx.runtime.sessions().active_id().is_some_and(|id| id.as_str() != first));
    let second = test.active().unwrap().to_owned();
    test.send(Message::area(sessions::Message::Delete(second)));
    assert_eq!(test.active(), None, "the center shows");

    test.type_text("center prompt");
    let start = test.update(Message::Composer(composer::Message::Submit));
    assert!(test.app.ctx.runtime.sessions().starting().is_some(), "the session is starting");
    test.send(Message::OpenRecent(first.clone()));
    assert_eq!(test.app.ctx.composer.text(), "first notes", "the opened session shows its own draft");
    test.type_text(" and more");

    test.run(start);
    test.until("the started session is active", |app| app.ctx.runtime.sessions().active_id().is_some_and(|id| id.as_str() != first));
    test.send(Message::OpenRecent(first));
    assert_eq!(test.app.ctx.composer.text(), "first notes and more", "the draft survived the start");
}

#[test]
fn the_working_indicator_follows_the_shown_sessions_turn() {
    let mut test = TestApp::new();
    let indicator = |test: &TestApp| test.app.ctx.composer.indicator().map(str::to_owned);
    test.type_text("hello");
    let start = test.update(Message::Composer(composer::Message::Submit));
    assert_eq!(indicator(&test).as_deref(), Some("Starting session…"));
    test.run(start);
    test.until("the first turn ends", |app| app.ctx.runtime.sessions().active().is_some_and(|session| !session.is_busy()));
    let first = test.active().expect("an active session").to_owned();
    assert_eq!(indicator(&test), None, "an idle session shows none");

    test.prompt("work read Reading the config");
    test.until("the call runs", |app| app.ctx.composer.indicator() == Some("Reading the config…"));
    test.command("session.new");
    test.until("the new session is active", |app| app.ctx.runtime.sessions().active_id().is_some_and(|id| id.as_str() != first));
    assert_eq!(indicator(&test), None, "the shown session is idle");
    test.send(Message::OpenRecent(first));
    assert_eq!(indicator(&test).as_deref(), Some("Reading the config…"), "switching back shows the running call");

    test.key(Key::Named(Named::Escape), Modifiers::empty());
    test.until("the turn is interrupted", |app| !app.ctx.composer.is_busy());
    assert_eq!(indicator(&test), None, "an interrupted turn shows none");
    test.prompt("ask");
    test.until("the approval waits", |app| turn_status(app.ctx.runtime.sessions().active().unwrap().dom()) == Some("Waiting"));
    assert!(test.app.ctx.composer.is_busy() && indicator(&test).is_none(), "the card asks the user instead");
}

#[test]
fn a_turn_left_running_by_a_crash_shows_no_working_indicator() {
    let mut test = TestApp::new();
    test.prompt("work read Reading the config");
    test.until("the call runs", |app| app.ctx.composer.indicator() == Some("Reading the config…"));
    let id = test.active().expect("an active session").to_owned();

    let mut test = test.reopen();
    test.send(Message::OpenRecent(id));
    let session = test.app.ctx.runtime.sessions().active().expect("the session reopened");
    assert_eq!((turn_status(session.dom()), session.harness().is_none()), (Some("Running"), true), "its DOM still calls the turn running");
    let composer = &test.app.ctx.composer;
    assert!(!composer.is_busy() && composer.indicator().is_none(), "no harness runs it");
}

#[test]
fn the_sidebar_tracks_a_background_session_while_it_runs_and_clears_when_it_ends() {
    let mut test = TestApp::new();
    test.prompt("work read Reading the config");
    test.until("the call runs", |app| app.ctx.composer.indicator() == Some("Reading the config…"));
    let first = test.active().expect("first session").to_owned();
    assert!(test.app.ctx.shell.sidebar().is_running(&first));
    assert!(test.app.ctx.shell.sidebar().shows_running(&first));
    assert_eq!(test.app.ctx.shell.session_status(), Some("Running"));

    // Switch to a new idle session: the background session stays running in the sidebar while
    // the top bar tracks the active idle session.
    test.command("session.new");
    test.until("the second session is active", |app| app.ctx.runtime.sessions().active_id().is_some_and(|id| id.as_str() != first));
    let second = test.active().expect("second session").to_owned();
    assert!(test.app.ctx.shell.sidebar().is_running(&first), "background session stays marked running");
    assert!(test.app.ctx.shell.sidebar().shows_running(&first));
    assert!(!test.app.ctx.shell.sidebar().is_running(&second), "idle active session is not running");
    assert_eq!(test.app.ctx.shell.session_status(), None, "top bar follows the idle active session");

    // End the background turn with Error: the sidebar clears without disturbing the active top bar.
    let (writer, root, turn) = {
        let session = test.app.ctx.runtime.sessions().get(&first).expect("first still open");
        let root = session.dom().root().expect("root").id;
        let turn = loa_runtime::session::dom_turns(session.dom()).last().expect("running turn").id;
        (Arc::clone(session.writer()), root, turn)
    };
    writer.commit(Draft::set(turn, attr::STATUS, TurnStatus::Error)).unwrap();
    test.until("the background turn ends", |app| !app.ctx.shell.sidebar().is_running(&first));
    assert!(!test.app.ctx.shell.sidebar().shows_running(&first));
    assert_eq!(test.app.ctx.shell.session_status(), None, "background status never overwrites the active top bar");
    assert_eq!(test.active(), Some(second.as_str()));

    // A background turn starting again marks the sidebar running without changing the active top bar.
    let next_turn = writer.commit(Draft::insert(root, ElementKind::Turn, [(attr::STATUS, Value::from(TurnStatus::Running))], "")).unwrap().node;
    test.until("the background turn starts", |app| app.ctx.shell.sidebar().shows_running(&first));
    assert_eq!(test.app.ctx.shell.session_status(), None, "background running turn leaves active top bar alone");

    // A waiting decision in the background takes precedence over the running indicator.
    writer
        .commit(Draft::insert(
            next_turn,
            ElementKind::Decision,
            [(attr::KIND, Value::from(DecisionKind::Approval)), (attr::STATUS, Value::from(DecisionStatus::Waiting))],
            "",
        ))
        .unwrap();
    writer.commit(Draft::set(next_turn, attr::STATUS, TurnStatus::Waiting)).unwrap();
    test.until("the background decision waits", |app| app.ctx.shell.sidebar().session(&first).is_some_and(|s| s.waiting));
    assert!(test.app.ctx.shell.sidebar().is_running(&first));
    assert!(!test.app.ctx.shell.sidebar().shows_running(&first), "waiting wins over running");
    assert_eq!(test.app.ctx.shell.session_status(), None);

    // Deleting the background session clears its sidebar running state.
    test.send(Message::area(sessions::Message::Delete(first.clone())));
    assert!(!test.app.ctx.shell.sidebar().is_running(&first));
    assert!(!test.app.ctx.shell.sidebar().shows_running(&first));
}

#[test]
fn a_stale_crash_dom_with_no_harness_is_never_running_in_the_sidebar() {
    let mut test = TestApp::new();
    test.prompt("work read Reading the config");
    test.until("the call runs", |app| app.ctx.composer.indicator() == Some("Reading the config…"));
    let id = test.active().expect("an active session").to_owned();
    assert!(test.app.ctx.shell.sidebar().is_running(&id));
    assert!(test.app.ctx.shell.sidebar().shows_running(&id));

    let mut test = test.reopen();
    assert!(test.app.ctx.shell.sidebar().session(&id).is_some(), "reopened recents list the crashed session");
    assert!(!test.app.ctx.shell.sidebar().is_running(&id), "disk recents are never marked running");
    assert!(!test.app.ctx.shell.sidebar().shows_running(&id));

    test.send(Message::OpenRecent(id.clone()));
    let session = test.app.ctx.runtime.sessions().active().expect("the session reopened");
    assert_eq!((session.is_busy(), session.harness().is_none()), (true, true), "stale DOM is busy with no harness");
    assert!(!session_running(test.app.ctx.runtime.sessions(), session.id()), "no harness or start means not running");
    assert!(!test.app.ctx.shell.sidebar().is_running(&id), "reopening a stale crash DOM does not mark it running");
    assert!(!test.app.ctx.shell.sidebar().shows_running(&id));

    // Peer sessions are never in runtime.sessions() and never running.
    assert!(!session_running(test.app.ctx.runtime.sessions(), &SessionId::from("peer:node:remote")));

    // When this busy session is starting/resuming, the helper treats it as running until starting clears.
    let session_id = SessionId::from(id.as_str());
    test.app.ctx.runtime.sessions_mut().set_starting(Some(session_id.clone()));
    assert!(session_running(test.app.ctx.runtime.sessions(), &session_id));
    test.app.ctx.runtime.sessions_mut().set_starting(None);
    assert!(!session_running(test.app.ctx.runtime.sessions(), &session_id));
}

/// A sidebar event, as its row emits it.
fn nav(event: Nav) -> Message {
    Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::Emit(event)))
}

#[test]
fn shell_events_route_to_intents_and_windows() {
    let mut test = TestApp::new();
    let remote = "peer:node/session".to_owned();
    assert!(matches!(route(ShellIntent::Sidebar(Nav::DeleteSession(remote.clone()))), Some(Intent::Notice(_))));
    assert!(matches!(route(ShellIntent::Sidebar(Nav::OpenSession(remote))), Some(Intent::Forward(Message::Area("peers", _)))));
    assert!(matches!(route(ShellIntent::Sidebar(Nav::DeleteSession("s1".into()))), Some(Intent::Forward(Message::Area("sessions", _)))));
    test.send(nav(Nav::OpenSettings));
    assert_eq!(test.app.windows.overlay, Some(Overlay::Settings(SettingsTab::General)));
    test.send(nav(Nav::OpenConfigs));
    assert_eq!(test.app.windows.overlay, Some(Overlay::Configs(ConfigsTab::Models)));
    test.key(Key::Named(Named::Escape), Modifiers::empty());
    assert_eq!(test.app.windows.overlay, None, "Esc closes Configs");
    let browse = test.app.intent(Intent::Browse);
    test.run(browse);
    assert_eq!(test.notice(), "no folder dialog in tests");
}

#[test]
fn usage_reads_its_source_and_spends_a_saved_reset() {
    let mut test = TestApp::new();
    test.send(nav(Nav::OpenUsage));
    assert_eq!(test.app.windows.overlay, Some(Overlay::Usage));
    test.send(Message::Usage(usage::Message::Toggle("anthropic".into())));
    test.click("Use a reset");
    assert_eq!(*test.usage.spent.lock().unwrap(), ["anthropic/c1"]);
    test.send(Message::Usage(usage::Message::Close));
    assert_eq!(test.app.windows.overlay, None);
}

#[test]
fn the_backends_run_the_configured_harness_executables() {
    let bin = tempfile::tempdir().unwrap();
    let omp = bin.path().join("my-omp");
    loa_util::exec::write_executable(&omp, "#!/bin/sh\n").unwrap();
    // On Windows the script lands next to a launcher; the harness runs whichever `resolve` finds.
    let omp = loa_util::exec::resolve(&omp);
    let runtime = Arc::new(tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap());
    let configured = omp.clone();
    let test = TestApp::on(runtime, crate::testing::FakeHarness::new("fake"), "testhost", move |config| {
        config.harness.executables.insert("omp".to_owned(), configured);
    });
    let parts = machine_parts(&test.app.ctx.services, Arc::clone(test.app.ctx.runtime.library()));
    // omp runs the model screens' RPC and the project digests.
    let omp_harness = parts.runtime.harnesses.get("omp").expect("omp is registered");
    assert_eq!(omp_harness.configured.as_deref(), Some(omp.as_path()));
    assert_eq!(omp_harness.executable.as_deref(), Some(omp.as_path()));
}
