use super::*;

fn state() -> State {
    let mut state = State::new();
    state.set_workflows(vec![
        Workflow { role: "RESEARCH".into(), models: vec!["provider/model:high".into()], rules: Rules::Managed { name: "Research".into(), allowed: vec!["scout".into()], disallowed: vec!["builder".into()], notes: "Be brief.".into(), fields: ManagedFields::default() } },
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
    for text in ["New workflow", "RESEARCH", "DEEP", "Copy from DEEP…", "EMPTY", "Description", "Routing note", "Add step", "Add block", "critic (disabled)", "Save", "Delete", "Reload", "Close"] {
        assert!(ui.find(text).is_ok(), "missing {text}");
    }
}

#[test]
fn structured_fields_edit_reorder_and_bound_indices() {
    use iced::widget::text_editor::{Action, Edit};
    let mut s = state(); s.edit("RESEARCH");
    local(&mut s, Local::Description("A short description".into()));
    local(&mut s, Local::Routing("scout".into(), "Find context".into()));
    local(&mut s, Local::Routing("builder".into(), "Ignored".into()));
    local(&mut s, Local::AddStep); local(&mut s, Local::Step(0, "First".into()));
    local(&mut s, Local::AddStep); local(&mut s, Local::Step(1, "Second".into()));
    local(&mut s, Local::MoveStepUp(1)); local(&mut s, Local::MoveStepDown(0));
    local(&mut s, Local::MoveStepDown(usize::MAX)); local(&mut s, Local::RemoveStep(usize::MAX));
    local(&mut s, Local::MoveStepUp(1)); local(&mut s, Local::RemoveStep(1));
    local(&mut s, Local::AddBlock); local(&mut s, Local::BlockHeading(0, "Guidance".into()));
    local(&mut s, Local::BlockText(0, Action::Edit(Edit::Paste(std::sync::Arc::new("Some **markdown**".into())))));
    local(&mut s, Local::BlockHeading(usize::MAX, "Ignored".into()));
    local(&mut s, Local::RemoveBlock(usize::MAX));
    let Rules::Managed { fields, .. } = s.draft().workflow.rules else { panic!("managed") };
    assert_eq!(fields.description, "A short description");
    assert_eq!(fields.routing, vec![RoutingNote { agent: "scout".into(), note: "Find context".into() }]);
    assert_eq!(fields.steps, ["Second"]);
    assert_eq!(fields.blocks[0].heading, "Guidance"); assert!(fields.blocks[0].text.contains("Some **markdown**"));
    local(&mut s, Local::RemoveBlock(0));
    let Rules::Managed { fields, .. } = s.draft().workflow.rules else { panic!("managed") };
    assert!(fields.blocks.is_empty());
}

#[test]
fn clearing_a_routing_note_or_disallowing_its_agent_removes_it() {
    let mut s = state(); s.edit("RESEARCH");
    local(&mut s, Local::Routing("scout".into(), "Find context".into()));
    local(&mut s, Local::Routing("scout".into(), String::new()));
    let Rules::Managed { fields, .. } = s.draft().workflow.rules else { panic!("managed") };
    assert!(fields.routing.is_empty());
    local(&mut s, Local::Routing("scout".into(), "Find context".into()));
    local(&mut s, Local::Access("scout".into(), Access::Disallow));
    let Rules::Managed { fields, disallowed, .. } = s.draft().workflow.rules else { panic!("managed") };
    assert!(fields.routing.is_empty());
    assert!(disallowed.iter().any(|agent| agent == "scout"));
}

#[test]
fn copy_handwritten_preserves_source_and_creates_independent_draft() {
    let mut s = state(); s.edit("RESEARCH");
    assert_eq!(local(&mut s, Local::CopyFrom("DEEP".into())), None);
    assert_eq!(s.selected(), None); let draft = s.draft();
    assert_eq!(draft.workflow.role, "");
    let Rules::Managed { name, notes, fields, .. } = draft.workflow.rules else { panic!("managed") };
    assert_eq!(name, "DEEP copy"); assert_eq!(notes, "## Deep Mode\nOriginal\n"); assert_eq!(fields, ManagedFields::default());
    let Some(Intent::Save { previous_role, .. }) = local(&mut s, Local::Save) else { panic!("save") };
    assert_eq!(previous_role, None);
    assert!(s.edit("DEEP"));
    assert_eq!(s.draft().workflow.rules, Rules::HandWritten { text: "## Deep Mode\nOriginal\n".into() });
}

#[test]
fn external_rules_are_raw_and_cannot_be_renamed() {
    use iced::widget::text_editor::{Action, Edit};
    let mut s = state();
    s.set_workflows(vec![Workflow { role: "EXTERNAL".into(), models: vec![], rules: Rules::External { text: "External\n".into() } }]);
    assert!(s.edit("EXTERNAL"));
    local(&mut s, Local::Role("OTHER".into()));
    assert_eq!(s.draft().workflow.role, "EXTERNAL");
    assert_eq!(s.draft().workflow.rules, Rules::External { text: "External\n".into() });
    local(&mut s, Local::Text(Action::Edit(Edit::Paste(std::sync::Arc::new("New ".into())))));
    let Rules::External { text } = s.draft().workflow.rules else { panic!("external") };
    assert!(text.contains("New ")); assert!(text.contains("External"));
    let mut ui = iced_test::simulator(s.view());
    assert!(ui.find("Edited outside Loa — edit the raw Markdown below; this workflow cannot be renamed here.").is_ok());
}
