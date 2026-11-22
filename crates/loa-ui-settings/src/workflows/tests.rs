use super::*;

fn state() -> State {
    let mut state = State::new();
    state.set_workflows(vec![
        Workflow { role: "RESEARCH".into(), models: vec!["provider/model:high".into()], rules: Rules::Managed { name: "Research".into(), allowed: vec!["scout".into()], disallowed: vec!["builder".into()], notes: "Be brief.".into() } },
        Workflow { role: "DEEP".into(), models: vec![], rules: Rules::HandWritten { text: "## Deep Mode\nOriginal\n".into() } },
        Workflow { role: "EMPTY".into(), models: vec![], rules: Rules::None },
    ]);
    state.set_agents(vec![AgentInfo { name: "scout".into(), enabled: true }, AgentInfo { name: "builder".into(), enabled: true }, AgentInfo { name: "critic".into(), enabled: false }]);
    state
}
fn local(state: &mut State, message: Local) -> Option<Intent> { state.update(Msg::Local(message)) }

#[test]
fn edit_managed_and_save() {
    let mut s = state(); assert!(s.edit("RESEARCH"));
    assert_eq!(s.access("scout"), Access::Allow); assert_eq!(s.access("builder"), Access::Disallow);
    local(&mut s, Local::Access("scout".into(), Access::Unspecified));
    local(&mut s, Local::Access("critic".into(), Access::Allow));
    local(&mut s, Local::Name("Review".into()));
    let Some(Intent::Save { draft, previous_role }) = local(&mut s, Local::Save) else { panic!("save") };
    assert_eq!(previous_role.as_deref(), Some("RESEARCH"));
    let Rules::Managed { name, allowed, disallowed, .. } = draft.workflow.rules else { panic!("managed") };
    assert_eq!(name, "Review"); assert_eq!(allowed, ["critic"]); assert_eq!(disallowed, ["builder"]);
    assert_eq!(draft.workflow.models, ["provider/model:high"]);
}

#[test]
fn handwritten_edit_is_saved_as_raw_text() {
    use iced::widget::text_editor::{Action, Edit};
    let mut s = state(); assert!(s.edit("DEEP"));
    local(&mut s, Local::Text(Action::Edit(Edit::Paste(std::sync::Arc::new("New ".to_owned())))));
    let Some(Intent::Save { draft, previous_role }) = local(&mut s, Local::Save) else { panic!("save") };
    assert_eq!(previous_role.as_deref(), Some("DEEP"));
    let Rules::HandWritten { text } = draft.workflow.rules else { panic!("hand written") };
    assert!(text.contains("New ")); assert!(text.contains("## Deep Mode"));
}

#[test]
fn delete_requires_confirmation_and_can_be_cancelled() {
    let mut s = state(); s.edit("DEEP");
    assert_eq!(local(&mut s, Local::ConfirmDelete), None);
    local(&mut s, Local::Delete); local(&mut s, Local::CancelDelete);
    assert_eq!(local(&mut s, Local::ConfirmDelete), None);
    local(&mut s, Local::Delete);
    assert_eq!(local(&mut s, Local::ConfirmDelete), Some(Intent::Delete("DEEP".into())));
}

#[test]
fn new_and_selection_emit_intents() {
    let mut s = state();
    assert_eq!(local(&mut s, Local::Open("RESEARCH".into())), Some(Intent::Open("RESEARCH".into())));
    assert_eq!(local(&mut s, Local::New), Some(Intent::New)); assert_eq!(s.selected(), None);
    assert_eq!(s.draft().workflow.role, "");
}

#[test]
fn view_displays_workflows_agents_and_reload() {
    let mut s = state(); s.edit("RESEARCH"); s.set_error(Some(Error::ChangedOnDisk));
    let mut ui = iced_test::simulator(s.view());
    for text in ["New workflow", "RESEARCH", "DEEP", "EMPTY", "critic (disabled)", "Save", "Delete", "Reload", "Close"] { assert!(ui.find(text).is_ok(), "missing {text}"); }
}
