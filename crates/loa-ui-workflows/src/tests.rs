use super::*;

fn entry(role: &str, name: &str, editable: bool) -> Entry {
    Entry {
        role: role.into(),
        name: name.into(),
        allowed: vec!["scout".into()],
        disallowed: vec!["builder".into()],
        notes: "Be brief.".into(),
        editable,
    }
}

fn agents() -> Vec<AgentInfo> {
    vec![
        AgentInfo { name: "scout".into(), enabled: true },
        AgentInfo { name: "builder".into(), enabled: true },
        AgentInfo { name: "critic".into(), enabled: false },
    ]
}

fn state() -> State {
    let mut state = State::new();
    state.set_data(vec![entry("RESEARCH", "Research", true), entry("DEEP", "Deep", false)], agents());
    state
}

#[test]
fn a_new_workflow_needs_a_name() {
    let mut state = state();
    state.edit(None);
    assert_eq!(state.update(Message::Save), None);
    assert!(state.problem().is_some());
    state.update(Message::Name("  Review ".into()));
    state.update(Message::Access("scout".into(), Access::Allow));
    state.update(Message::Access("critic".into(), Access::Disallow));
    let event = state.update(Message::Save);
    assert_eq!(
        event,
        Some(Event::Save {
            previous_role: None,
            entry: Entry {
                role: String::new(),
                name: "Review".into(),
                allowed: vec!["scout".into()],
                disallowed: vec!["critic".into()],
                notes: String::new(),
                editable: true,
            }
        })
    );
}

#[test]
fn editing_loads_the_choices_and_saves_with_the_previous_role() {
    let mut state = state();
    state.edit(Some("RESEARCH"));
    assert_eq!(state.access("scout"), Access::Allow);
    assert_eq!(state.access("builder"), Access::Disallow);
    assert_eq!(state.access("critic"), Access::Unspecified);
    state.update(Message::Name("Research 2".into()));
    state.update(Message::Access("scout".into(), Access::Unspecified));
    let Some(Event::Save { previous_role, entry }) = state.update(Message::Save) else { panic!("saves") };
    assert_eq!(previous_role.as_deref(), Some("RESEARCH"));
    assert_eq!((entry.role.as_str(), entry.name.as_str()), ("RESEARCH", "Research 2"));
    assert!(entry.allowed.is_empty());
    assert_eq!(entry.disallowed, ["builder"]);
    assert_eq!(entry.notes, "Be brief.");
}

#[test]
fn an_agent_in_both_lists_is_refused() {
    let mut state = State::new();
    let mut both = entry("X", "X", true);
    both.disallowed = vec!["scout".into()];
    state.set_data(vec![both], agents());
    state.edit(Some("X"));
    assert_eq!(state.update(Message::Save), None);
    assert!(state.error.as_deref().unwrap().contains("scout"));
}

#[test]
fn read_only_workflows_ignore_edits_and_cannot_be_saved_or_deleted() {
    let mut state = state();
    state.edit(Some("DEEP"));
    state.update(Message::Name("Other".into()));
    state.update(Message::Access("critic".into(), Access::Allow));
    assert_eq!(state.name, "Deep");
    assert_eq!(state.access("critic"), Access::Unspecified);
    assert_eq!(state.update(Message::Save), None);
    assert_eq!(state.update(Message::Delete), None);
    assert_eq!(state.update(Message::Delete), None);
}

#[test]
fn delete_asks_before_emitting() {
    let mut state = state();
    state.edit(Some("RESEARCH"));
    assert_eq!(state.update(Message::Delete), None);
    assert!(state.confirming_delete);
    state.update(Message::CancelDelete);
    assert_eq!(state.update(Message::Delete), None);
    assert_eq!(state.update(Message::Delete), Some(Event::Delete { role: "RESEARCH".into() }));
    state.edit(None);
    assert_eq!(state.update(Message::Delete), None, "a new draft has nothing to delete");
}

#[test]
fn refreshing_keeps_the_draft_while_its_role_exists() {
    let mut state = state();
    state.edit(Some("RESEARCH"));
    state.update(Message::Name("Draft".into()));
    state.set_data(vec![entry("RESEARCH", "Research", true)], agents());
    assert_eq!((state.selected(), state.name.as_str()), (Some("RESEARCH"), "Draft"));
    state.set_data(vec![], agents());
    assert_eq!((state.selected(), state.name.as_str()), (None, ""));
}

#[test]
fn unknown_agents_of_a_workflow_still_get_a_row() {
    let mut state = State::new();
    state.set_data(vec![entry("X", "X", true)], vec![]);
    state.edit(Some("X"));
    let names: Vec<String> = state.rows().into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["scout", "builder"]);
}

#[test]
fn errors_and_close_pass_through() {
    let mut state = state();
    state.set_error(Some("boom".into()));
    assert_eq!(state.error.as_deref(), Some("boom"));
    assert_eq!(state.update(Message::Close), Some(Event::Close));
}

#[test]
fn the_view_shows_the_list_the_agents_and_the_hints() {
    let mut state = state();
    state.edit(Some("RESEARCH"));
    state.set_error(Some("Could not save".into()));
    let mut ui = iced_test::simulator(state.view());
    for label in ["New workflow", "Research", "Deep", "read-only, from RULES.md", "Could not save", "critic (disabled)", "Save", "Delete", "Close"] {
        assert!(ui.find(label).is_ok(), "the screen shows {label:?}");
    }
    drop(ui);

    state.edit(Some("DEEP"));
    let mut ui = iced_test::simulator(state.view());
    assert!(ui.find("This workflow is written by hand in RULES.md. Edit it there.").is_ok());
    assert!(ui.find("Save").is_err(), "no Save for a read-only workflow");
}

#[test]
fn each_radio_is_labelled_with_its_agent() {
    assert_eq!(Access::Allow.describe("a"), "Allow a");
    assert_eq!(Access::Disallow.describe("scout"), "Disallow scout");
    assert_eq!(Access::Unspecified.describe("b"), "Not set b");
}
