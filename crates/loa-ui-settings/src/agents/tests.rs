use std::path::PathBuf;

use super::*;

fn file(name: &str, source: AgentSource) -> AgentEntry {
    let fields = AgentFields {
        name: name.into(),
        description: format!("{name} agent"),
        model: Some(ModelSpec::One("anthropic/claude-sonnet".into())),
        thinking_level: Some("high".into()),
        tools: Some(vec!["read".into(), "grep".into()]),
        spawns: Some(vec!["worker".into()]),
        body: "You review code.\n".into(),
    };
    AgentEntry { path: PathBuf::from(format!("{name}.md")), source, enabled: true, fields }
}

fn choices(tools: &[&str]) -> Choices {
    Choices {
        models: vec!["anthropic/claude-sonnet".into(), "@slow".into()],
        thinking_levels: ["off", "low", "high"].map(String::from).to_vec(),
        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
        tools_note: None,
    }
}

fn state() -> State {
    let mut state = State::new();
    state.set_agents(vec![file("reviewer", AgentSource::User), file("worker", AgentSource::Project)]);
    state.set_choices(choices(&["read", "grep", "edit", "bash"]));
    state
}

fn local(state: &mut State, message: Local) -> Option<Intent> {
    state.update(Msg::Local(message))
}

fn type_into(state: &mut State, message: fn(String) -> Local, text: &str) {
    assert!(local(state, message(text.to_owned())).is_none());
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
    assert!(local(&mut state, Local::Template(template)).is_none());
    let fields = state.fields();
    assert_eq!(fields.name, "reviewer-copy");
    assert_eq!(fields.description, "reviewer agent");
    assert_eq!(fields.model, Some(ModelSpec::One("anthropic/claude-sonnet".into())));
    assert_eq!(fields.thinking_level.as_deref(), Some("high"));
    assert_eq!(fields.tools, Some(vec!["read".into(), "grep".into()]));
    assert_eq!(fields.spawns, Some(vec!["worker".into()]));
    assert_eq!(fields.body.trim_end(), "You review code.");
    // A copy keeps the copied file's other keys but writes a new file.
    assert_eq!((state.draft().base, state.draft().editing), (Some(PathBuf::from("reviewer.md")), None));
}

#[test]
fn blank_template_clears_the_form() {
    let mut state = state();
    state.copy(0);
    let blank = state.templates().remove(0);
    local(&mut state, Local::Template(blank));
    let fields = state.fields();
    assert_eq!((fields.name.as_str(), fields.description.as_str()), ("", ""));
    assert_eq!((fields.model, fields.tools, fields.spawns), (None, None, None));
    assert_eq!(state.draft().base, None);
}

#[test]
fn tool_checkboxes_change_the_definitions_tools() {
    let mut state = state();
    state.new_agent();
    assert_eq!(state.fields().tools, None);
    local(&mut state, Local::Tool("edit".into(), true));
    local(&mut state, Local::Tool("read".into(), true));
    // Listed order, not click order.
    assert_eq!(state.fields().tools, Some(vec!["read".into(), "edit".into()]));
    local(&mut state, Local::Tool("read".into(), false));
    assert_eq!(state.fields().tools, Some(vec!["edit".into()]));
    local(&mut state, Local::Tool("edit".into(), false));
    assert_eq!(state.fields().tools, None);
}

#[test]
fn spawn_checkboxes_cover_the_other_agents_only() {
    let mut state = state();
    state.copy(0);
    type_into(&mut state, Local::Name, "reviewer");
    assert_eq!(state.other_agents(), ["worker"]);
    local(&mut state, Local::Spawn("worker".into(), true));
    assert_eq!(state.fields().spawns, Some(vec!["worker".into()]));
}

#[test]
fn model_and_thinking_pickers_set_and_clear_their_keys() {
    let mut state = state();
    state.new_agent();
    local(&mut state, Local::Model("@slow".into()));
    local(&mut state, Local::Thinking("low".into()));
    let fields = state.fields();
    assert_eq!(fields.model, Some(ModelSpec::One("@slow".into())));
    assert_eq!(fields.thinking_level.as_deref(), Some("low"));
    local(&mut state, Local::Model(MODEL_DEFAULT.into()));
    local(&mut state, Local::Thinking(THINKING_DEFAULT.into()));
    let fields = state.fields();
    assert_eq!((fields.model, fields.thinking_level), (None, None));
}

#[test]
fn save_asks_the_app_to_check_and_edits_follow_its_errors() {
    let mut state = state();
    state.new_agent();
    type_into(&mut state, Local::Description, "Does things");
    let Some(Intent::Save(draft)) = local(&mut state, Local::Save) else { panic!("save") };
    assert_eq!(draft.fields.description, "Does things");
    let error = FieldError { field: "name", message: "name is required".into() };
    state.set_errors(vec![error.clone()]);
    assert_eq!(state.errors(), [error]);
    // The errors follow the edits once shown.
    let Some(Intent::Check(draft)) = local(&mut state, Local::Name("does-things".into())) else { panic!("check") };
    assert_eq!(draft.fields.name, "does-things");
    state.set_errors(Vec::new());
    assert!(state.errors().is_empty());
    // A fresh form stops checking.
    state.new_agent();
    assert!(local(&mut state, Local::Name("x".into())).is_none());
}

#[test]
fn save_emits_the_draft() {
    let mut state = state();
    state.new_agent();
    type_into(&mut state, Local::Name, "does-things");
    type_into(&mut state, Local::Description, "Does things");
    local(&mut state, Local::Tool("read".into(), true));
    local(&mut state, Local::Tool("bash".into(), true));
    match local(&mut state, Local::Save) {
        Some(Intent::Save(draft)) => {
            assert_eq!(draft.fields.name, "does-things");
            assert_eq!(draft.fields.tools, Some(vec!["read".into(), "bash".into()]));
            assert_eq!((draft.base, draft.editing), (None, None));
        }
        other => panic!("expected Save, got {other:?}"),
    }
}

#[test]
fn close_emits_close() {
    assert_eq!(State::new().update(Msg::Emit(Intent::Close)), Some(Intent::Close));
    assert_eq!(State::new().update(Msg::Emit(Intent::Toggle(PathBuf::from("x.md")))), None);
}

#[test]
fn view_builds_with_and_without_errors() {
    let mut state = state();
    let _ = state.view();
    state.new_agent();
    local(&mut state, Local::Save);
    state.set_errors(vec![FieldError { field: "name", message: "name is required".into() }]);
    state.set_error("form", "cannot write".into());
    let _ = state.view();
    let _ = state.body();
}

#[test]
fn replace_and_delete_ask_before_their_event() {
    let mut state = state();
    state.edit(0);
    assert_eq!(state.editing(), Some(std::path::Path::new("reviewer.md")));

    assert!(local(&mut state, Local::Delete).is_none());
    assert_eq!(state.pending_question().as_deref(), Some("Delete reviewer?"));
    assert!(local(&mut state, Local::Cancel).is_none());
    assert!(state.pending_question().is_none());
    assert!(local(&mut state, Local::Confirm).is_none());

    local(&mut state, Local::Delete);
    assert!(matches!(local(&mut state, Local::Confirm), Some(Intent::Delete(path)) if path == std::path::Path::new("reviewer.md")));

    state.ask_replace(state.draft(), PathBuf::from("reviewer.md"));
    assert_eq!(state.pending_question().as_deref(), Some("Replace reviewer?"));
    assert!(matches!(local(&mut state, Local::Confirm), Some(Intent::Replace(draft, _)) if draft.fields.name == "reviewer"));
    assert!(matches!(local(&mut state, Local::Toggle), Some(Intent::Toggle(_))));
}
