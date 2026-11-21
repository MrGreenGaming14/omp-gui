use crate::composer::*;
use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::text_editor::{Action, Edit, Motion};
use loa_contracts::harness::{Attachment, Prompt};
use loa_core::SessionMode;
use loa_ui_kit::testkit;

fn act(state: &mut State, message: Message) -> Option<Event> { state.update(message).1 }

/// Types `text` a key at a time; `\n` is Shift+Enter. Returns the events.
fn type_text(state: &mut State, text: &str) -> Vec<Event> {
    text.chars().filter_map(|c| act(state, Message::Edit(Action::Edit(if c == '\n' { Edit::Enter } else { Edit::Insert(c) })))).collect()
}

fn submitted(event: Option<Event>) -> Prompt {
    match event {
        Some(Event::Submit(prompt)) => prompt,
        other => panic!("expected a submit, got {other:?}"),
    }
}

/// The messages real key presses produce in a focused editor.
fn keys(state: &State, events: &[iced::Event]) -> Vec<Message> {
    let mut ui = iced_test::simulator(state.view());
    ui.click(iced_test::selector::id(EDITOR_ID)).expect("editor");
    ui.simulate(events.iter().cloned());
    ui.into_messages().filter(|m| !matches!(m, Message::Edit(Action::Click(_) | Action::Drag(_)))).collect()
}

#[test]
fn key_bindings_follow_the_editor_state() {
    let (none, shift, cmd) = (Modifiers::empty(), Modifiers::SHIFT, Modifiers::COMMAND);
    let enter = |m| testkit::key(Key::Named(Named::Enter), m, Some("\r"));
    let idle = State::new();
    let cases: Vec<(&State, iced::Event, Option<Message>)> = vec![
        (&idle, enter(none), Some(Message::Submit)),
        (&idle, enter(shift), Some(Message::Edit(Action::Edit(Edit::Enter)))),
        (&idle, testkit::char("z", cmd), Some(Message::Undo)),
        (&idle, testkit::char("z", cmd | shift), Some(Message::Redo)),
        (&idle, testkit::char("y", cmd), Some(Message::Redo)),
        (&idle, testkit::char("n", cmd), None),
        (&idle, testkit::named(Named::Escape, none), None),
    ];
    for (state, event, expected) in cases {
        assert_eq!(keys(state, &[event.clone()]).first(), expected.as_ref(), "{event:?}");
    }
    let mut popup = State::new();
    popup.set_commands(vec![Command { name: "review".into(), description: String::new() }]);
    type_text(&mut popup, "/r");
    popup.turn_started();
    let table = [(Named::ArrowDown, Message::PopupStep(1)), (Named::ArrowUp, Message::PopupStep(-1)), (Named::Tab, Message::PopupAccept(None)), (Named::Enter, Message::PopupAccept(None)), (Named::Escape, Message::PopupClose)];
    for (key, expected) in table {
        assert_eq!(keys(&popup, &[testkit::named(key, none)]), [expected], "{key:?}");
    }
    let mut busy = State::new();
    busy.turn_started();
    assert_eq!(keys(&busy, &[testkit::named(Named::Escape, none)]), [Message::Interrupt]);
}

#[test]
fn submit_queue_and_stop() {
    let mut state = State::new();
    state.set_author("node-a");
    assert_eq!(act(&mut state, Message::Submit), None, "blank");
    type_text(&mut state, "  fix the bug\nplease\n");
    let prompt = submitted(act(&mut state, Message::Submit));
    assert_eq!((prompt.text.as_str(), prompt.by.as_str(), state.text()), ("fix the bug\nplease", "node-a", String::new()));
    state.turn_started();
    type_text(&mut state, "second");
    assert_eq!(act(&mut state, Message::Submit), Some(Event::QueueChanged(vec!["second".into()])));
    type_text(&mut state, "third");
    act(&mut state, Message::Submit);
    let first = state.queue().next().unwrap().id;
    assert_eq!(act(&mut state, Message::Queue(first, Some(" ".into()))), Some(Event::QueueChanged(vec![" ".into(), "third".into()])));
    assert_eq!(submitted(state.turn_ended()).text, "third", "blank queued messages are skipped");
    assert_eq!(state.turn_ended(), None);
    // Stop moves queued messages back into the editor, ahead of what is typed.
    state.turn_started();
    type_text(&mut state, "queued");
    act(&mut state, Message::Submit);
    type_text(&mut state, "draft");
    assert_eq!(act(&mut state, Message::Interrupt), Some(Event::Stop));
    assert_eq!(state.text(), "queued\n\ndraft");
    assert_eq!(state.queue().count(), 0);
}

#[test]
fn undo_groups_words_and_deletions() {
    let mut state = State::new();
    type_text(&mut state, "hello world");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "hello ");
    act(&mut state, Message::Redo);
    assert_eq!(state.text(), "hello world");
    for _ in 0..3 {
        act(&mut state, Message::Edit(Action::Edit(Edit::Backspace)));
    }
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "hello world", "a run of deletions undoes at once");
    act(&mut state, Message::Edit(Action::Move(Motion::Home)));
    type_text(&mut state, "x");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "hello world", "moving the cursor ends a group");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "hello ");
}

#[test]
fn popups_complete_commands_and_attach_mentioned_files() {
    let mut state = State::new();
    let command = |name: &str| Command { name: name.into(), description: format!("{name} the session") };
    state.set_commands(vec![command("review"), command("rewind"), command("compact")]);
    type_text(&mut state, "/re");
    assert_eq!((state.popup_kind(), state.popup_items()), (Some(PopupKind::Command), (vec!["review", "rewind"], 0)));
    act(&mut state, Message::PopupStep(1));
    act(&mut state, Message::PopupAccept(None));
    assert_eq!(state.text(), "/rewind ");
    type_text(&mut state, "see @ma");
    assert_eq!(state.popup_kind(), None, "no files yet");
    state.set_files(vec!["src/main.rs".into(), "docs/a b.png".into()]);
    assert_eq!(state.popup_items(), (vec!["src/main.rs"], 0));
    act(&mut state, Message::PopupClose);
    type_text(&mut state, "i");
    assert_eq!(state.popup_kind(), None, "Esc keeps the word's popup closed");
    type_text(&mut state, " @do");
    assert_eq!(type_text(&mut state, "c"), [], "the open popup asks for files once");
    act(&mut state, Message::PopupAccept(Some(0)));
    type_text(&mut state, "and @src/main.rs, @nope");
    let prompt = submitted(act(&mut state, Message::Submit));
    assert_eq!(prompt.text, "/rewind see @mai @\"docs/a b.png\" and @src/main.rs, @nope");
    let attached = [Attachment { path: "docs/a b.png".into(), mime: Some("image/png".into()) }, Attachment { path: "src/main.rs".into(), mime: None }];
    assert_eq!(prompt.attachments, attached);
    assert_eq!(act(&mut State::new(), Message::MentionPressed), Some(Event::FilesRequested { query: String::new() }));
}

#[test]
fn pickers_emit_changes_once_and_skip_unavailable_options() {
    let mut state = State::new();
    let harness = |id: &str, available| HarnessOption { id: id.into(), label: id.into(), available, detail: None };
    state.set_harnesses(vec![harness("omp", true), harness("gone", false), harness("claude", true)], Some("omp"));
    let model = |id: &str| ModelOption { id: id.into(), name: format!("Claude {id}"), provider: "anthropic".into(), role: None };
    state.set_models(vec![model("opus"), model("sonnet")], Some("opus"));
    state.set_model_actions(vec![PickerAction { id: "all".into(), label: "All models…".into() }]);
    state.set_efforts(vec!["low".into(), "high".into()], Some("low".into()));
    state.set_access(vec![AccessOption { id: "ask".into(), label: "Ask".into(), detail: "asks first".into() }, AccessOption { id: "full".into(), label: "Full".into(), detail: "never asks".into() }], Some("ask"));
    let ws = |p: &str| Workspace { path: p.into(), full: format!("/home/{p}"), branch: None };
    state.set_workspace(Some(ws("a")));
    state.set_folders(vec![ws("a"), ws("b")]);
    let cases = [
        (PickerId::Harness, "gone", None),
        (PickerId::Harness, "omp", None),
        (PickerId::Harness, "claude", Some(Event::HarnessChanged("claude".into()))),
        (PickerId::Model, "sonnet", Some(Event::ModelChanged(model("sonnet")))),
        (PickerId::Model, "all", Some(Event::ModelAction("all".into()))),
        (PickerId::Effort, "high", Some(Event::EffortChanged("high".into()))),
        (PickerId::Access, "full", Some(Event::AccessChanged("full".into()))),
        (PickerId::Mode, "plan", Some(Event::ModeChanged(SessionMode::Plan))),
        (PickerId::Folder, "/home/b", Some(Event::FolderPicked("/home/b".into()))),
    ];
    for (picker, key, expected) in cases {
        act(&mut state, Message::Toggle(picker));
        let message = state.pick_key(picker, key).unwrap_or_else(|| panic!("{key} listed"));
        assert_eq!(act(&mut state, message), expected, "{picker:?} {key}");
    }
    assert_eq!((state.harness().unwrap().id.as_str(), state.efforts().1, state.models().1.unwrap().id.as_str()), ("claude", Some("high"), "sonnet"));
    assert_eq!(act(&mut state, Message::Pick(PickerId::Folder, 2)), Some(Event::BrowseFolders));
    assert_eq!(act(&mut state, Message::Pick(PickerId::Folder, 3)), Some(Event::OpenWorkspace));
    act(&mut state, Message::Toggle(PickerId::Model));
    for label in ["sonnet", "Claude opus", "anthropic", "All models…", "Plan", "High", "Full", "a"] {
        assert!(iced_test::simulator(state.view()).find(label).is_ok(), "{label}");
    }
}
