//! Routing tables (keymap chord → command, command id → owner, `/` name →
//! area) and the app's session lifecycle.

use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};
use loa_ui_shell::palette::{Platform, Scope};

use super::keys::APP_COMMANDS;
use super::*;
use super::lifecycle::turn_status;
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
