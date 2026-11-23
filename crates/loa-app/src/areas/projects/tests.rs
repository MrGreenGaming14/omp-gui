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

fn saved_omp_session(dir: &Path, id: &str, cwd: &Path, title: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{id}.jsonl"));
    let header = serde_json::json!({"type": "session", "version": 3, "id": id, "cwd": cwd, "title": title});
    let message = serde_json::json!({"type": "message", "id": "first", "parentId": null, "message": {"role": "user", "content": "Read the project guide."}});
    std::fs::write(&path, format!("{header}\n{message}\n")).unwrap();
    path
}

fn create_project(test: &mut TestApp, name: &str, folder: &Path) {
    test.send(area(Message::New));
    for local in [create::Local::Name(name.into()), create::Local::Folder(folder.display().to_string()), create::Local::Create] {
        test.send(area(Message::Form(Msg::Local(local))));
    }
}

#[test]
fn project_lists_terminal_history_then_opens_one_independent_copy() {
    use loa_view::settings::projects::OmpHistory;

    let mut test = TestApp::new();
    let folder = test.dir.path().join("Existing project");
    let subfolder = folder.join("child");
    std::fs::create_dir_all(&subfolder).unwrap();
    let storage = test.dir.path().join("omp/sessions/encoded-workspace");
    let source = saved_omp_session(&storage, "terminal-one", &folder, "Saved terminal chat");
    saved_omp_session(&storage, "terminal-two", &subfolder, "Child folder chat");
    let original = std::fs::read(&source).unwrap();
    create_project(&mut test, "Existing", &folder);
    let screen = test.area::<State>().screen().unwrap();
    let Some(OmpHistory::Ready(rows)) = screen.omp_history() else { panic!("expected a loaded terminal history") };
    assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["terminal-one"]);
    assert!(test.app.ctx.runtime.library().store().list().unwrap().is_empty(), "listing must not import");
    let messages = loa_ui_kit::testkit::click_label(screen.view(), "Saved terminal chat");
    assert!(!messages.is_empty());
    for message in messages { test.send(area(Message::Screen(message))); }
    let id = test.active().expect("the imported conversation is open").to_owned();
    assert_eq!(session_project(&test.app, &id).as_deref(), Some("Existing"));
    let dom = test.app.ctx.runtime.library().load(&id).unwrap();
    assert!(dom.iter().any(|node| node.text == "Read the project guide."));
    let root = dom.root().unwrap();
    use loa_harness::omp::native::X_SESSION_FILE;
    let copy = PathBuf::from(root.str_attr(X_SESSION_FILE).unwrap());
    assert_ne!(copy, source);
    assert_eq!(std::fs::read(copy).unwrap(), original);
    assert_eq!(std::fs::read(&source).unwrap(), original);

    let mut test = test.reopen();
    test.send(area(Message::Open("Existing".into())));
    let screen = test.area::<State>().screen().unwrap();
    assert!(matches!(screen.omp_history(), Some(OmpHistory::Ready(rows)) if rows.is_empty()));
    assert_eq!(screen.overview().unwrap().sessions[0].id, id);
    assert_eq!(test.app.ctx.runtime.library().store().list().unwrap().len(), 1);
}

#[test]
fn changed_terminal_file_cannot_be_imported_into_the_wrong_project() {
    use loa_view::settings::projects::Intent;

    let mut test = TestApp::new();
    let (folder, other) = (test.dir.path().join("project"), test.dir.path().join("other"));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let storage = test.dir.path().join("omp/sessions/saved");
    saved_omp_session(&storage, "chat", &folder, "Moving chat");
    create_project(&mut test, "Project", &folder);
    saved_omp_session(&storage, "chat", &other, "Moving chat");
    test.send(area(Message::Screen(Msg::Emit(Intent::OpenOmpHistory("chat".into())))));
    assert!(test.notice().contains("terminal session changed"));
    assert!(test.app.ctx.runtime.library().store().list().unwrap().is_empty());
    assert_eq!(test.app.ctx.screen(), Some("projects"));
}

#[test]
fn obsolete_overview_cannot_restore_the_previous_folders_history() {
    use loa_view::settings::projects::OmpHistory;

    let mut test = TestApp::new();
    let (old, new) = (test.dir.path().join("old"), test.dir.path().join("new"));
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&new).unwrap();
    let storage = test.dir.path().join("omp/sessions/saved");
    saved_omp_session(&storage, "old-chat", &old, "Old chat");
    saved_omp_session(&storage, "new-chat", &new, "New chat");
    create_project(&mut test, "Project", &old);
    let obsolete = test.area::<State>().overview_request.get();
    test.send(area(Message::FolderChosen(Pick::Project("Project".into()), Ok(Some(new)))));
    let stale = loa_settings::projects::ProjectOverview {
        name: "Project".into(), cwd: Some(old.to_string_lossy().into_owned()),
        settings: loa_settings::projects::ProjectSettings { folder: Some(old), ..Default::default() }, sessions: Vec::new(),
    };
    test.send(area(Message::Overview(obsolete, "Project".into(), Ok(stale))));
    let screen = test.area::<State>().screen().unwrap();
    assert!(matches!(screen.omp_history(), Some(OmpHistory::Ready(rows)) if rows.len() == 1 && rows[0].id == "new-chat"));
}

#[test]
fn an_unfinished_import_is_not_visible_in_the_session_store() {
    use loa_session::Error;

    let test = TestApp::new();
    let library = test.app.ctx.runtime.library();
    let cwd = test.dir.path();
    let native = saved_omp_session(&cwd.join("external"), "source", cwd, "Saved chat");
    let result = library.import_staged("attempt", "owner", "Project", |_, _, sink| {
        loa_harness::omp::native::import(&native, sink, "owner")?;
        assert!(library.store().list().unwrap().is_empty());
        Err(Error::Port("import interrupted".into()))
    });
    assert!(matches!(result, Err(Error::Port(message)) if message == "import interrupted"));
    assert!(library.store().list().unwrap().is_empty());
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
