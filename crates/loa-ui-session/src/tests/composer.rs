use crate::composer::*;
use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::text_editor::{Action, Edit, Motion};
use loa_ui_kit::testkit;
use loa_view::session::composer::*;
use loa_view::session::{Attachment, Prompt, SessionMode};

fn act(state: &mut State, message: Message) -> Option<Intent> { state.update(message).1 }

/// Types `text` a key at a time; `\n` is Shift+Enter. Returns the events.
fn type_text(state: &mut State, text: &str) -> Vec<Intent> {
    text.chars().filter_map(|c| act(state, Message::Edit(Action::Edit(if c == '\n' { Edit::Enter } else { Edit::Insert(c) })))).collect()
}

fn submitted(event: Option<Intent>) -> Prompt {
    match event {
        Some(Intent::Submit(prompt)) => prompt,
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
        assert_eq!(keys(state, std::slice::from_ref(&event)).first(), expected.as_ref(), "{event:?}");
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
    assert_eq!(act(&mut state, Message::Submit), Some(Intent::QueueChanged(vec!["second".into()])));
    type_text(&mut state, "third");
    act(&mut state, Message::Submit);
    let first = state.queue().next().unwrap().id;
    assert_eq!(act(&mut state, Message::Queue(first, Some(" ".into()))), Some(Intent::QueueChanged(vec![" ".into(), "third".into()])));
    assert_eq!(submitted(state.turn_ended()).text, "third", "blank queued messages are skipped");
    assert_eq!(state.turn_ended(), None);
    // Stop moves queued messages back into the editor, ahead of what is typed.
    state.turn_started();
    type_text(&mut state, "queued");
    act(&mut state, Message::Submit);
    type_text(&mut state, "draft");
    assert_eq!(act(&mut state, Message::Interrupt), Some(Intent::Stop));
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
    assert_eq!(act(&mut State::new(), Message::MentionPressed), Some(Intent::FilesRequested { query: String::new() }));
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
        (PickerId::Harness, "claude", Some(Intent::HarnessChanged("claude".into()))),
        (PickerId::Model, "sonnet", Some(Intent::ModelChanged(model("sonnet")))),
        (PickerId::Model, "all", Some(Intent::ModelAction("all".into()))),
        (PickerId::Effort, "high", Some(Intent::EffortChanged("high".into()))),
        (PickerId::Access, "full", Some(Intent::AccessChanged("full".into()))),
        (PickerId::Mode, "plan", Some(Intent::ModeChanged(SessionMode::Plan))),
        (PickerId::Folder, "/home/b", Some(Intent::FolderPicked("/home/b".into()))),
    ];
    for (picker, key, expected) in cases {
        act(&mut state, Message::Toggle(picker));
        let message = state.pick_key(picker, key).unwrap_or_else(|| panic!("{key} listed"));
        assert_eq!(act(&mut state, message), expected, "{picker:?} {key}");
    }
    assert_eq!((state.harness().unwrap().id.as_str(), state.efforts().1, state.models().1.unwrap().id.as_str()), ("claude", Some("high"), "sonnet"));
    assert_eq!(act(&mut state, Message::Pick(PickerId::Folder, 2)), Some(Intent::BrowseFolders));
    assert_eq!(act(&mut state, Message::Pick(PickerId::Folder, 3)), Some(Intent::OpenWorkspace));
    act(&mut state, Message::Toggle(PickerId::Model));
    for label in ["sonnet", "Claude opus", "anthropic", "All models…", "Plan", "High", "Full", "a"] {
        assert!(iced_test::simulator(state.view()).find(label).is_ok(), "{label}");
    }
}

#[test]
fn pasted_images_become_numbered_tokens_and_are_sent_while_their_token_stays() {
    let mut state = State::new();
    type_text(&mut state, "compare");
    let _ = state.attach_image(pasted("/tmp/one.png".into(), Some((1414, 281))));
    let _ = state.attach_image(pasted("/tmp/two.png".into(), Some((578, 200))));
    assert_eq!(state.text(), "compare [Image #1] [Image #2] ");
    assert_eq!(state.images.iter().filter(|image| image.shown).count(), 2, "both cards show");

    // The × on a card takes its token out of the text too; undo brings both back.
    act(&mut state, Message::RemoveImage(1));
    assert_eq!(state.text(), "compare [Image #2] ");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "compare [Image #1] [Image #2] ");
    assert_eq!(state.images.iter().filter(|image| image.shown).count(), 2, "the card is back");
    act(&mut state, Message::RemoveImage(1));
    let prompt = submitted(act(&mut state, Message::Submit));
    assert_eq!(prompt.text, "compare [Image #2]");
    assert_eq!(prompt.attachments, vec![Attachment { path: "/tmp/two.png".into(), mime: Some("image/png".into()) }]);

    // The next message numbers its images from 1 again.
    let _ = state.attach_image(pasted("/tmp/three.png".into(), None));
    assert_eq!(state.text(), "[Image #1] ");
}

#[test]
fn a_cursor_just_before_a_mention_opens_no_popup() {
    let mut state = State::new();
    type_text(&mut state, "see @");
    act(&mut state, Message::Edit(Action::Move(Motion::Left)));
    assert_eq!(state.popup_kind(), None);
}

#[test]
fn leaving_a_running_session_brings_its_queue_back_and_a_failed_send_returns_the_text() {
    let mut state = State::new();
    type_text(&mut state, "first");
    let _ = submitted(act(&mut state, Message::Submit));
    state.turn_started();
    type_text(&mut state, "queued");
    assert!(matches!(act(&mut state, Message::Submit), Some(Intent::QueueChanged(_))));

    // Another, idle session is shown: nothing stays queued for the old one.
    state.follow_session(false);
    assert!(!state.is_busy());
    assert_eq!(state.text(), "queued");
    // Showing a running session makes the composer queue again.
    state.follow_session(true);
    assert!(state.is_busy());
    state.follow_session(false);

    // A send that could not start a session gives its text back.
    let prompt = submitted(act(&mut state, Message::Submit));
    assert!(state.is_busy(), "starting");
    state.follow_session(false);
    assert!(state.is_busy(), "a send on its way is left alone");
    let _ = state.give_back(&prompt.text);
    assert!(!state.is_busy());
    assert_eq!(state.text(), "queued");
}

#[test]
fn a_magic_keyword_starts_the_shimmer_and_removing_it_stops_it() {
    let mut state = State::new();
    type_text(&mut state, "please workflowz");
    assert!(state.keyword);
    act(&mut state, Message::Shimmer);
    act(&mut state, Message::WindowFocused(false));
    assert!(!state.focused, "an unfocused window stops the shimmer");
    for _ in 0.."workflowz".len() {
        act(&mut state, Message::Edit(Action::Edit(Edit::Backspace)));
    }
    assert!(!state.keyword);
    assert_eq!(state.shimmer, 0);
}

#[test]
fn the_project_chip_picks_a_project_or_none() {
    let mut state = State::new();
    state.set_projects(vec!["alpha".into(), "beta".into()], None);
    act(&mut state, Message::Toggle(PickerId::Project));
    assert_eq!(state.pickers.open, Some(PickerId::Project));
    let beta = state.pick_key(PickerId::Project, "beta").expect("listed");
    assert_eq!(act(&mut state, beta.clone()), Some(Intent::ProjectPicked(Some("beta".into()))));
    assert_eq!(state.pickers.open, None, "picking closes the menu");
    assert_eq!(act(&mut state, beta), None, "the same project again changes nothing");
    assert_eq!(act(&mut state, Message::Pick(PickerId::Project, 0)), Some(Intent::ProjectPicked(None)));
    let _ = state.view();
}

#[test]
fn the_effort_slider_applies_its_level_when_let_go() {
    let mut state = State::new();
    state.set_efforts(vec!["low".into(), "medium".into(), "high".into()], Some("low".into()));
    act(&mut state, Message::Toggle(PickerId::Effort));
    assert_eq!(act(&mut state, Message::EffortSlid(2)), None, "dragging only previews");
    assert_eq!(act(&mut state, Message::EffortReleased), Some(Intent::EffortChanged("high".into())));
    assert_eq!(state.pickers.open, Some(PickerId::Effort), "the card stays open");
    act(&mut state, Message::EffortSlid(2));
    assert_eq!(act(&mut state, Message::EffortReleased), None, "the same level again changes nothing");
    let _ = state.view();
}

#[test]
fn a_taken_draft_empties_the_composer_and_comes_back_whole() {
    let mut state = State::new();
    type_text(&mut state, "see");
    let _ = state.mention_path("src/a.rs");
    let _ = state.attach_image(pasted("one.png".into(), None));
    act(&mut state, Message::Edit(Action::Move(Motion::Left)));
    let cursor = state.cursor();

    let draft = state.take_draft();
    assert!(!draft.is_empty());
    assert_eq!((state.text(), state.images.len()), (String::new(), 0));
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "", "undo can't bring the taken text back");
    assert!(state.take_draft().is_empty());

    type_text(&mut state, "other");
    state.restore_draft(draft);
    assert_eq!((state.text(), state.cursor()), ("see @src/a.rs [Image #1] ".to_owned(), cursor));
    assert_eq!(state.images.iter().filter(|image| image.shown).count(), 1, "the card is back");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "see @src/a.rs [Image #1] ", "undo can't reach the text typed before the restore");

    act(&mut state, Message::Edit(Action::Move(Motion::DocumentEnd)));
    let _ = state.attach_image(pasted("two.png".into(), None));
    let prompt = submitted(act(&mut state, Message::Submit));
    assert_eq!(prompt.text, "see @src/a.rs [Image #1] [Image #2]", "the image counter is back too");
    let paths: Vec<_> = prompt.attachments.iter().map(|attachment| attachment.path.clone()).collect();
    assert_eq!(paths, ["src/a.rs", "one.png", "two.png"].map(std::path::PathBuf::from), "the mention and both images are sent");
}

#[test]
fn a_draft_taken_while_messages_wait_leaves_them_their_images_and_mentions() {
    let mut state = State::new();
    type_text(&mut state, "first");
    let _ = submitted(act(&mut state, Message::Submit));
    let _ = state.mention_path("src/a.rs");
    let _ = state.attach_image(pasted("one.png".into(), None));
    assert!(matches!(act(&mut state, Message::Submit), Some(Intent::QueueChanged(_))), "queued while the send starts");
    type_text(&mut state, "draft");

    let draft = state.take_draft();
    let mut other = State::new();
    let _ = other.attach_image(pasted("two.png".into(), None));
    state.restore_draft(other.take_draft());
    assert_eq!(state.text(), "[Image #2] ", "a restored image is numbered on from the queued one");
    state.turn_started();
    let queued = submitted(state.turn_ended());
    assert_eq!(queued.text, "@src/a.rs [Image #1]");
    let paths: Vec<_> = queued.attachments.iter().map(|attachment| attachment.path.clone()).collect();
    assert_eq!(paths, ["src/a.rs", "one.png"].map(std::path::PathBuf::from), "the queued message is sent with its mention and image");
    assert_eq!(state.turn_ended(), None);
    assert_eq!(submitted(act(&mut state, Message::Submit)).attachments, [Attachment { path: "two.png".into(), mime: Some("image/png".into()) }]);
    state.restore_draft(draft);
    assert_eq!(state.text(), "draft");
}

/// A composer holding `text` with an image `path` pasted after it, and a mention of `mention`.
fn drafted(text: &str, path: &str, mention: &str) -> State {
    let mut state = State::new();
    type_text(&mut state, text);
    let _ = state.attach_image(pasted(path.into(), None));
    let _ = state.mention_path(mention);
    state
}

fn attached(prompt: &Prompt) -> Vec<std::path::PathBuf> { prompt.attachments.iter().map(|attachment| attachment.path.clone()).collect() }

#[test]
fn a_draft_put_ahead_of_another_keeps_both_texts_and_images_apart() {
    let mut editor = drafted("now", "b.png", "b.rs");
    let cursor = editor.cursor();
    editor.prepend_draft(drafted("before", "a.png", "a.rs").take_draft());
    assert_eq!(editor.text(), "before [Image #2] @a.rs \n\nnow [Image #1] @b.rs ");
    assert_eq!(editor.cursor().position.line, cursor.position.line + 2, "the cursor stays in the editor's own text");
    act(&mut editor, Message::Undo);
    assert_eq!(editor.text(), "before [Image #2] @a.rs \n\nnow [Image #1] @b.rs ", "undo starts from the merged text");
    let prompt = submitted(act(&mut editor, Message::Submit));
    assert_eq!(attached(&prompt), ["a.rs", "b.rs", "b.png", "a.png"].map(std::path::PathBuf::from), "every mention and image is sent once");

    let mut stored = drafted("stored", "d.png", "d.rs").take_draft();
    stored.prepend(drafted("carried", "c.png", "c.rs").take_draft());
    let mut shown = State::new();
    shown.restore_draft(stored);
    assert_eq!(shown.text(), "carried [Image #2] @c.rs \n\nstored [Image #1] @d.rs ");
    let _ = shown.attach_image(pasted("e.png".into(), None));
    assert!(shown.text().contains("[Image #3]"), "the counter counts both drafts' images");
    assert_eq!(attached(&submitted(act(&mut shown, Message::Submit))), ["c.rs", "d.rs", "d.png", "c.png", "e.png"].map(std::path::PathBuf::from));
}
