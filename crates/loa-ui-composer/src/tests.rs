use super::*;
use iced::keyboard::key::{Named, NativeCode, Physical};
use iced::keyboard::{self, Key, Modifiers};
use iced::widget::text_editor::Motion;
use iced_test::selector::id;
use loa_adapter::Attachment;

fn act(state: &mut State, message: Message) -> Option<Event> {
    state.update(message).1
}

/// Types `text` one key at a time; `\n` is Shift+Enter. Returns the events.
fn type_text(state: &mut State, text: &str) -> Vec<Event> {
    text.chars()
        .filter_map(|c| {
            let edit = if c == '\n' { Edit::Enter } else { Edit::Insert(c) };
            act(state, Message::Edit(Action::Edit(edit)))
        })
        .collect()
}

fn submitted(event: Option<Event>) -> Prompt {
    match event {
        Some(Event::Submit(prompt)) => prompt,
        other => panic!("expected a submit, got {other:?}"),
    }
}

fn column_of(state: &State) -> usize {
    state.cursor().position.column
}

#[test]
fn submit_sends_trimmed_text_and_clears_the_editor() {
    let mut state = State::new();
    state.set_author("node-a");
    type_text(&mut state, "  fix the bug\nplease\n");
    let prompt = submitted(act(&mut state, Message::Submit));
    assert_eq!(prompt.text, "fix the bug\nplease");
    assert_eq!(prompt.by, "node-a");
    assert_eq!(state.text(), "");
    assert!(state.is_busy());
}

#[test]
fn blank_input_does_not_submit() {
    let mut state = State::new();
    assert_eq!(act(&mut state, Message::Submit), None);
    type_text(&mut state, " \n\t ");
    assert_eq!(act(&mut state, Message::Submit), None);
    assert_eq!(act(&mut state, Message::Send), None);
    assert!(!state.is_busy());
}

#[test]
fn undo_and_redo_step_through_words_and_restore_the_cursor() {
    let mut state = State::new();
    type_text(&mut state, "hello world");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "hello ");
    assert_eq!(column_of(&state), 6);
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "");
    assert_eq!(act(&mut state, Message::Undo), None);
    assert_eq!(state.text(), "");

    act(&mut state, Message::Redo);
    assert_eq!(state.text(), "hello ");
    act(&mut state, Message::Redo);
    assert_eq!(state.text(), "hello world");
    assert_eq!(column_of(&state), 11);
    act(&mut state, Message::Redo);
    assert_eq!(state.text(), "hello world");
}

#[test]
fn a_run_of_deletions_undoes_at_once() {
    let mut state = State::new();
    type_text(&mut state, "abc");
    for _ in 0..2 {
        act(&mut state, Message::Edit(Action::Edit(Edit::Backspace)));
    }
    assert_eq!(state.text(), "a");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "abc");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "");
}

#[test]
fn moving_the_cursor_ends_an_undo_group() {
    let mut state = State::new();
    type_text(&mut state, "ab");
    act(&mut state, Message::Edit(Action::Move(Motion::Left)));
    type_text(&mut state, "x");
    assert_eq!(state.text(), "axb");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "ab");
    assert_eq!(column_of(&state), 1);
}

#[test]
fn a_new_edit_after_undo_drops_redo() {
    let mut state = State::new();
    type_text(&mut state, "one two");
    act(&mut state, Message::Undo);
    type_text(&mut state, "three");
    act(&mut state, Message::Redo);
    assert_eq!(state.text(), "one three");
}

#[test]
fn a_new_line_is_its_own_undo_step() {
    let mut state = State::new();
    type_text(&mut state, "ab\ncd");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "ab\n");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "ab");
}

fn commands() -> Vec<Command> {
    ["review", "rewind", "compact"]
        .into_iter()
        .map(|name| Command { name: name.into(), description: format!("{name} the session") })
        .collect()
}

#[test]
fn slash_popup_filters_navigates_and_inserts() {
    let mut state = State::new();
    state.set_commands(commands());
    type_text(&mut state, "/re");
    assert_eq!(state.popup_kind(), Some(PopupKind::Command));
    assert_eq!(state.popup_items(), (vec!["review", "rewind"], 0));

    act(&mut state, Message::PopupNext);
    assert_eq!(state.popup_items().1, 1);
    act(&mut state, Message::PopupNext);
    assert_eq!(state.popup_items().1, 0);
    act(&mut state, Message::PopupPrevious);
    assert_eq!(state.popup_items().1, 1);

    assert_eq!(act(&mut state, Message::PopupAccept), None);
    assert_eq!(state.text(), "/rewind ");
    assert_eq!(column_of(&state), 8);
    assert_eq!(state.popup_kind(), None);

    type_text(&mut state, "now");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "/rewind ");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "/re");
}

#[test]
fn slash_popup_matches_fuzzily_and_hides_without_matches() {
    let mut state = State::new();
    state.set_commands(commands());
    type_text(&mut state, "/cpt");
    assert_eq!(state.popup_items().0, vec!["compact"]);
    type_text(&mut state, "zz");
    assert_eq!(state.popup_kind(), None);
    // With nothing to pick, Enter is a plain submit again.
    assert!(matches!(act(&mut state, Message::Submit), Some(Event::Submit(_))));
}

#[test]
fn slash_opens_only_at_the_start() {
    let mut state = State::new();
    state.set_commands(commands());
    type_text(&mut state, "see /re");
    assert_eq!(state.popup_kind(), None);
}

#[test]
fn esc_closes_the_popup_until_a_new_trigger() {
    let mut state = State::new();
    state.set_commands(commands());
    state.set_files(vec!["src/main.rs".into()]);
    type_text(&mut state, "/re");
    act(&mut state, Message::PopupClose);
    assert_eq!(state.popup_kind(), None);
    type_text(&mut state, "v");
    assert_eq!(state.popup_kind(), None);
    type_text(&mut state, " @");
    assert_eq!(state.popup_kind(), Some(PopupKind::File));
}

#[test]
fn at_popup_requests_files_and_refilters_when_they_arrive() {
    let mut state = State::new();
    let events = type_text(&mut state, "look at @ma");
    assert_eq!(events, vec![Event::FilesRequested { query: String::new() }]);
    assert_eq!(state.popup_kind(), None);

    state.set_files(vec!["README.md".into(), "src/main.rs".into(), "crates/app/src/main.rs".into()]);
    assert_eq!(state.popup_kind(), Some(PopupKind::File));
    assert_eq!(state.popup_items().0, vec!["src/main.rs", "crates/app/src/main.rs"]);

    act(&mut state, Message::PopupNext);
    act(&mut state, Message::PopupAccept);
    assert_eq!(state.text(), "look at @crates/app/src/main.rs ");
    type_text(&mut state, "please");

    let prompt = submitted(act(&mut state, Message::Submit));
    assert_eq!(prompt.text, "look at @crates/app/src/main.rs please");
    assert_eq!(prompt.attachments, vec![Attachment { path: "crates/app/src/main.rs".into(), mime: None }]);
}

#[test]
fn picked_mention_mid_text_replaces_the_whole_word() {
    let mut state = State::new();
    state.set_files(vec!["docs/My Notes.pdf".into()]);
    type_text(&mut state, "read @notes");
    act(&mut state, Message::Edit(Action::Move(Motion::Left)));
    act(&mut state, Message::Edit(Action::Move(Motion::Left)));
    assert_eq!(state.popup_items().0, vec!["docs/My Notes.pdf"]);
    act(&mut state, Message::PopupPick(0));
    assert_eq!(state.text(), "read @\"docs/My Notes.pdf\" ");

    // The file list refreshes without the file; the picked mention still resolves.
    state.set_files(Vec::new());
    let prompt = submitted(act(&mut state, Message::Submit));
    assert_eq!(
        prompt.attachments,
        vec![Attachment { path: "docs/My Notes.pdf".into(), mime: Some("application/pdf".into()) }]
    );
}

#[test]
fn typed_mentions_of_unknown_files_are_not_attached() {
    let mut state = State::new();
    state.set_files(vec!["a.rs".into()]);
    type_text(&mut state, "compare @a.rs and @b.rs");
    let prompt = submitted(act(&mut state, Message::Submit));
    assert_eq!(prompt.attachments, vec![Attachment { path: "a.rs".into(), mime: None }]);
}

#[test]
fn pasted_images_become_numbered_tokens_and_are_sent_while_their_token_stays() {
    let mut state = State::new();
    type_text(&mut state, "compare");
    let _ = state.attach_image("/tmp/one.png".into(), Some((1414, 281)));
    let _ = state.attach_image("/tmp/two.png".into(), Some((578, 200)));
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
    let _ = state.attach_image("/tmp/three.png".into(), None);
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
    assert!(matches!(act(&mut state, Message::Submit), Some(Event::QueueChanged(_))));

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
fn messages_written_mid_turn_queue_and_release_one_per_turn_end() {
    let mut state = State::new();
    type_text(&mut state, "first");
    assert_eq!(submitted(act(&mut state, Message::Submit)).text, "first");

    // Before the turn even starts, a second message already queues.
    type_text(&mut state, "second");
    assert_eq!(act(&mut state, Message::Submit), Some(Event::QueueChanged(vec!["second".into()])));
    state.turn_started();
    type_text(&mut state, "third");
    assert_eq!(
        act(&mut state, Message::Submit),
        Some(Event::QueueChanged(vec!["second".into(), "third".into()]))
    );
    type_text(&mut state, "fourth");
    act(&mut state, Message::Submit);
    assert_eq!(state.text(), "");

    let ids: Vec<u64> = state.queue().map(|m| m.id).collect();
    assert_eq!(
        act(&mut state, Message::QueueEdit(ids[1], "third, edited".into())),
        Some(Event::QueueChanged(vec!["second".into(), "third, edited".into(), "fourth".into()]))
    );
    assert_eq!(
        act(&mut state, Message::QueueRemove(ids[0])),
        Some(Event::QueueChanged(vec!["third, edited".into(), "fourth".into()]))
    );
    assert_eq!(act(&mut state, Message::QueueRemove(ids[0])), None);

    assert_eq!(submitted(state.turn_ended()).text, "third, edited");
    assert!(state.is_busy());
    assert_eq!(state.queue().count(), 1);
    state.turn_started();
    assert_eq!(submitted(state.turn_ended()).text, "fourth");
    state.turn_started();
    assert_eq!(state.turn_ended(), None);
    assert!(!state.is_busy());

    type_text(&mut state, "fifth");
    assert_eq!(submitted(act(&mut state, Message::Submit)).text, "fifth");
}

#[test]
fn queued_message_edited_blank_is_skipped() {
    let mut state = State::new();
    type_text(&mut state, "one");
    act(&mut state, Message::Submit);
    type_text(&mut state, "two");
    act(&mut state, Message::Submit);
    type_text(&mut state, "three");
    act(&mut state, Message::Submit);
    let id = state.queue().next().map(|m| m.id).unwrap();
    act(&mut state, Message::QueueEdit(id, "  ".into()));
    assert_eq!(submitted(state.turn_ended()).text, "three");
}

#[test]
fn stop_interrupts_and_returns_queued_messages_to_the_editor() {
    let mut state = State::new();
    assert_eq!(act(&mut state, Message::Interrupt), None);
    type_text(&mut state, "run");
    act(&mut state, Message::Submit);
    state.turn_started();
    type_text(&mut state, "queued one");
    act(&mut state, Message::Submit);
    type_text(&mut state, "draft");

    assert_eq!(act(&mut state, Message::Interrupt), Some(Event::Stop));
    assert_eq!(state.text(), "queued one\n\ndraft");
    assert_eq!(state.queue().count(), 0);
    assert_eq!(state.turn_ended(), None);
    assert!(!state.is_busy());
    assert_eq!(act(&mut state, Message::StopPressed), None);

    // Enter now sends what came back from the queue.
    assert_eq!(submitted(act(&mut state, Message::Submit)).text, "queued one\n\ndraft");
}

fn models() -> Vec<ModelOption> {
    vec![
        ModelOption {
            id: "anthropic/sonnet".into(),
            name: "Sonnet".into(),
            provider: "anthropic".into(),
            role: Some("default".into()),
        },
        ModelOption { id: "openai/gpt".into(), name: "GPT".into(), provider: "openai".into(), role: None },
    ]
}

#[test]
fn pickers_show_role_and_model_and_emit_changes() {
    let mut state = State::new();
    state.set_models(models(), Some("anthropic/sonnet"));
    assert_eq!(models()[0].to_string(), "default · Sonnet (anthropic)");
    assert_eq!(models()[1].to_string(), "GPT (openai)");

    assert_eq!(act(&mut state, Message::ModelPicked(models()[0].clone())), None);
    assert_eq!(act(&mut state, Message::ModelPicked(models()[1].clone())), Some(Event::ModelChanged(models()[1].clone())));

    state.set_efforts(vec!["low".into(), "high".into()], Some("low".into()));
    assert_eq!(act(&mut state, Message::EffortPicked("low".into())), None);
    assert_eq!(act(&mut state, Message::EffortPicked("high".into())), Some(Event::EffortChanged("high".into())));

    assert_eq!(act(&mut state, Message::ModePicked(SessionMode::Normal)), None);
    assert_eq!(act(&mut state, Message::ModePicked(SessionMode::Plan)), Some(Event::ModeChanged(SessionMode::Plan)));
}

fn editor_height(state: &State) -> f32 {
    let mut ui = iced_test::simulator(state.view());
    ui.find(id(EDITOR_ID)).expect("editor is in the view").bounds().height
}

#[test]
fn editor_grows_with_its_lines_up_to_the_maximum() {
    let mut state = State::new();
    let one = editor_height(&state);
    type_text(&mut state, "a\nb\nc");
    let three = editor_height(&state);
    type_text(&mut state, &"\nline".repeat(60));
    let many = editor_height(&state);
    assert!(one < three, "{one} < {three}");
    assert!(three < many, "{three} < {many}");
    // Past the cap the editor scrolls instead of growing.
    type_text(&mut state, &"\nmore".repeat(60));
    assert_eq!(editor_height(&state), many);
    assert!(many <= MAX_EDITOR_HEIGHT + 16.0, "{many} stays near the cap");
}

fn key_press(key: Key, modifiers: Modifiers, text: Option<&str>) -> iced::Event {
    iced::Event::Keyboard(keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: Physical::Unidentified(NativeCode::Unidentified),
        location: keyboard::Location::Standard,
        modifiers,
        repeat: false,
        text: text.map(Into::into),
    })
}

#[test]
fn real_keys_shift_enter_breaks_the_line_and_enter_sends() {
    let mut state = State::new();
    let messages: Vec<Message> = {
        let mut ui = iced_test::simulator(state.view());
        ui.click(id(EDITOR_ID)).expect("editor is clickable");
        ui.typewrite("hi");
        ui.simulate([key_press(Key::Named(Named::Enter), Modifiers::SHIFT, Some("\r"))]);
        ui.typewrite("there");
        ui.simulate([key_press(Key::Named(Named::Enter), Modifiers::empty(), Some("\r"))]);
        ui.into_messages().collect()
    };
    let events: Vec<Event> = messages.into_iter().filter_map(|m| act(&mut state, m)).collect();
    assert_eq!(events.len(), 1);
    match &events[0] {
        Event::Submit(prompt) => assert_eq!(prompt.text, "hi\nthere"),
        other => panic!("expected a submit, got {other:?}"),
    }
}

fn unavailable(id: &str) -> HarnessOption {
    HarnessOption { id: id.into(), label: id.into(), available: false, detail: Some("not installed".into()) }
}

fn harnesses() -> Vec<HarnessOption> {
    vec![
        HarnessOption { id: "omp".into(), label: "omp".into(), available: true, detail: None },
        unavailable("claude"),
        unavailable("codex"),
    ]
}

#[test]
fn harness_switches_to_available_harnesses_only() {
    let mut state = State::new();
    let mut options = harnesses();
    options.push(HarnessOption { id: "opencode".into(), label: "opencode".into(), available: true, detail: None });
    state.set_harnesses(options, Some("omp"));
    assert_eq!(state.harness().map(|h| h.id.as_str()), Some("omp"));
    assert_eq!(act(&mut state, Message::HarnessPicked("claude".into())), None);
    assert_eq!(act(&mut state, Message::HarnessPicked("missing".into())), None);
    assert_eq!(act(&mut state, Message::HarnessPicked("omp".into())), None);
    assert_eq!(state.harness().map(|h| h.id.as_str()), Some("omp"));
    assert_eq!(
        act(&mut state, Message::HarnessPicked("opencode".into())),
        Some(Event::HarnessChanged("opencode".into()))
    );
    assert_eq!(state.harness().map(|h| h.id.as_str()), Some("opencode"));
}

#[test]
fn picker_menus_toggle_and_close_on_a_choice() {
    let mut state = State::new();
    state.set_models(models(), None);
    act(&mut state, Message::PickerToggled(Picker::Model));
    assert_eq!(state.pickers.open, Some(Picker::Model));
    act(&mut state, Message::PickerToggled(Picker::Effort));
    assert_eq!(state.pickers.open, Some(Picker::Effort));
    act(&mut state, Message::PickerToggled(Picker::Effort));
    assert_eq!(state.pickers.open, None);
    act(&mut state, Message::PickerToggled(Picker::Mode));
    act(&mut state, Message::PickerClosed);
    assert_eq!(state.pickers.open, None);
    act(&mut state, Message::PickerToggled(Picker::Model));
    let model = models()[1].clone();
    assert_eq!(act(&mut state, Message::ModelPicked(model.clone())), Some(Event::ModelChanged(model)));
    assert_eq!(state.pickers.open, None);
}

#[test]
fn model_actions_close_the_menu_and_report_their_id() {
    let mut state = State::new();
    state.set_model_actions(vec![PickerAction { id: "browse".into(), label: "All models…".into() }]);
    act(&mut state, Message::PickerToggled(Picker::Model));
    assert_eq!(state.pickers.open, Some(Picker::Model));
    assert_eq!(act(&mut state, Message::ModelActionPicked("browse".into())), Some(Event::ModelAction("browse".into())));
    assert_eq!(state.pickers.open, None);
}

#[test]
fn plus_starts_a_file_mention_at_the_cursor() {
    let mut state = State::new();
    assert_eq!(act(&mut state, Message::MentionPressed), Some(Event::FilesRequested { query: String::new() }));
    assert_eq!(state.text(), "@");
    state.set_files(vec!["README.md".into()]);
    assert_eq!(state.popup_kind(), Some(PopupKind::File));

    let mut state = State::new();
    type_text(&mut state, "look at");
    assert_eq!(act(&mut state, Message::MentionPressed), Some(Event::FilesRequested { query: String::new() }));
    assert_eq!(state.text(), "look at @");
    act(&mut state, Message::Undo);
    assert_eq!(state.text(), "look at");
}

#[test]
fn picker_labels_are_short_and_capitalized() {
    assert_eq!(short_model_name("Claude Opus 5.5"), "Opus 5.5");
    assert_eq!(short_model_name("GPT-5"), "GPT-5");
    assert_eq!(short_model_name("Claude "), "Claude ");
    assert_eq!(capitalized("medium"), "Medium");
    assert_eq!(capitalized("plan"), "Plan");
    assert_eq!(capitalized(""), "");
}

#[test]
fn the_project_chip_picks_a_project_or_none() {
    let mut state = State::new();
    state.set_projects(vec!["alpha".into(), "beta".into()], None);
    act(&mut state, Message::PickerToggled(Picker::Project));
    assert_eq!(state.pickers.open, Some(Picker::Project));
    assert_eq!(act(&mut state, Message::ProjectPicked(Some("beta".into()))), Some(Event::ProjectPicked(Some("beta".into()))));
    assert_eq!(state.pickers.open, None, "picking closes the menu");
    assert_eq!(act(&mut state, Message::ProjectPicked(Some("beta".into()))), None, "the same project again changes nothing");
    assert_eq!(act(&mut state, Message::ProjectPicked(None)), Some(Event::ProjectPicked(None)));
    let _ = state.view();
}
