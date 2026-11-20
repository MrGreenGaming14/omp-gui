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
fn omp_app(dir: &Path, harness: &'static str) -> (App, PathBuf) {
    let mut app = fake_app(dir, &[harness]);
    let agent_dir = dir.join("agent");
    app.areas.agents.use_fixed(Fixed {
        agent_dir: agent_dir.clone(),
        models: vec!["@slow".into(), "anthropic/claude-sonnet".into()],
        tools: TOOLS.map(String::from).to_vec(),
    });
    (app, agent_dir)
}

fn fill(app: &mut App, message: form::Message) {
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
    assert_eq!(app.ctx.notice(), Some("Saved agent reviewer"));

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

#[test]
fn saving_never_replaces_an_existing_agent() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, agent_dir) = omp_app(dir.path(), "omp");
    update(&mut app, AppMessage::Composer(composer::Message::OpenAgents));
    fill(&mut app, form::Message::Name("reviewer".into()));
    fill(&mut app, form::Message::Description("First".into()));
    fill(&mut app, form::Message::Save);
    app.ctx.close_screen();
    update(&mut app, AppMessage::Agents(Message::Open));
    fill(&mut app, form::Message::Name("reviewer".into()));
    fill(&mut app, form::Message::Description("Second".into()));
    fill(&mut app, form::Message::Save);
    assert!(app.ctx.notice().is_some_and(|notice| notice.starts_with("Can't save agent reviewer")));
    let (files, _) = agents::list(&agent_dir, None);
    assert_eq!(files[0].definition.description(), "First");
}
