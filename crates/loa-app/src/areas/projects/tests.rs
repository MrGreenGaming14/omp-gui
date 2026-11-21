use std::path::{Path, PathBuf};

use loa_core::attr;
use loa_ui_kit::widgets::Msg;
use loa_ui_settings::projects::{self as screen, create};
use loa_ui_shell::sidebar;
use loa_view::shell::SidebarIntent;

use super::{Message, Pick, State};
use crate::app::{App, Message as AppMessage};
use crate::testing::TestApp;

fn area(message: Message) -> AppMessage {
    AppMessage::area(message)
}

fn sidebar_message(message: sidebar::Message) -> AppMessage {
    AppMessage::Shell(loa_ui_shell::Message::Sidebar(message))
}

/// Makes `project` the active one, then starts a session with `text` in it.
fn chat_in(test: &mut TestApp, project: &str, text: &str) -> String {
    test.send(area(Message::Activate(project.to_owned())));
    test.session(text)
}

/// The project recorded on session `id`'s root.
fn session_project(app: &App, id: &str) -> Option<String> {
    let session = app.ctx.runtime.sessions().get(id).expect("the session is open");
    session.writer().with_dom(|dom| dom.root().and_then(|root| root.str_attr(attr::PROJECT)).map(str::to_owned))
}

/// The directories `<folder>/.omgui` links to.
fn linked(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder.join(loa_projects::links::LINKS_DIR)) else { return Vec::new() };
    let mut targets: Vec<_> = entries.filter_map(|entry| std::fs::read_link(entry.unwrap().path()).ok()).collect();
    targets.sort();
    targets
}

#[test]
fn projects_open_and_pin_from_the_palette() {
    let mut test = TestApp::new();
    chat_in(&mut test, "alpha", "hello");
    let titles = test.palette_titles();
    let at = |title: &str| titles.iter().position(|each| each == title).unwrap_or_else(|| panic!("{title} listed: {titles:?}"));
    assert_eq!(at("Pin project: alpha"), at("Open project: alpha") + 1, "each project's Open is followed by its Pin");
    test.command("projects.open:alpha");
    assert_eq!(test.app.ctx.screen(), Some("projects"));
    assert_eq!(test.area::<State>().shown(), Some("alpha"));
    test.command("projects.pin:alpha");
    assert!(test.app.ctx.shell.sidebar().pinned_projects().contains("alpha"));
    test.command("projects.bogus");
    assert_eq!(test.notice(), "Unknown command projects.bogus");
}

#[test]
fn a_project_without_a_folder_keeps_the_current_folder() {
    let mut test = TestApp::new();
    let cwd = test.app.ctx.runtime.cwd().to_path_buf();
    test.send(sidebar_message(sidebar::Message::Emit(SidebarIntent::OpenProject("ghost".into()))));
    assert_eq!(test.app.ctx.runtime.project(), Some("ghost"), "a project is a group, not a folder");
    assert_eq!(test.app.ctx.runtime.cwd(), cwd);
    assert_eq!(test.app.ctx.screen(), Some("projects"));
}

#[test]
fn picking_a_folder_leaves_the_active_project_alone() {
    let mut test = TestApp::new();
    test.send(area(Message::Activate("alpha".into())));
    let other = test.dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let other = other.canonicalize().unwrap();
    test.send(AppMessage::FolderChosen(Ok(Some(other.clone()))));
    assert_eq!(test.app.ctx.runtime.cwd(), other);
    assert_eq!(test.app.ctx.runtime.project(), Some("alpha"), "a folder is not a project: picking one leaves the project alone");
}

#[test]
fn the_new_project_screen_makes_a_project_whose_folder_links_its_sessions() {
    let mut test = TestApp::new();
    let (home, moved) = (test.dir.path().join("launch"), test.dir.path().join("moved"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&moved).unwrap();
    let session_dir = test.app.ctx.runtime.cwd().to_path_buf();
    let session = chat_in(&mut test, "alpha", "first");
    assert_eq!(session_project(&test.app, &session).as_deref(), Some("alpha"));
    assert!(test.app.ctx.shell.sidebar().projects().contains(&"alpha"), "listed from the session's project");

    test.send(sidebar_message(sidebar::Message::Emit(SidebarIntent::NewProject)));
    assert_eq!(test.app.ctx.screen(), Some("projects"), "the + opens the New project screen");
    assert!(test.area::<State>().form().is_some());
    for local in [create::Local::Name(" Launch ".into()), create::Local::Folder(home.display().to_string()), create::Local::Create] {
        test.send(area(Message::Form(Msg::Local(local))));
    }
    assert!(test.app.ctx.shell.sidebar().projects().contains(&"Launch"), "an empty project is listed");
    assert_eq!(test.app.ctx.runtime.project(), Some("Launch"), "a new project becomes the active one");
    assert_eq!(test.app.ctx.runtime.cwd(), home, "new chats start in its folder");
    assert_eq!(session_project(&test.app, &session).as_deref(), Some("alpha"), "making a project moves nothing");
    assert!(linked(&home).is_empty());

    test.send(sidebar_message(sidebar::Message::MoveTo { id: session.clone(), project: Some("Launch".into()) }));
    assert_eq!(session_project(&test.app, &session).as_deref(), Some("Launch"), "moved from its sidebar row");
    assert_eq!(linked(&home), std::slice::from_ref(&session_dir), ".omgui links the folder the session runs in");

    // Changing the folder moves the links and the active folder with it.
    test.send(area(Message::FolderChosen(Pick::Project("Launch".into()), Ok(Some(moved.clone())))));
    assert!(linked(&home).is_empty(), "the old folder's links are gone");
    assert_eq!(linked(&moved), [session_dir]);
    assert_eq!(test.app.ctx.runtime.cwd(), moved);

    // The composer's chip moves the shown session out of every project.
    test.send(sidebar_message(sidebar::Message::OpenSession(session.clone())));
    test.send(AppMessage::Composer(loa_ui_session::composer::Message::Pick(loa_ui_session::composer::PickerId::Project, 0)));
    assert_eq!(session_project(&test.app, &session), None, "moved out by the composer chip");
    assert!(linked(&moved).is_empty(), "and its link goes with it");
}

#[test]
fn a_project_row_starts_a_session_in_it_and_deleting_keeps_its_sessions() {
    let mut test = TestApp::new();
    let session = chat_in(&mut test, "alpha", "first");
    test.app.ctx.shell.sidebar_mut().set_pinned_projects(["alpha".to_owned()]);

    test.send(sidebar_message(sidebar::Message::Emit(SidebarIntent::NewSessionIn("alpha".into()))));
    let last = test.harness.seen().configs.last().and_then(|config| config.project.clone());
    assert_eq!(last.as_deref(), Some("alpha"), "the row's + starts a session in it");

    test.send(sidebar_message(sidebar::Message::Emit(SidebarIntent::DeleteProject("alpha".into()))));
    assert_eq!(test.app.ctx.screen(), Some("projects"), "the row's X asks on the project screen");
    assert_eq!(session_project(&test.app, &session).as_deref(), Some("alpha"), "nothing is deleted before the confirmation");
    test.send(area(Message::Screen(Msg::Local(screen::Local::ConfirmDelete))));

    assert_eq!(session_project(&test.app, &session), None, "its sessions are kept, in no project");
    assert!(test.app.ctx.runtime.sessions().get(&session).is_some());
    assert!(!test.app.ctx.shell.sidebar().projects().contains(&"alpha"));
    assert!(test.app.ctx.shell.sidebar().pinned_projects().is_empty(), "its pin goes");
    assert!(!test.app.ctx.shell.open_projects().iter().any(|open| open == "alpha"), "its tab goes");
    assert_eq!(test.app.ctx.runtime.project(), None, "it is no longer the active project");
    assert_ne!(test.app.ctx.screen(), Some("projects"), "its screen closes");
}
