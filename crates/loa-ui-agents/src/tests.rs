use std::path::PathBuf;

use super::*;

fn file(name: &str, source: AgentSource) -> AgentFile {
    let mut definition = AgentDefinition::new(name, &format!("{name} agent")).unwrap();
    definition.set_model(Some(ModelSpec::One("anthropic/claude-sonnet".into()))).unwrap();
    definition.set_thinking_level(Some("high")).unwrap();
    definition.set_tools(Some(vec!["read".into(), "grep".into()])).unwrap();
    definition.set_spawns(Some(vec!["worker".into()])).unwrap();
    definition.set_body("You review code.\n");
    AgentFile { definition, path: PathBuf::from(format!("{name}.md")), source, enabled: true }
}

fn state() -> State {
    let mut state = State::new();
    state.set_agents(&[file("reviewer", AgentSource::User), file("worker", AgentSource::Project)]);
    state.set_tools(["read", "grep", "edit", "bash"].map(String::from).to_vec(), None);
    state.set_models(vec!["anthropic/claude-sonnet".into(), "@slow".into()]);
    state
}

fn type_into(state: &mut State, message: fn(String) -> Message, text: &str) {
    assert!(state.update(message(text.to_owned())).is_none());
}

#[test]
fn templates_are_blank_and_one_copy_per_agent() {
    let titles: Vec<String> = state().templates().iter().map(Template::to_string).collect();
    assert_eq!(titles, ["Blank", "Copy of reviewer", "Copy of worker"]);
}

#[test]
fn copy_template_fills_every_field_and_suffixes_the_name() {
    let mut state = state();
    let template = state.templates().remove(1);
    assert!(state.update(Message::Template(template)).is_none());
    let def = state.definition().unwrap();
    assert_eq!(def.name(), "reviewer-copy");
    assert_eq!(def.description(), "reviewer agent");
    assert_eq!(def.model(), Some(ModelSpec::One("anthropic/claude-sonnet".into())));
    assert_eq!(def.thinking_level(), Some("high"));
    assert_eq!(def.tools(), Some(vec!["read".into(), "grep".into()]));
    assert_eq!(def.spawns(), Some(vec!["worker".into()]));
    assert_eq!(def.body().trim_end(), "You review code.");
}

#[test]
fn blank_template_clears_the_form() {
    let mut state = state();
    state.copy(0);
    let blank = state.templates().remove(0);
    state.update(Message::Template(blank));
    let def = state.definition().unwrap();
    assert_eq!((def.name(), def.description()), ("", ""));
    assert_eq!((def.model(), def.tools(), def.spawns()), (None, None, None));
}

#[test]
fn tool_checkboxes_change_the_definitions_tools() {
    let mut state = state();
    state.new_agent();
    assert_eq!(state.definition().unwrap().tools(), None);
    state.update(Message::Tool("edit".into(), true));
    state.update(Message::Tool("read".into(), true));
    // Listed order, not click order.
    assert_eq!(state.definition().unwrap().tools(), Some(vec!["read".into(), "edit".into()]));
    state.update(Message::Tool("read".into(), false));
    assert_eq!(state.definition().unwrap().tools(), Some(vec!["edit".into()]));
    state.update(Message::Tool("edit".into(), false));
    assert_eq!(state.definition().unwrap().tools(), None);
}

#[test]
fn spawn_checkboxes_cover_the_other_agents_only() {
    let mut state = state();
    state.copy(0);
    type_into(&mut state, Message::Name, "reviewer");
    assert_eq!(state.other_agents(), ["worker"]);
    state.update(Message::Spawn("worker".into(), true));
    assert_eq!(state.definition().unwrap().spawns(), Some(vec!["worker".into()]));
}

#[test]
fn model_and_thinking_pickers_set_and_clear_their_keys() {
    let mut state = state();
    state.new_agent();
    state.update(Message::Model("@slow".into()));
    state.update(Message::Thinking("low".into()));
    let def = state.definition().unwrap();
    assert_eq!(def.model(), Some(ModelSpec::One("@slow".into())));
    assert_eq!(def.thinking_level(), Some("low"));
    state.update(Message::Model(MODEL_DEFAULT.into()));
    state.update(Message::Thinking(THINKING_DEFAULT.into()));
    let def = state.definition().unwrap();
    assert_eq!((def.model(), def.thinking_level()), (None, None));
}

#[test]
fn save_with_an_empty_name_shows_the_error_and_emits_nothing() {
    let mut state = state();
    state.new_agent();
    type_into(&mut state, Message::Description, "Does things");
    assert!(state.update(Message::Save).is_none());
    assert!(state.errors().iter().any(|error| error.field == "name"));
    assert!(state.errors().iter().all(|error| error.field != "description"));
    // The errors follow the edits once shown.
    type_into(&mut state, Message::Name, "does-things");
    assert!(state.errors().is_empty());
}

#[test]
fn save_emits_the_validated_definition() {
    let mut state = state();
    state.new_agent();
    type_into(&mut state, Message::Name, "does-things");
    type_into(&mut state, Message::Description, "Does things");
    state.update(Message::Tool("read".into(), true));
    state.update(Message::Tool("bash".into(), true));
    match state.update(Message::Save) {
        Some(Event::Save(def)) => {
            assert_eq!(def.name(), "does-things");
            assert_eq!(def.tools(), Some(vec!["read".into(), "bash".into()]));
        }
        other => panic!("expected Save, got {other:?}"),
    }
}

#[test]
fn an_unknown_tool_of_a_copied_agent_is_reported() {
    let mut state = state();
    state.set_tools(vec!["read".into()], None);
    state.copy(0);
    assert!(state.update(Message::Save).is_none());
    assert!(state.errors().iter().any(|error| error.field == "tools" && error.message.contains("grep")));
}

#[test]
fn close_emits_close() {
    assert!(matches!(State::new().update(Message::Close), Some(Event::Close)));
}

#[test]
fn view_builds_with_and_without_errors() {
    let mut state = state();
    let _ = state.view();
    state.new_agent();
    state.update(Message::Save);
    let _ = state.view();
}
