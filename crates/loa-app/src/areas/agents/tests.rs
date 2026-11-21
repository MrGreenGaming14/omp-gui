//! The agents area driven through `App::update`: omp's directory and models come from the test
//! app's offline model backend and the tools are fixed, so nothing here starts omp.

use std::path::{Path, PathBuf};

use loa_ui_kit::widgets::Msg;
use loa_ui_settings::agents::Local;

use super::*;
use crate::app::Message as AppMessage;
use crate::testing::{FakeHarness, TestApp};

const TOOLS: [&str; 4] = ["read", "edit", "grep", "bash"];

/// An app whose only harness is `harness`, with omp's agent directory at `<home>/omp`.
pub(super) fn omp_app(harness: &'static str) -> (TestApp, PathBuf) {
    let mut test = TestApp::with(FakeHarness::new(harness));
    test.area_mut::<State>().use_tools(TOOLS.map(String::from).to_vec());
    let agent_dir = test.dir.path().join("omp");
    (test, agent_dir)
}

pub(super) fn open(test: &mut TestApp) {
    test.send(AppMessage::area(Message::Open));
}

pub(super) fn fill(test: &mut TestApp, message: Local) {
    test.send(AppMessage::area(Message::Screen(Msg::Local(message))));
}

fn form(test: &TestApp) -> &screen::State {
    test.area::<State>().form()
}

/// Whether the palette lists a command titled `title`.
fn listed(test: &mut TestApp, title: &str) -> bool {
    test.palette_titles().iter().any(|listed| listed == title)
}

#[test]
fn the_form_opens_with_omp() {
    let (mut test, _) = omp_app("omp");
    open(&mut test);
    assert_eq!(test.app.ctx.screen(), Some(State::ID));
    assert!(form(&test).errors().is_empty());
    assert!(test.area::<State>().configs_body(&test.app.ctx).is_some(), "Configs › Agents draws the form");
}

#[test]
fn other_harnesses_cannot_open_the_form() {
    let (mut test, _) = omp_app("fake-a");
    open(&mut test);
    assert_eq!(test.app.ctx.screen(), None);
    assert!(test.area::<State>().configs_body(&test.app.ctx).is_none(), "Configs › Agents keeps the file list");

    assert!(!listed(&mut test, "New agent"));
    test.command("agents.new");
    assert_eq!(test.app.ctx.screen(), None);
    assert_eq!(test.notice(), OMP_ONLY);
}

#[test]
fn palette_commands_are_listed_with_omp() {
    let (mut test, _) = omp_app("omp");
    assert!(listed(&mut test, "New agent"));
    test.command("agents.new");
    assert_eq!(test.app.ctx.screen(), Some(State::ID));
}

#[test]
fn save_writes_the_agent_with_exactly_the_ticked_tools() {
    let (mut test, agent_dir) = omp_app("omp");
    open(&mut test);
    fill(&mut test, Local::Name("reviewer".into()));
    fill(&mut test, Local::Description("Reviews diffs".into()));
    fill(&mut test, Local::Tool("grep".into(), true));
    fill(&mut test, Local::Tool("read".into(), true));
    fill(&mut test, Local::Save);

    let path = agent_dir.join("agents").join("reviewer.md");
    assert!(path.is_file(), "{path:?} written");
    let (files, broken) = agents::list(&agent_dir, None);
    assert!(broken.is_empty());
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].definition.name(), "reviewer");
    assert_eq!(files[0].definition.description(), "Reviews diffs");
    assert_eq!(files[0].definition.tools(), Some(vec!["read".to_owned(), "grep".to_owned()]));
    assert!(test.notice().starts_with("Saved reviewer. omp sessions started from now on can use it"));
    assert!(test.notice().contains("already running"));

    // The list refreshed: the palette offers the new agent.
    assert!(listed(&mut test, "Edit agent: reviewer"));
    test.app.ctx.close_screen();
    test.command("agents.edit.reviewer");
    assert_eq!(test.app.ctx.screen(), Some(State::ID));
    assert_eq!(form(&test).name(), "reviewer");
}

#[test]
fn invalid_forms_show_their_errors_and_write_nothing() {
    let (mut test, agent_dir) = omp_app("omp");
    open(&mut test);
    fill(&mut test, Local::Description("No name".into()));
    fill(&mut test, Local::Save);
    assert!(form(&test).errors().iter().any(|error| error.field == "name"));
    assert!(!agent_dir.join("agents").exists());
}

#[test]
fn save_with_an_empty_name_shows_the_error_and_writes_nothing() {
    let (mut test, agent_dir) = omp_app("omp");
    open(&mut test);
    fill(&mut test, Local::Description("Does things".into()));
    fill(&mut test, Local::Save);
    assert!(form(&test).errors().iter().any(|error| error.field == "name"));
    assert!(form(&test).errors().iter().all(|error| error.field != "description"));
    // The errors follow the edits once shown.
    fill(&mut test, Local::Name("does-things".into()));
    assert!(form(&test).errors().is_empty());
    assert!(!agent_dir.join("agents").exists());
}

#[test]
fn an_unknown_tool_of_a_copied_agent_is_reported() {
    let (mut test, agent_dir) = omp_app("omp");
    test.area_mut::<State>().use_tools(vec!["read".into()]);
    std::fs::create_dir_all(agent_dir.join("agents")).unwrap();
    std::fs::write(agent_dir.join("agents").join("reviewer.md"), "---\nname: reviewer\ndescription: Reviews\ntools:\n  - read\n  - grep\n---\n").unwrap();
    open(&mut test);
    let template = form(&test).templates().remove(1);
    fill(&mut test, Local::Template(template));
    fill(&mut test, Local::Save);
    assert!(form(&test).errors().iter().any(|error| error.field == "tools" && error.message.contains("grep")));
    assert!(!agent_dir.join("agents").join("reviewer-copy.md").exists());
}

#[test]
fn a_copy_keeps_the_keys_the_form_has_no_field_for() {
    let (mut test, agent_dir) = omp_app("omp");
    std::fs::create_dir_all(agent_dir.join("agents")).unwrap();
    std::fs::write(agent_dir.join("agents").join("plan.md"), "---\nname: plan\ndescription: Plans\noutput:\n  type: string\n---\nPlan.\n").unwrap();
    open(&mut test);
    let template = form(&test).templates().remove(1);
    fill(&mut test, Local::Template(template));
    fill(&mut test, Local::Save);
    let copy = std::fs::read_to_string(agent_dir.join("agents").join("plan-copy.md")).unwrap();
    assert!(copy.contains("name: plan-copy") && copy.contains("output:\n  type: string\n"), "{copy}");
}

/// Saves a new agent `name` with `description` through the form.
pub(super) fn save_new(test: &mut TestApp, name: &str, description: &str) {
    open(test);
    fill(test, Local::Name(name.into()));
    fill(test, Local::Description(description.into()));
    fill(test, Local::Save);
}

fn description_on_disk(agent_dir: &Path, name: &str) -> Option<String> {
    let (files, _) = agents::list(agent_dir, None);
    files.iter().find(|file| file.definition.name() == name).map(|file| file.definition.description().to_owned())
}

#[test]
fn saving_an_existing_name_replaces_the_file_only_after_confirming() {
    let (mut test, agent_dir) = omp_app("omp");
    save_new(&mut test, "reviewer", "First");
    test.app.ctx.close_screen();
    save_new(&mut test, "reviewer", "Second");
    // Asked, nothing written yet.
    assert_eq!(description_on_disk(&agent_dir, "reviewer").as_deref(), Some("First"));
    assert!(form(&test).pending_question().is_some_and(|question| question == "Replace reviewer?"));

    fill(&mut test, Local::Cancel);
    assert_eq!(description_on_disk(&agent_dir, "reviewer").as_deref(), Some("First"));
    assert!(form(&test).pending_question().is_none());

    fill(&mut test, Local::Save);
    fill(&mut test, Local::Confirm);
    assert_eq!(description_on_disk(&agent_dir, "reviewer").as_deref(), Some("Second"));
    assert!(test.notice().starts_with("Saved reviewer."));
}

#[test]
fn replacing_a_disabled_agent_keeps_it_disabled() {
    let (mut test, agent_dir) = omp_app("omp");
    save_new(&mut test, "reviewer", "First");
    fill(&mut test, Local::Toggle);
    assert!(agent_dir.join("agents").join("reviewer.md.disabled").is_file());
    test.app.ctx.close_screen();
    save_new(&mut test, "reviewer", "Second");
    fill(&mut test, Local::Confirm);
    assert!(!agent_dir.join("agents").join("reviewer.md").exists());
    assert!(agent_dir.join("agents").join("reviewer.md.disabled").is_file());
    assert_eq!(description_on_disk(&agent_dir, "reviewer").as_deref(), Some("Second"));
}

#[test]
fn editing_an_agent_saves_back_to_its_own_file_without_asking() {
    let (mut test, agent_dir) = omp_app("omp");
    save_new(&mut test, "reviewer", "First");
    fill(&mut test, Local::Description("Edited".into()));
    fill(&mut test, Local::Save);
    assert!(form(&test).pending_question().is_none());
    assert_eq!(description_on_disk(&agent_dir, "reviewer").as_deref(), Some("Edited"));
}

#[test]
fn disabling_and_enabling_renames_the_file_and_marks_the_list() {
    let (mut test, agent_dir) = omp_app("omp");
    save_new(&mut test, "reviewer", "First");
    let enabled = agent_dir.join("agents").join("reviewer.md");
    let disabled = agent_dir.join("agents").join("reviewer.md.disabled");

    fill(&mut test, Local::Toggle);
    assert!(!enabled.exists() && disabled.is_file());
    let (files, _) = agents::list(&agent_dir, None);
    assert_eq!(files.len(), 1, "a disabled agent stays listed");
    assert!(!files[0].enabled);
    assert_eq!(form(&test).is_enabled(&disabled), Some(false));
    assert!(test.notice().starts_with("Disabled reviewer."));
    assert!(listed(&mut test, "Edit agent: reviewer"));

    fill(&mut test, Local::Toggle);
    assert!(enabled.is_file() && !disabled.exists());
    assert_eq!(form(&test).is_enabled(&enabled), Some(true));
    assert!(test.notice().starts_with("Enabled reviewer."));
}

#[test]
fn deleting_removes_the_file_only_after_confirming() {
    let (mut test, agent_dir) = omp_app("omp");
    save_new(&mut test, "reviewer", "First");
    let path = agent_dir.join("agents").join("reviewer.md");

    fill(&mut test, Local::Delete);
    assert!(path.is_file());
    assert!(form(&test).pending_question().is_some_and(|question| question == "Delete reviewer?"));
    fill(&mut test, Local::Cancel);
    assert!(path.is_file());

    fill(&mut test, Local::Delete);
    fill(&mut test, Local::Confirm);
    assert!(!path.exists());
    assert!(test.notice().starts_with("Deleted reviewer."));
    assert!(!listed(&mut test, "Edit agent: reviewer"));
}

#[test]
fn a_write_error_keeps_the_form_with_the_reason_at_the_top() {
    let (mut test, agent_dir) = omp_app("omp");
    // A file where the agents directory should be makes the write fail.
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("agents"), "not a directory").unwrap();
    save_new(&mut test, "reviewer", "First");
    assert_eq!(test.app.ctx.screen(), Some(State::ID));
    assert!(form(&test).errors().iter().any(|error| error.field == "form"));
    assert_eq!(form(&test).name(), "reviewer");
}
