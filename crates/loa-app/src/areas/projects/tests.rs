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
        settings: loa_settings::projects::ProjectSettings { folder: Some(old), ..Default::default() }, ideas: Err("not read".into()), sessions: Vec::new(),
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

#[test]
fn ideas_notes_are_their_own_file_and_reach_chats_only_when_shared() {
    use iced::widget::text_editor::{Action, Edit};
    use screen::Local::{ConfirmDelete, Ideas, Instructions, Save, ShareIdeas};

    let mut test = TestApp::new();
    let folder = test.dir.path().join("planner");
    std::fs::create_dir_all(&folder).unwrap();
    create_project(&mut test, "Planner", &folder);
    let notes = test.app.ctx.services.storage.dirs.data.join("projects").join("Planner.ideas.md");
    let local = |local| area(Message::Screen(Msg::Local(local)));
    let paste = |text: &str| Action::Edit(Edit::Paste(std::sync::Arc::new(text.into())));
    let new_chat_context = |test: &mut TestApp| {
        test.send(area(Message::NewChatIn("Planner".into())));
        test.harness.seen().configs.last().and_then(|config| config.context.clone()).unwrap_or_default()
    };

    test.send(local(Ideas(paste("- try iroh relays\r\n- add a sidebar"))));
    test.send(local(Save));
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "- try iroh relays\r\n- add a sidebar", "stored as typed, next to the settings");
    assert!(!test.area::<State>().screen().unwrap().is_dirty());
    assert!(!new_chat_context(&mut test).contains("try iroh relays"), "not shared by default");

    test.send(local(ShareIdeas(true)));
    test.send(local(Save));
    let context = new_chat_context(&mut test);
    assert!(context.contains("## Ideas and notes") && context.contains("- try iroh relays\r\n- add a sidebar"), "{context}");

    // Saving other settings leaves notes edited outside since alone.
    std::fs::write(&notes, "edited outside").unwrap();
    test.send(local(Instructions(paste("Use rustls."))));
    test.send(local(Save));
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "edited outside");

    let mut test = test.reopen();
    test.send(area(Message::Open("Planner".into())));
    let overview = test.area::<State>().screen().unwrap().overview().unwrap();
    assert_eq!((overview.ideas.as_deref(), overview.settings.ideas_in_context), (Ok("edited outside"), true));
    test.send(local(ConfirmDelete));
    assert!(!notes.exists(), "deleting the project deletes its notes");
}

#[test]
fn notes_edited_outside_are_never_overwritten_by_an_older_page_edit() {
    use crate::area::Area;
    use iced::widget::text_editor::{Action, Edit};
    use screen::Local::{Ideas, Save};

    let mut test = TestApp::new();
    let folder = test.dir.path().join("planner");
    std::fs::create_dir_all(&folder).unwrap();
    create_project(&mut test, "Planner", &folder);
    let notes = test.app.ctx.services.storage.dirs.data.join("projects").join("Planner.ideas.md");
    let local = |local| area(Message::Screen(Msg::Local(local)));
    let paste = |text: &str| Action::Edit(Edit::Paste(std::sync::Arc::new(text.into())));
    let shown = |test: &TestApp| test.area::<State>().screen().unwrap().overview().unwrap().ideas.clone();

    test.send(local(Ideas(paste("typed in Loa"))));
    std::fs::write(&notes, "edited outside").unwrap();
    test.send(area(Message::Open("Planner".into())));
    assert_eq!(shown(&test).as_deref(), Ok("edited outside"), "a reload in between sees the outside edit");
    test.send(local(Save));
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "edited outside", "and the page doesn't overwrite it");
    let view = test.area::<State>().view_screen(&test.app.ctx).unwrap();
    let reload = loa_ui_kit::testkit::click_label(view, "Reload");
    assert!(test.area::<State>().screen().unwrap().is_dirty(), "the page edit stays until reloaded");

    for message in reload { test.send(area(message)); }
    assert!(!test.area::<State>().screen().unwrap().is_dirty(), "reloading takes the notes from the file");
    test.send(local(Ideas(paste("- then typed in Loa\n"))));
    test.send(local(Save));
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "- then typed in Loa\nedited outside");
    test.send(local(Ideas(paste("- and again\n"))));
    test.send(local(Save));
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "- then typed in Loa\n- and again\nedited outside", "a save is the base of the next");
}

#[test]
fn unreadable_notes_leave_the_page_and_the_rest_of_the_context_working() {
    use screen::Local::{Instructions, Save, ShareIdeas};

    let mut test = TestApp::new();
    let folder = test.dir.path().join("planner");
    std::fs::create_dir_all(&folder).unwrap();
    create_project(&mut test, "Planner", &folder);
    let notes = test.app.ctx.services.storage.dirs.data.join("projects").join("Planner.ideas.md");
    std::fs::write(&notes, [0xff, 0xfe, b'h', 0, b'i', 0]).unwrap();
    let local = |local| area(Message::Screen(Msg::Local(local)));
    test.send(area(Message::Open("Planner".into())));
    let overview = test.area::<State>().screen().unwrap().overview().expect("the page still loads");
    assert!(overview.ideas.as_ref().is_err_and(|error| error.contains("Planner.ideas.md")), "{:?}", overview.ideas);

    let paste = std::sync::Arc::new("Use rustls.".to_owned());
    test.send(local(Instructions(iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Paste(paste)))));
    test.send(local(ShareIdeas(true)));
    test.send(local(Save));
    assert_eq!(std::fs::read(&notes).unwrap(), [0xff, 0xfe, b'h', 0, b'i', 0], "the notes file is left as it is");
    test.send(area(Message::NewChatIn("Planner".into())));
    let context = test.harness.seen().configs.last().and_then(|config| config.context.clone()).unwrap_or_default();
    assert!(context.contains("Use rustls.") && !context.contains("## Ideas and notes"), "{context}");
}

/// The ids of the shown project's terminal history, sorted.
fn listed_history(test: &TestApp) -> Vec<String> {
    use loa_view::settings::projects::OmpHistory;

    let Some(OmpHistory::Ready(rows)) = test.area::<State>().screen().unwrap().omp_history() else { panic!("expected a loaded terminal history") };
    let mut ids: Vec<_> = rows.iter().map(|row| row.id.clone()).collect();
    ids.sort();
    ids
}

#[test]
fn deleting_a_terminal_conversation_removes_it_from_omp_storage_only() {
    use loa_view::settings::projects::Intent;

    let mut test = TestApp::new();
    let (folder, other) = (test.dir.path().join("project"), test.dir.path().join("other"));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let storage = test.dir.path().join("omp/sessions/saved");
    let doomed = saved_omp_session(&storage, "doomed", &folder, "Old terminal chat");
    let sidecar = doomed.with_extension("");
    std::fs::create_dir_all(&sidecar).unwrap();
    std::fs::write(sidecar.join("artifact.md"), "notes").unwrap();
    let kept = saved_omp_session(&storage, "kept", &folder, "Kept terminal chat");
    create_project(&mut test, "Project", &folder);
    assert_eq!(listed_history(&test), ["doomed", "kept"]);

    test.send(area(Message::Screen(Msg::Local(screen::Local::DeleteOmpHistory("doomed".into())))));
    assert!(doomed.is_file(), "nothing is deleted before the confirmation");
    let delete = test.app.update(area(Message::Screen(Msg::Local(screen::Local::ConfirmDeleteOmpHistory))));
    test.send(area(Message::Screen(Msg::Emit(Intent::OpenOmpHistory("kept".into())))));
    assert!(test.app.ctx.runtime.library().store().list().unwrap().is_empty(), "no row opens during a deletion");
    test.send(area(Message::FolderChosen(Pick::Project("Project".into()), Ok(Some(other)))));
    assert!(test.notice().contains("Wait for the current project operation"), "the folder stays during a deletion");
    test.run(delete);
    assert!(!doomed.exists() && !sidecar.exists() && kept.is_file());
    assert_eq!(listed_history(&test), ["kept"]);

    let mut test = test.reopen();
    test.send(area(Message::Open("Project".into())));
    assert_eq!(listed_history(&test), ["kept"], "a fresh scan doesn't bring it back");
}

#[test]
fn a_failed_deletion_is_reported_and_the_history_listed_again() {
    let mut test = TestApp::new();
    let folder = test.dir.path().join("project");
    std::fs::create_dir_all(&folder).unwrap();
    let storage = test.dir.path().join("omp/sessions/saved");
    let gone = saved_omp_session(&storage, "gone", &folder, "Removed in a terminal");
    saved_omp_session(&storage, "kept", &folder, "Kept terminal chat");
    create_project(&mut test, "Project", &folder);
    std::fs::remove_file(gone).unwrap();

    test.send(area(Message::Screen(Msg::Local(screen::Local::DeleteOmpHistory("gone".into())))));
    test.send(area(Message::Screen(Msg::Local(screen::Local::ConfirmDeleteOmpHistory))));
    assert!(test.notice().contains("Can't delete OMP terminal session"), "{}", test.notice());
    assert_eq!(listed_history(&test), ["kept"]);
    test.send(area(Message::Screen(Msg::Local(screen::Local::DeleteOmpHistory("kept".into())))));
    test.send(area(Message::Screen(Msg::Local(screen::Local::ConfirmDeleteOmpHistory))));
    assert!(listed_history(&test).is_empty(), "the history is free again after a failure");
}
