//! The agents area driven through `App::update`: omp's directory, models and
//! tools are fixed, so nothing here starts omp.

use std::path::{Path, PathBuf};

use loa_ui_composer as composer;

use super::*;
use crate::app::tests::{fake_app, run, update};
use crate::app::{App, Message as AppMessage};

const TOOLS: [&str; 4] = ["read", "edit", "grep", "bash"];

/// An app whose only harness is `harness`, with omp's agent directory at
/// `<dir>/agent`.
pub(super) fn omp_app(dir: &Path, harness: &'static str) -> (App, PathBuf) {
    let mut app = fake_app(dir, &[harness]);
    let agent_dir = dir.join("agent");
    app.areas.agents.use_fixed(Fixed {
        agent_dir: agent_dir.clone(),
        models: vec!["@slow".into(), "anthropic/claude-sonnet".into()],
        tools: TOOLS.map(String::from).to_vec(),
    });
    (app, agent_dir)
}

pub(super) fn fill(app: &mut App, message: form::Message) {
    update(app, AppMessage::Agents(Message::Form(message)));
}

fn command(app: &mut App, id: &str) {
    let task = app.command(&palette::CommandId::new(id));
    run(app, task);
}

/// The palette command titled `title` as the palette lists it when opened.
fn palette_command(app: &mut App, title: &str) -> Option<palette::Command> {
    update(app, AppMessage::Palette(palette::Message::Open));
    app.palette.commands().iter().find(|command| command.title == title).cloned()
}

#[test]
fn composer_button_opens_the_form_with_omp() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = omp_app(dir.path(), "omp");
    update(&mut app, AppMessage::Composer(composer::Message::OpenAgents));
    assert_eq!(app.ctx.screen(), Some(AreaId::Agents));
    assert!(app.areas.agents.form.errors().is_empty());
}

#[test]
fn other_harnesses_cannot_open_the_form() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = omp_app(dir.path(), "fake-a");
    update(&mut app, AppMessage::Composer(composer::Message::OpenAgents));
    assert_eq!(app.ctx.screen(), None);

    let new = palette_command(&mut app, "New agent").expect("listed");
    assert!(!new.enabled);
    command(&mut app, "agents.new");
    assert_eq!(app.ctx.screen(), None);
    assert_eq!(app.ctx.notice(), Some(OMP_ONLY));
}

#[test]
fn palette_commands_are_enabled_with_omp() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = omp_app(dir.path(), "omp");
    assert!(palette_command(&mut app, "New agent").expect("listed").enabled);
    command(&mut app, "agents.new");
    assert_eq!(app.ctx.screen(), Some(AreaId::Agents));
}

#[test]
fn save_writes_the_agent_with_exactly_the_ticked_tools() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, agent_dir) = omp_app(dir.path(), "omp");
    update(&mut app, AppMessage::Composer(composer::Message::OpenAgents));
    fill(&mut app, form::Message::Name("reviewer".into()));
    fill(&mut app, form::Message::Description("Reviews diffs".into()));
    fill(&mut app, form::Message::Tool("grep".into(), true));
    fill(&mut app, form::Message::Tool("read".into(), true));
    fill(&mut app, form::Message::Save);

    let path = agent_dir.join("agents").join("reviewer.md");
    assert!(path.is_file(), "{path:?} written");
    let (files, broken) = agents::list(&agent_dir, None);
    assert!(broken.is_empty());
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].definition.name(), "reviewer");
    assert_eq!(files[0].definition.description(), "Reviews diffs");
    assert_eq!(files[0].definition.tools(), Some(vec!["read".to_owned(), "grep".to_owned()]));
    assert!(app.ctx.notice().is_some_and(|notice| notice.starts_with("Saved reviewer. omp sessions started from now on can use it")));
    assert!(app.ctx.notice().is_some_and(|notice| notice.contains("already running")));

    // The list refreshed: the palette offers the new agent.
    assert!(palette_command(&mut app, "Edit agent: reviewer").expect("listed").enabled);
    app.ctx.close_screen();
    command(&mut app, "agents.edit.reviewer");
    assert_eq!(app.ctx.screen(), Some(AreaId::Agents));
    assert_eq!(app.areas.agents.form.name(), "reviewer");
}

#[test]
fn invalid_forms_show_their_errors_and_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, agent_dir) = omp_app(dir.path(), "omp");
    update(&mut app, AppMessage::Composer(composer::Message::OpenAgents));
    fill(&mut app, form::Message::Description("No name".into()));
    fill(&mut app, form::Message::Save);
    assert!(app.areas.agents.form.errors().iter().any(|error| error.field == "name"));
    assert!(!agent_dir.join("agents").exists());
}

/// Saves a new agent `name` with `description` through the form.
pub(super) fn save_new(app: &mut App, name: &str, description: &str) {
    update(app, AppMessage::Agents(Message::Open));
    fill(app, form::Message::Name(name.into()));
    fill(app, form::Message::Description(description.into()));
    fill(app, form::Message::Save);
}

fn description_on_disk(agent_dir: &Path, name: &str) -> Option<String> {
    let (files, _) = agents::list(agent_dir, None);
    files.iter().find(|file| file.definition.name() == name).map(|file| file.definition.description().to_owned())
}

#[test]
fn saving_an_existing_name_replaces_the_file_only_after_confirming() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, agent_dir) = omp_app(dir.path(), "omp");
    save_new(&mut app, "reviewer", "First");
    app.ctx.close_screen();
    save_new(&mut app, "reviewer", "Second");
    // Asked, nothing written yet.
    assert_eq!(description_on_disk(&agent_dir, "reviewer").as_deref(), Some("First"));
    assert!(app.areas.agents.form.pending_question().is_some_and(|question| question == "Replace reviewer?"));

    fill(&mut app, form::Message::Cancel);
    assert_eq!(description_on_disk(&agent_dir, "reviewer").as_deref(), Some("First"));
    assert!(app.areas.agents.form.pending_question().is_none());

    fill(&mut app, form::Message::Save);
    fill(&mut app, form::Message::Confirm);
    assert_eq!(description_on_disk(&agent_dir, "reviewer").as_deref(), Some("Second"));
    assert!(app.ctx.notice().is_some_and(|notice| notice.starts_with("Saved reviewer.")));
}

#[test]
fn replacing_a_disabled_agent_keeps_it_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, agent_dir) = omp_app(dir.path(), "omp");
    save_new(&mut app, "reviewer", "First");
    fill(&mut app, form::Message::Toggle);
    assert!(agent_dir.join("agents").join("reviewer.md.disabled").is_file());
    app.ctx.close_screen();
    save_new(&mut app, "reviewer", "Second");
    fill(&mut app, form::Message::Confirm);
    assert!(!agent_dir.join("agents").join("reviewer.md").exists());
    assert!(agent_dir.join("agents").join("reviewer.md.disabled").is_file());
    assert_eq!(description_on_disk(&agent_dir, "reviewer").as_deref(), Some("Second"));
}

#[test]
fn editing_an_agent_saves_back_to_its_own_file_without_asking() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, agent_dir) = omp_app(dir.path(), "omp");
    save_new(&mut app, "reviewer", "First");
    fill(&mut app, form::Message::Description("Edited".into()));
    fill(&mut app, form::Message::Save);
    assert!(app.areas.agents.form.pending_question().is_none());
    assert_eq!(description_on_disk(&agent_dir, "reviewer").as_deref(), Some("Edited"));
}

#[test]
fn disabling_and_enabling_renames_the_file_and_marks_the_list() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, agent_dir) = omp_app(dir.path(), "omp");
    save_new(&mut app, "reviewer", "First");
    let enabled = agent_dir.join("agents").join("reviewer.md");
    let disabled = agent_dir.join("agents").join("reviewer.md.disabled");

    fill(&mut app, form::Message::Toggle);
    assert!(!enabled.exists() && disabled.is_file());
    let (files, _) = agents::list(&agent_dir, None);
    assert_eq!(files.len(), 1, "a disabled agent stays listed");
    assert!(!files[0].enabled);
    assert_eq!(app.areas.agents.form.is_enabled(&disabled), Some(false));
    assert!(app.ctx.notice().is_some_and(|notice| notice.starts_with("Disabled reviewer.")));
    assert!(palette_command(&mut app, "Edit agent: reviewer").is_some());

    fill(&mut app, form::Message::Toggle);
    assert!(enabled.is_file() && !disabled.exists());
    assert_eq!(app.areas.agents.form.is_enabled(&enabled), Some(true));
    assert!(app.ctx.notice().is_some_and(|notice| notice.starts_with("Enabled reviewer.")));
}

#[test]
fn deleting_removes_the_file_only_after_confirming() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, agent_dir) = omp_app(dir.path(), "omp");
    save_new(&mut app, "reviewer", "First");
    let path = agent_dir.join("agents").join("reviewer.md");

    fill(&mut app, form::Message::Delete);
    assert!(path.is_file());
    assert!(app.areas.agents.form.pending_question().is_some_and(|question| question == "Delete reviewer?"));
    fill(&mut app, form::Message::Cancel);
    assert!(path.is_file());

    fill(&mut app, form::Message::Delete);
    fill(&mut app, form::Message::Confirm);
    assert!(!path.exists());
    assert!(app.ctx.notice().is_some_and(|notice| notice.starts_with("Deleted reviewer.")));
    assert!(palette_command(&mut app, "Edit agent: reviewer").is_none());
}

#[test]
fn a_write_error_keeps_the_form_with_the_reason_at_the_top() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, agent_dir) = omp_app(dir.path(), "omp");
    // A file where the agents directory should be makes the write fail.
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("agents"), "not a directory").unwrap();
    save_new(&mut app, "reviewer", "First");
    assert_eq!(app.ctx.screen(), Some(AreaId::Agents));
    assert!(app.areas.agents.form.errors().iter().any(|error| error.field == "form"));
    assert_eq!(app.areas.agents.form.name(), "reviewer");
}
