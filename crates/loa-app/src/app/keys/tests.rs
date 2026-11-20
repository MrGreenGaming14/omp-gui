//! Keyboard control through `App::update`, and keyboard reachability of the
//! app's widgets in a headless 1600x1000 window.

use iced::futures::StreamExt;
use iced::keyboard::key::{NativeCode, Physical};
use iced::keyboard::{self, Key, Location, Modifiers};
use iced::{Settings, Size, event};
use iced_runtime::Action as RuntimeAction;
use iced_test::{Simulator, selector};
use loa_bus::Policy;
use loa_ui_decisions as decisions;

use super::*;
use crate::app::tests::{active, deliver, escape, fake_app, prompt, update};
use crate::area::Area;
use crate::areas::with_areas;

fn press(key: Key, modifiers: Modifiers) -> keyboard::Event {
    keyboard::Event::KeyPressed {
        modified_key: key.clone(),
        key,
        physical_key: Physical::Unidentified(NativeCode::Unidentified),
        location: Location::Standard,
        modifiers,
        text: None,
        repeat: false,
    }
}

fn chord(c: char, modifiers: Modifiers) -> keyboard::Event {
    press(Key::Character(c.to_string().into()), modifiers)
}

fn named(key: Named, modifiers: Modifiers) -> keyboard::Event {
    press(Key::Named(key), modifiers)
}

fn primary() -> Modifiers {
    palette::Platform::current().primary()
}

/// Presses a key the way the app's keyboard subscription delivers it, and
/// runs whatever it starts.
fn key(app: &mut App, event: keyboard::Event) {
    update(app, Message::Keyboard(event));
}

/// Widget operations (focus moves) a task asks the runtime for.
fn widget_operations(task: Task<Message>) -> usize {
    let Some(stream) = iced_runtime::task::into_stream(task) else { return 0 };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(stream.filter(|action| std::future::ready(matches!(action, RuntimeAction::Widget(_)))).count())
}

/// A session with one finished turn, so it stays in the sidebar.
fn session(app: &mut App, text: &str) -> String {
    key(app, chord('n', primary()));
    let id = active(app).expect("Ctrl/Cmd+N started a session").to_owned();
    prompt(app, text);
    id
}

#[test]
fn shortcuts_start_and_switch_sessions_show_tabs_and_open_the_palette() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let first = session(&mut app, "one");
    let second = session(&mut app, "two");
    assert_ne!(first, second);
    let order = app.session_order();
    assert_eq!(order.len(), 2, "both sessions are in the sidebar: {order:?}");
    assert_eq!(active(&app), Some(second.as_str()));

    key(&mut app, named(Named::Tab, Modifiers::CTRL));
    let after_next = active(&app).unwrap().to_owned();
    assert_ne!(after_next, second, "Ctrl+Tab shows the other session");
    key(&mut app, named(Named::Tab, Modifiers::CTRL | Modifiers::SHIFT));
    assert_eq!(active(&app), Some(second.as_str()), "Ctrl+Shift+Tab goes back");

    for (n, id) in order.iter().enumerate() {
        let digit = char::from_digit(n as u32 + 1, 10).unwrap();
        key(&mut app, chord(digit, primary()));
        assert_eq!(active(&app), Some(id.as_str()), "Ctrl/Cmd+{digit}");
    }
    key(&mut app, chord('9', primary()));
    assert!(app.ctx.notice().is_some_and(|notice| notice.contains("no session 9")), "{:?}", app.ctx.notice());

    let tabs: Vec<String> = app.ctx.shell.tabs().iter().map(|tab| tab.id.clone()).collect();
    app.ctx.shell.set_panel_open(false);
    key(&mut app, chord('2', primary() | Modifiers::ALT));
    assert!(app.ctx.shell.panel_open(), "showing a tab opens the panel");
    assert_eq!(app.ctx.shell.active_tab(), Some(tabs[1].as_str()));
    key(&mut app, chord('1', primary() | Modifiers::ALT));
    assert_eq!(app.ctx.shell.active_tab(), Some(tabs[0].as_str()));
    key(&mut app, chord('b', primary()));
    assert!(!app.ctx.shell.panel_open(), "Ctrl/Cmd+B");

    // The palette is modal: app shortcuts wait until it closes.
    key(&mut app, chord('k', primary()));
    assert!(app.palette.is_open());
    let open = app.ctx.sessions.iter().count();
    key(&mut app, chord('n', primary()));
    assert_eq!(app.ctx.sessions.iter().count(), open, "Ctrl/Cmd+N does nothing behind the palette");
    key(&mut app, escape());
    assert!(!app.palette.is_open());

    // Ctrl/Cmd+/ lists the keymap.
    key(&mut app, chord('/', primary()));
    assert!(app.palette.is_open() && app.palette.shortcuts_only());
    let listed: Vec<_> = app.palette.entries().collect();
    assert!(listed.len() > 15, "{}", listed.len());
    assert!(listed.iter().all(|entry| app.palette.hint(entry.command).is_some()), "only rows with keys");
    key(&mut app, escape());
}

#[test]
fn the_palette_lists_every_area_command_and_the_whole_keymap() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    session(&mut app, "hello");
    update(&mut app, Message::Palette(palette::Message::Open));
    let commands = app.palette.commands();
    let has = |id: &str| commands.iter().find(|command| command.id.as_str() == id);

    let ctx = &app.ctx;
    for area in with_areas!(each_area!(app.areas, ref |area, wrap| area.palette_commands(ctx))) {
        for command in area {
            assert!(has(command.id.as_str()).is_some(), "area command {} missing", command.id);
        }
    }

    let keymap = &app.ctx.keymap;
    for id in [
        ids::PALETTE_TOGGLE,
        ids::PANEL_TOGGLE,
        ids::SESSION_NEW,
        ids::SESSION_INTERRUPT,
        ids::SESSION_NEXT,
        ids::SESSION_PREVIOUS,
        ids::PROJECT_TAB_NEXT,
        ids::PROJECT_TAB_PREVIOUS,
        ids::COMPOSER_FOCUS,
        ids::SEARCH_FOCUS,
        ids::KEYMAP_SHOW,
        ids::FOCUS_NEXT,
        ids::FOCUS_PREVIOUS,
        ids::session_goto(1),
    ] {
        let command = has(id.as_str()).unwrap_or_else(|| panic!("{id} missing"));
        assert_eq!(command.shortcut, keymap.chord_for(&id), "{id} shows its chord");
        assert!(command.shortcut.is_some(), "{id} is bound");
    }
    let first_tab = &app.ctx.shell.tabs()[0];
    let open_tab = has(&format!("{TAB_COMMAND}{}", first_tab.id)).unwrap();
    assert_eq!(open_tab.shortcut, keymap.chord_for(&ids::panel_tab(1)), "tab rows show Ctrl/Cmd+Alt+n");

    let reference = |title: &str| {
        commands.iter().find(|command| command.title == title).unwrap_or_else(|| panic!("{title} missing"))
    };
    assert_eq!(reference("Prompt card: pick option 1 to 9").keys.as_deref(), Some("1–9"));
    assert!(!reference("Prompt card: confirm the pick").enabled, "reference rows don't run");
    for title in [
        "Close the palette or a screen, dismiss the focused prompt",
        "Composer: send",
        "Composer: new line",
        "Composer: undo, redo",
        "Palette: move, run, close",
        "Show right panel tab 1 to 9",
    ] {
        assert!(reference(title).keys.is_some());
    }

    // Composer pickers and the hover-only pin button are reachable too.
    for title in ["Mode: plan", "Pin session", "Show tools", "Show peers", "Show project tabs"] {
        assert!(commands.iter().any(|command| command.title == title), "{title} missing");
    }
}

#[test]
fn composer_pickers_and_pin_run_from_the_palette() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    key(&mut app, chord('n', primary()));
    let session = app.ctx.sessions.active().expect("started");
    let id = session.id().clone();
    let (_, mut feed) = session.writer().subscribe(Policy::every_patch(1024));
    prompt(&mut app, "hello");
    deliver(&mut app, &id, &mut feed);
    assert!(!app.ctx.sessions.active().unwrap().is_busy(), "the turn ended");
    let id = id.to_string();
    let task = app.command(&CommandId::new(format!("{MODE}plan")));
    crate::app::tests::run(&mut app, task);
    // The fake harness can't be limited to read-only tools, so the sessions
    // area refuses: proof the command took the composer picker's path.
    let notice = app.ctx.notice().unwrap_or_default();
    assert!(notice.contains("Can't enter plan mode on fake-a"), "{notice}");
    let _ = app.command(&PIN);
    assert_eq!(app.ctx.shell.sidebar().session(&id).map(|summary| summary.pinned), Some(true));
    assert_eq!(app.session_order().first(), Some(&id), "pinned sessions come first");
}

#[test]
fn focus_shortcuts_move_the_keyboard_focus() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let task = app.update(Message::Keyboard(chord('l', primary())));
    assert_eq!(widget_operations(task), 0);
    assert!(app.ctx.notice().is_some_and(|notice| notice.contains("Start or open a session")));

    session(&mut app, "hello");
    for event in [
        chord('l', primary()),
        chord('f', primary() | Modifiers::SHIFT),
        named(Named::Tab, Modifiers::empty()),
        named(Named::Tab, Modifiers::SHIFT),
    ] {
        let task = app.update(Message::Keyboard(event.clone()));
        assert_eq!(widget_operations(task), 1, "{event:?} moves the focus");
    }
}

#[test]
fn keys_reach_the_app_while_the_composer_has_focus() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    session(&mut app, "hello");
    let mut ui = Simulator::with_size(Settings::default(), Size::new(1600.0, 1000.0), app.view());
    // Focus targets carry stable ids.
    for id in [composer::EDITOR_ID, shell::sidebar::SEARCH_ID] {
        assert!(ui.find(selector::id(id)).is_ok(), "{id} is on screen");
    }
    ui.click(selector::id(composer::EDITOR_ID)).unwrap();
    assert!(ui.find(selector::is_focused()).is_ok(), "the composer took the focus");

    // App shortcuts pass the focused editor uncaptured, so the keyboard
    // subscription (which only sees ignored events) gets them.
    for event in [
        chord('n', primary()),
        chord('k', primary()),
        chord('l', primary()),
        chord('/', primary()),
        chord('1', primary()),
        chord('1', primary() | Modifiers::ALT),
        named(Named::Tab, Modifiers::CTRL),
        named(Named::Tab, Modifiers::empty()),
    ] {
        let status = ui.simulate([iced::Event::Keyboard(event.clone())]);
        assert_eq!(status, [event::Status::Ignored], "{event:?} reaches the app");
    }
    assert_eq!(ui.typewrite("x"), event::Status::Captured, "typing stays in the composer");
}

#[test]
fn decision_keys_answer_and_the_card_names_this_node() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    update(&mut app, Message::NewSession);
    let id = app.ctx.sessions.active().unwrap().id().clone();
    let (_, mut feed) = app.ctx.sessions.active().unwrap().writer().subscribe(Policy::every_patch(1024));
    prompt(&mut app, "ask to run ls");
    deliver(&mut app, &id, &mut feed);
    assert!(app.areas.decisions.focused().is_some());

    // Number keys and Enter, mapped as the decisions subscription maps them.
    for event in [chord('1', Modifiers::empty()), named(Named::Enter, Modifiers::empty())] {
        let shortcut = decisions::shortcut(&event).expect("a card key");
        let message = areas::decisions::Message::Desk(decisions::desk::Message::Shortcut(shortcut));
        update(&mut app, Message::Decisions(message));
    }
    deliver(&mut app, &id, &mut feed);
    assert!(app.areas.decisions.focused().is_none(), "answered");
    assert_eq!(decisions::shortcut(&chord('1', primary())), None, "Ctrl/Cmd+1 switches sessions instead");

    let mut ui = Simulator::with_size(Settings::default(), Size::new(1600.0, 1000.0), app.view());
    let key = &app.ctx.services.node.key;
    let label = ui.find(|candidate: selector::Candidate<'_>| match candidate {
        selector::Candidate::Text { content, .. } if content.contains(" by ") => Some(content.to_owned()),
        _ => None,
    });
    let label = label.expect("the card says who answered");
    assert_eq!(label, "Allowed by testhost");
    assert!(!label.contains(key.as_str()));
}
