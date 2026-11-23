use super::*;
use crate::{app::Message as AppMessage, testing::{FakeHarness, TestApp}};
use loa_ui_kit::widgets::Msg;
use loa_ui_settings::workflows::Local;

fn app(harness: &'static str) -> TestApp {
    let mut test = TestApp::with(FakeHarness::new(harness));
    let dir = test.dir.path().join("omp");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.yml"), "modelRoles:\n  default: provider/model\n  DEEP: provider/deep\n").unwrap();
    std::fs::write(dir.join("RULES.md"), "## Deep Mode\nWhen the active model role is `DEEP`.\nOriginal.\n").unwrap();
    test.send(AppMessage::area(Message::Open));
    test
}
fn local(test: &mut TestApp, message: Local) { test.send(AppMessage::area(Message::Screen(Msg::Local(message)))); }

/// An omp session that switches models between turns, with `config` as omp's `config.yml`.
fn role_app(config: &str, effort: bool) -> TestApp {
    use loa_contracts::harness::{Capabilities, ModelSwitch};
    let capabilities = Capabilities::builder().model_switch(ModelSwitch::PerTurn).build();
    let harness = FakeHarness::new("omp").capabilities(capabilities).models(&[("provider/deep", &["high"])]);
    let test = TestApp::with(if effort { harness.effort() } else { harness });
    let dir = test.dir.path().join("omp");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.yml"), config).unwrap();
    test
}
const DEEP: &str = "modelRoles:\n  DEEP: provider/deep:high\n";

fn role_of(app: &crate::app::App) -> Option<&str> {
    app.ctx.runtime.sessions().active()?.dom().root()?.str_attr(loa_core::attr::MODEL_ROLE)
}
fn sent(app: &crate::app::App, text: &str) -> bool {
    app.ctx.runtime.sessions().active().is_some_and(|session| !session.is_busy() && session.dom().iter().any(|node| node.text == text))
}
fn open(test: &mut TestApp, id: &str) {
    use loa_ui_shell as shell;
    let nav = loa_view::shell::SidebarIntent::OpenSession(id.to_owned());
    test.send(AppMessage::Shell(shell::Message::Sidebar(shell::sidebar::Message::Emit(nav))));
}

#[test]
fn loads_and_edits_handwritten_rules() {
    let mut test = app("omp");
    assert!(test.area::<State>().workflows.iter().any(|w| matches!(w.rules, workflows::Rules::HandWritten { .. })));
    local(&mut test, Local::Open("DEEP".into()));
    use iced::widget::text_editor::{Action, Edit, Motion};
    local(&mut test, Local::Text(Action::Move(Motion::DocumentEnd)));
    local(&mut test, Local::Text(Action::Edit(Edit::Paste(std::sync::Arc::new("Updated.\n".into())))));
    local(&mut test, Local::Save);
    assert!(std::fs::read_to_string(test.dir.path().join("omp/RULES.md")).unwrap().contains("Updated."));
}

#[test]
fn creates_structured_workflow_and_reopens_the_same_fields() {
    use iced::widget::text_editor::{Action, Edit};
    use loa_view::settings::workflows::{ManagedFields, RoutingNote, RuleBlock, Rules};

    let mut test = app("omp");
    let path = test.dir.path().join("omp/RULES.md");
    let original = std::fs::read_to_string(&path).unwrap();
    local(&mut test, Local::New);
    local(&mut test, Local::Name("Review".into()));
    local(&mut test, Local::Description("Use for documentation review.".into()));
    local(&mut test, Local::Access("reader".into(), screen::Access::Allow));
    local(&mut test, Local::Routing("reader".into(), "Check examples.".into()));
    local(&mut test, Local::AddStep);
    local(&mut test, Local::Step(0, "Read the change.".into()));
    local(&mut test, Local::AddStep);
    local(&mut test, Local::Step(1, "Summarize findings.".into()));
    local(&mut test, Local::AddBlock);
    local(&mut test, Local::BlockHeading(0, "Reporting".into()));
    local(&mut test, Local::BlockText(0, Action::Edit(Edit::Paste(std::sync::Arc::new("Cite the relevant files.".into())))));
    local(&mut test, Local::Save);
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written, format!("{original}{}", concat!(
        "\n<!-- omp-gui:workflow REVIEW -->\n## Review Mode\n\n",
        "When the active model role is `REVIEW`, operate in Review Mode.\n\n",
        "- Allowed agents: `reader`. Route work only to these agents.\n",
        "\n<!-- loa:workflow-field -->\n### When to use\n\nUse for documentation review.\n",
        "\n<!-- loa:workflow-field -->\n### Routing\n\n- `reader`: Check examples.\n",
        "\n<!-- loa:workflow-field -->\n### Steps\n\n1. Read the change.\n2. Summarize findings.\n",
        "\n<!-- loa:workflow-field -->\n### Reporting\n\nCite the relevant files.\n",
        "<!-- /omp-gui:workflow -->\n",
    )));
    local(&mut test, Local::Open("DEEP".into()));
    local(&mut test, Local::Open("REVIEW".into()));
    let draft = test.area::<State>().screen.draft().workflow;
    assert_eq!(draft.models, ["provider/model"]);
    let expected = Rules::Managed {
        name: "Review".into(), allowed: vec!["reader".into()], disallowed: vec![], notes: String::new(),
        fields: ManagedFields {
            description: "Use for documentation review.".into(),
            routing: vec![RoutingNote { agent: "reader".into(), note: "Check examples.".into() }],
            steps: vec!["Read the change.".into(), "Summarize findings.".into()],
            blocks: vec![RuleBlock { heading: "Reporting".into(), text: "Cite the relevant files.".into() }],
        },
    };
    assert_eq!(draft.rules, expected);
    local(&mut test, Local::CopyFrom("REVIEW".into()));
    local(&mut test, Local::Name("Second review".into()));
    local(&mut test, Local::Save);
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.starts_with(&written));
    assert!(saved.contains("<!-- omp-gui:workflow SECOND_REVIEW -->"));
    let Rules::Managed { fields, .. } = expected else { unreachable!() };
    let Rules::Managed { fields: copied, .. } = test.area::<State>().screen.draft().workflow.rules else { panic!("copy is not managed") };
    assert_eq!(copied, fields);
}

#[test]
fn copies_handwritten_rules_without_changing_the_source() {
    let mut test = app("omp");
    let path = test.dir.path().join("omp/RULES.md");
    let original = std::fs::read_to_string(&path).unwrap();
    local(&mut test, Local::CopyFrom("DEEP".into()));
    local(&mut test, Local::Name("Study".into()));
    local(&mut test, Local::Save);
    let written = std::fs::read_to_string(&path).unwrap();
    let (source, copy) = written.split_once("\n<!-- omp-gui:workflow STUDY -->\n").unwrap();
    assert_eq!(source, original);
    assert!(copy.contains(&original));
    local(&mut test, Local::Open("STUDY".into()));
    let view::Rules::Managed { notes, .. } = test.area::<State>().screen.draft().workflow.rules else { panic!("copy is not managed") };
    assert_eq!(notes, original.trim_end_matches('\n'));
}

#[test]
fn outside_edits_open_as_raw_markdown_and_save_in_place() {
    use iced::widget::text_editor::{Action, Edit, Motion};

    let mut test = app("omp");
    let path = test.dir.path().join("omp/RULES.md");
    let body = "## Deep Mode\n\nKeep this custom preamble.\n\nWhen the active model role is `DEEP`, read the examples.\n\n<!-- loa:workflow-field -->\n### Symbols\n\nOutside notation.\n";
    let suffix = "\n## Unrelated\nKeep this too.\n";
    let marked = |body: &str| format!("<!-- omp-gui:workflow DEEP -->\n{body}<!-- /omp-gui:workflow -->\n{suffix}");
    std::fs::write(&path, marked(body)).unwrap();
    test.send(AppMessage::area(Message::Edit("DEEP".into())));
    assert!(matches!(test.area::<State>().screen.draft().workflow.rules, view::Rules::External { .. }));
    local(&mut test, Local::Save);
    let external_edit = body.replace("read the examples", "review the examples");
    test.send(AppMessage::area(Message::Screen(Msg::Emit(Intent::Close))));
    std::fs::write(&path, marked(&external_edit)).unwrap();
    test.send(AppMessage::area(Message::Open));
    local(&mut test, Local::Save);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), marked(&external_edit));
    local(&mut test, Local::Text(Action::Move(Motion::DocumentEnd)));
    local(&mut test, Local::Text(Action::Edit(Edit::Paste(std::sync::Arc::new("Add a source link.\n".into())))));
    local(&mut test, Local::Save);
    let saved = std::fs::read_to_string(&path).unwrap();
    let edited = format!("{external_edit}Add a source link.\n");
    assert_eq!(saved, marked(&edited));
    local(&mut test, Local::CopyFrom("DEEP".into()));
    local(&mut test, Local::Save);
    let copied = std::fs::read_to_string(&path).unwrap();
    assert!(copied.starts_with(&saved));
    assert!(copied.contains("<!-- omp-gui:workflow DEEP_COPY -->"));
    let view::Rules::Managed { notes, .. } = test.area::<State>().screen.draft().workflow.rules else { panic!("copy is not managed") };
    assert_eq!(notes, edited.trim_end_matches('\n'));
}

#[test]
fn saving_disables_editor_actions_until_the_file_is_reloaded() {
    let mut test = app("omp");
    local(&mut test, Local::New);
    local(&mut test, Local::Name("Review".into()));
    local(&mut test, Local::Description("Original draft.".into()));
    let save = test.app.update(AppMessage::area(Message::Screen(Msg::Local(Local::Save))));
    let view = test.area::<State>().view_screen(&test.app.ctx).unwrap();
    assert!(loa_ui_kit::testkit::click_label(view, "Saving workflow…").is_empty());
    local(&mut test, Local::Description("Late edit.".into()));
    let view::Rules::Managed { fields, .. } = test.area::<State>().screen.draft().workflow.rules else { panic!("not managed") };
    assert_eq!(fields.description, "Original draft.");
    test.run(save);
    local(&mut test, Local::Description("Next edit.".into()));
    local(&mut test, Local::Save);
    assert!(std::fs::read_to_string(test.dir.path().join("omp/RULES.md")).unwrap().contains("Next edit."));
}

#[test]
fn stale_save_does_not_overwrite_external_changes() {
    let mut test = app("omp");
    local(&mut test, Local::Open("DEEP".into()));
    let path = test.dir.path().join("omp/RULES.md");
    std::fs::write(&path, "outside edit\n").unwrap();
    local(&mut test, Local::Save);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "outside edit\n");
    assert!(!loa_ui_kit::testkit::click_label(test.area::<State>().screen.view(), "Reload").is_empty());
    assert_eq!(test.area::<State>().workflows.len(), 2);
}

#[test]
fn picking_without_session_updates_model_and_role() {
    let mut test = app("omp");
    test.send(AppMessage::area(Message::Pick("DEEP".into())));
    assert_eq!(test.app.ctx.composer.models().1.and_then(|m| m.role.as_deref()), Some("DEEP"));
}

#[test]
fn non_omp_cannot_pick_a_workflow() {
    let mut test = app("other");
    test.send(AppMessage::area(Message::Pick("DEEP".into())));
    assert_eq!(test.notice(), "Available with OMP sessions");
}

#[test]
fn selected_role_is_disclosed_in_new_session_config() {
    let mut test = app("omp");
    test.send(AppMessage::area(Message::Pick("DEEP".into())));
    test.session("hello");
    let seen = test.harness.seen();
    let config = seen.configs.last().unwrap();
    assert_eq!(config.model_role.as_deref(), Some("DEEP"));
    assert_eq!(config.instructions.as_deref(), Some("The active model role is `DEEP`. Follow the RULES.md section for that role."));
}

#[test]
fn role_switch_reads_resolved_directory_and_discloses_once() {
    let mut test = role_app(DEEP, true);
    test.session("hello");
    test.command("sessions.role.DEEP");
    test.until("role switched", |app| role_of(app) == Some("DEEP"));
    test.prompt("first");
    test.until("first turn ends", |app| !app.ctx.composer.is_busy());
    test.prompt("second");
    test.until("second turn ends", |app| !app.ctx.composer.is_busy());
    let seen = test.harness.seen();
    assert_eq!(seen.models.last().map(String::as_str), Some("provider/deep"));
    assert_eq!(seen.efforts.last().map(String::as_str), Some("high"));
    assert!(seen.prompts.iter().any(|prompt| prompt == "The active model role is now `DEEP`.\n\nfirst"));
    assert!(seen.prompts.iter().any(|prompt| prompt == "second"));
}

#[test]
fn mid_turn_role_pick_precedes_the_queued_prompt() {
    let mut test = role_app(DEEP, true);
    test.session("hello");
    test.prompt("ask");
    test.until("decision waits", |app| app.areas.get::<crate::areas::decisions::State>().focused().is_some());
    test.command("sessions.role.DEEP");
    test.prompt("queued");
    assert!(test.harness.seen().models.is_empty());
    assert!(!test.harness.seen().prompts.iter().any(|prompt| prompt.contains("queued")));
    test.send(AppMessage::area(crate::areas::decisions::State::dismiss()));
    test.until("queued prompt completes", |app| sent(app, "The active model role is now `DEEP`.\n\nqueued"));
    assert_eq!(test.harness.seen().efforts, ["high"]);
}

#[test]
fn failed_role_switch_releases_the_held_prompt() {
    let mut test = role_app("modelRoles:\n  GONE: provider/missing\n", true);
    test.session("hello");
    test.prompt("ask");
    test.until("decision waits", |app| app.areas.get::<crate::areas::decisions::State>().focused().is_some());
    test.command("sessions.role.GONE");
    test.prompt("queued");
    test.send(AppMessage::area(crate::areas::decisions::State::dismiss()));
    test.until("the held prompt goes out without a role note", |app| sent(app, "queued"));
    test.until("the composer is ready", |app| !app.ctx.composer.is_busy());
    assert_eq!(role_of(&test.app), None);
}

#[test]
fn prompt_sent_while_an_idle_session_switches_waits_for_the_switch() {
    let mut test = role_app(DEEP, true);
    test.session("hello");
    let switch = test.app.command(&loa_ui_shell::palette::CommandId::new("sessions.role.DEEP"));
    test.prompt("during");
    assert!(!test.harness.seen().prompts.iter().any(|prompt| prompt.contains("during")), "sent before the switch ended");
    assert!(test.app.ctx.composer.is_busy());
    test.run(switch);
    test.until("the held prompt goes out with the role note", |app| sent(app, "The active model role is now `DEEP`.\n\nduring"));
    assert!(!test.app.ctx.composer.is_busy());
}

#[test]
fn role_note_stays_with_the_session_that_switched() {
    let mut test = role_app(DEEP, true);
    let first = test.session("hello");
    test.command("sessions.role.DEEP");
    test.until("role switched", |app| role_of(app) == Some("DEEP"));
    test.command("session.new");
    let second = test.session("other");
    assert_ne!(first, second);
    open(&mut test, &first);
    test.prompt("back");
    test.until("the first session answers", |app| sent(app, "The active model role is now `DEEP`.\n\nback"));
    let seen = test.harness.seen();
    assert!(seen.prompts.iter().any(|prompt| prompt == "other"));
    assert_eq!(seen.prompts.iter().filter(|prompt| prompt.contains("The active model role")).count(), 1);
}

#[test]
fn resumed_session_is_told_the_role_its_dom_records() {
    let mut test = role_app(DEEP, true);
    let id = test.session("hello");
    test.command("sessions.role.DEEP");
    test.until("role switched", |app| role_of(app) == Some("DEEP"));
    let mut test = test.reopen();
    open(&mut test, &id);
    test.prompt("again");
    test.until("the resumed session answers", |app| sent(app, "again") || sent(app, "The active model role is now `DEEP`.\n\nagain"));
    let seen = test.harness.seen();
    assert_eq!(seen.resumes, 1);
    let config = seen.configs.last().unwrap();
    assert_eq!(config.session_id, id);
    assert_eq!(config.instructions.as_deref(), Some("The active model role is `DEEP`. Follow the RULES.md section for that role."));
}

#[test]
fn refused_thinking_level_still_records_the_role() {
    let mut test = role_app(DEEP, false);
    test.session("hello");
    test.command("sessions.role.DEEP");
    test.until("role recorded", |app| role_of(app) == Some("DEEP"));
    assert_eq!(test.harness.seen().models, ["provider/deep"]);
    assert!(test.notice().contains("high thinking wasn't applied"), "{}", test.notice());
}

#[test]
fn role_commands_follow_config_changes() {
    let mut test = role_app(DEEP, true);
    test.session("hello");
    assert!(test.palette_titles().iter().any(|title| title == "Model role: DEEP (provider/deep:high)"));
    std::fs::write(test.dir.path().join("omp/config.yml"), "modelRoles:\n  DEEP: provider/deep:high\n  STUDY: provider/deep\n").unwrap();
    assert!(test.palette_titles().iter().any(|title| title == "Model role: STUDY (provider/deep)"));
    test.prompt("/role");
    assert_eq!(test.notice(), "Name a model role: /role <DEEP|STUDY>");
}

#[test]
fn reload_then_save_keeps_the_version_on_disk() {
    let mut test = app("omp");
    local(&mut test, Local::Open("DEEP".into()));
    use iced::widget::text_editor::{Action, Edit, Motion};
    local(&mut test, Local::Text(Action::Move(Motion::DocumentEnd)));
    local(&mut test, Local::Text(Action::Edit(Edit::Paste(std::sync::Arc::new("Draft.\n".into())))));
    let path = test.dir.path().join("omp/RULES.md");
    let outside = "## Deep Mode\nWhen the active model role is `DEEP`.\nChanged outside.\n";
    std::fs::write(&path, outside).unwrap();
    local(&mut test, Local::Save);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), outside);
    test.send(AppMessage::area(Message::Screen(Msg::Emit(Intent::Reload))));
    local(&mut test, Local::Save);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), outside);
}
