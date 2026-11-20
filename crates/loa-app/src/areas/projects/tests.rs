use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use iced::widget::text_editor::{Action, Edit};
use loa_adapter::{Adapter, AdapterError, Answer, Capabilities, HarnessSession, Prompt, SessionState};
use loa_bus::Policy;
use loa_core::{Dom, Draft, ElementKind, NodeId, PatchSink, Role, TurnStatus, Value, attr, now_ms};
use loa_projects::{PROJECT_READ, PROJECT_SEARCH, SummaryAnswer};
use loa_ui_shell as shell;

use super::*;
use crate::app::tests::{active, deliver, prompt, run, services, update};
use crate::app::{App, Message as AppMessage};

const HARNESS: &str = "fake-p";

/// A harness that takes host tools and project context, records every
/// config it starts with, and answers each prompt with one finished turn.
struct ProjectAdapter {
    configs: Arc<Mutex<Vec<SessionConfig>>>,
}

#[async_trait::async_trait]
impl Adapter for ProjectAdapter {
    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { host_tools: true, ..Capabilities::default() }
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        let attrs: [(&str, Value); 6] = [
            (attr::ID, config.session_id.clone().into()),
            (attr::NODE, config.node.clone().into()),
            (attr::HARNESS, HARNESS.into()),
            (attr::CREATED, Value::Int(now_ms())),
            (attr::CWD, config.cwd.display().to_string().into()),
            (attr::PROJECT, config.project.clone().unwrap_or_default().into()),
        ];
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?.node;
        self.configs.lock().unwrap().push(config);
        Ok(Box::new(Turns { sink, root }))
    }

    async fn resume(&self, _: SessionConfig, _: &Dom, _: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        Err(AdapterError::Unsupported { harness: HARNESS, what: "resume" })
    }
}

struct Turns {
    sink: Arc<dyn PatchSink>,
    root: NodeId,
}

#[async_trait::async_trait]
impl HarnessSession for Turns {
    async fn send(&self, prompt: Prompt) -> loa_adapter::Result<()> {
        let started: [(&str, Value); 2] = [(attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, now_ms().into())];
        let turn = self.sink.commit(Draft::insert(self.root, ElementKind::Turn, started, ""))?.node;
        self.sink.commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from(Role::User))], prompt.text.as_str()))?;
        let reply = format!("Done: {}. The endpoint listens on port 31415.", prompt.text);
        self.sink.commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from(Role::Assistant))], reply.as_str()))?;
        self.sink.commit(Draft::set(turn, attr::ENDED, now_ms()))?;
        self.sink.commit(Draft::set(turn, attr::STATUS, TurnStatus::Done))?;
        Ok(())
    }

    async fn interrupt(&self) -> loa_adapter::Result<()> {
        Ok(())
    }

    async fn answer(&self, decision: NodeId, _: Answer) -> loa_adapter::Result<()> {
        Err(AdapterError::UnknownDecision(decision))
    }

    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn state(&self) -> SessionState {
        SessionState::Idle
    }

    async fn wait_idle(&self) -> loa_adapter::Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> loa_adapter::Result<()> {
        Ok(())
    }
}

/// Digests every session as the iroh setup on port 31415.
struct FakeSummarizer;

#[async_trait::async_trait]
impl Summarizer for FakeSummarizer {
    async fn summarize(&self, _: &str, _: Option<&str>) -> loa_projects::Result<SummaryAnswer> {
        let text = r#"{"summary":"Set up the iroh endpoint on port 31415.","decisions":["port 31415"],"files":[{"path":"src/net.rs","note":"endpoint"}],"open_items":[],"pointers":["T1"]}"#;
        Ok(SummaryAnswer { text: text.to_owned(), model: Some("smol".into()) })
    }
}

/// The app working in `<dir>/work` on [`ProjectAdapter`], its projects
/// area digesting with [`FakeSummarizer`] right after a turn ends.
fn project_app(dir: &Path) -> (App, Arc<Mutex<Vec<SessionConfig>>>) {
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut services = services(&work, Vec::new());
    let configs = Arc::new(Mutex::new(Vec::new()));
    services.harnesses.add(Arc::new(ProjectAdapter { configs: Arc::clone(&configs) }), HARNESS, true, None, None);
    let (mut app, _) = App::build(services);
    let config = ServiceConfig { debounce: Duration::from_millis(10), ..ServiceConfig::default() };
    let (projects, task) = State::with(&mut app.ctx, Arc::new(FakeSummarizer), config);
    app.areas.projects = projects;
    run(&mut app, task.map(AppMessage::Projects));
    (app, configs)
}

fn projects(app: &mut App, message: Message) {
    update(app, AppMessage::Projects(message));
}

fn on_screen(app: &mut App, message: screen::Message) {
    projects(app, Message::Screen(message));
}

/// Opens `project` from the sidebar, starts a chat there with the screen's
/// "New chat", sends `text` and delivers the turn. Returns the session id.
fn chat_in(app: &mut App, project: &str, text: &str) -> String {
    let open = shell::Message::Sidebar(shell::sidebar::Message::OpenProject(project.to_owned()));
    update(app, AppMessage::Shell(open));
    on_screen(app, screen::Message::NewChat);
    let session = app.ctx.sessions.active().expect("the new chat runs");
    let id = session.id().clone();
    let (_, mut feed) = session.writer().subscribe(Policy::every_patch(1024));
    prompt(app, text);
    deliver(app, &id, &mut feed);
    id.to_string()
}

/// The folder session `id` runs in, as recorded in its journal.
fn session_cwd(app: &App, id: &str) -> Option<String> {
    let session = app.ctx.sessions.get(id).unwrap();
    session.writer().with_dom(|dom| dom.root().and_then(|root| root.str_attr(attr::CWD)).map(str::to_owned))
}

fn open_from_sidebar(app: &mut App, project: &str) {
    let open = shell::Message::Sidebar(shell::sidebar::Message::OpenProject(project.to_owned()));
    update(app, AppMessage::Shell(open));
}

#[test]
fn selecting_a_project_switches_the_active_folder_and_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (mut app, configs) = project_app(dir.path());
    // One running chat per project, each in its own folder.
    app.ctx.services.set_active("alpha".into(), a.clone());
    let in_a = chat_in(&mut app, "alpha", "work on a");
    app.ctx.services.set_active("beta".into(), b.clone());
    let in_b = chat_in(&mut app, "beta", "work on b");
    assert_eq!(session_cwd(&app, &in_a), Some(a.display().to_string()));
    assert_eq!(session_cwd(&app, &in_b), Some(b.display().to_string()));

    open_from_sidebar(&mut app, "alpha");
    assert_eq!(app.ctx.services.project, "alpha");
    assert_eq!(app.ctx.services.cwd, a);
    assert_eq!(app.files.root(), a);
    assert_eq!(app.terminal.new_tab().cwd, a, "new terminals open in the active folder");
    assert_eq!(app.ctx.screen(), Some(AreaId::Projects), "the project screen lists its chats");

    open_from_sidebar(&mut app, "beta");
    assert_eq!(app.ctx.services.project, "beta");
    assert_eq!(app.ctx.services.cwd, b);
    assert_eq!(app.files.root(), b);
    assert_eq!(app.terminal.new_tab().cwd, b);
    update(&mut app, AppMessage::NewSession);
    {
        let configs = configs.lock().unwrap();
        let config = configs.last().unwrap();
        assert_eq!(config.cwd, b, "a new chat starts in the active folder");
        assert_eq!(config.project.as_deref(), Some("beta"));
    }
    assert_eq!(session_cwd(&app, &in_a), Some(a.display().to_string()), "a running session keeps its folder");

    // The workspace project goes back to the launch folder.
    open_from_sidebar(&mut app, "work");
    assert_eq!(app.ctx.services.cwd, dir.path().join("work"));
    open_from_sidebar(&mut app, "alpha");
    drop(app);

    let (restarted, _) = App::build(services(&dir.path().join("work"), Vec::new()));
    assert_eq!(restarted.ctx.services.project, "alpha");
    assert_eq!(restarted.ctx.services.cwd, a);
    assert_eq!(restarted.files.root(), a);
    assert_eq!(restarted.terminal.new_tab().cwd, a);
}

#[test]
fn the_sidebar_file_list_follows_the_active_project() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("in_a.txt"), "a").unwrap();
    std::fs::write(b.join("in_b.txt"), "b").unwrap();
    let (mut app, _) = project_app(dir.path());
    app.ctx.services.set_active("alpha".into(), a.clone());
    chat_in(&mut app, "alpha", "work on a");
    app.ctx.services.set_active("beta".into(), b.clone());
    chat_in(&mut app, "beta", "work on b");

    open_from_sidebar(&mut app, "alpha");
    assert_eq!(app.files.root(), a);
    assert!(app.files.visible().any(|path| path == Path::new("in_a.txt")));

    open_from_sidebar(&mut app, "beta");
    assert_eq!(app.files.root(), b, "the sidebar list is re-rooted at the opened project");
    let listed: Vec<_> = app.files.visible().collect();
    assert_eq!(listed, [Path::new("in_b.txt")]);
    let _ = app.view();
}

#[test]
fn project_tabs_list_activated_projects_and_the_shortcuts_switch_between_them() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (mut app, _) = project_app(dir.path());
    assert!(!app.ctx.shell.project_tabs(), "off by default");
    assert!(app.ctx.shell.open_projects().is_empty());

    app.ctx.services.set_active("alpha".into(), a.clone());
    let in_alpha = chat_in(&mut app, "alpha", "work on a");
    app.ctx.services.set_active("beta".into(), b.clone());
    chat_in(&mut app, "beta", "work on b");
    assert_eq!(app.ctx.shell.open_projects(), ["alpha", "beta"], "both projects have a tab");

    // The palette toggle turns the strip on and saves it.
    let toggle = loa_ui_palette::CommandId::new("project.tabs.toggle");
    let task = app.command(&toggle);
    run(&mut app, task);
    assert!(app.ctx.shell.project_tabs());
    assert!(app.ctx.services.config.load().unwrap().config.project_tabs);

    // Next and previous switch the active project, wrapping around.
    open_from_sidebar(&mut app, "alpha");
    assert_eq!(app.ctx.services.project, "alpha");
    let task = app.command(&loa_ui_palette::ids::PROJECT_TAB_NEXT);
    run(&mut app, task);
    assert_eq!((app.ctx.services.project.as_str(), app.ctx.services.cwd.clone()), ("beta", b.clone()));
    let task = app.command(&loa_ui_palette::ids::PROJECT_TAB_NEXT);
    run(&mut app, task);
    assert_eq!(app.ctx.services.project, "alpha", "wraps to the first tab");
    let task = app.command(&loa_ui_palette::ids::PROJECT_TAB_PREVIOUS);
    run(&mut app, task);
    assert_eq!(app.ctx.services.project, "beta");

    // Closing a tab leaves its session open and the other tab alone.
    update(&mut app, AppMessage::Shell(shell::Message::CloseProjectTab("alpha".into())));
    assert_eq!(app.ctx.shell.open_projects(), ["beta"]);
    assert!(app.ctx.sessions.get(&in_alpha).is_some(), "closing a tab keeps the session");
    open_from_sidebar(&mut app, "alpha");
    assert_eq!(app.ctx.shell.open_projects(), ["beta", "alpha"], "activating brings the tab back");

    let task = app.command(&toggle);
    run(&mut app, task);
    assert!(!app.ctx.shell.project_tabs());
}

#[test]
fn a_project_without_a_folder_only_opens_its_screen() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = project_app(dir.path());
    let cwd = app.ctx.services.cwd.clone();
    open_from_sidebar(&mut app, "ghost");
    assert_eq!(app.ctx.services.cwd, cwd);
    assert_eq!(app.ctx.screen(), Some(AreaId::Projects));
    assert_eq!(app.ctx.notice(), Some("ghost has no folder yet; start a chat in it first"));
}

#[test]
fn project_screen_lists_sessions_saves_settings_and_excludes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, configs) = project_app(dir.path());
    assert_eq!(app.ctx.shell.sidebar().projects(), ["work"], "the workspace's project before any session");

    let session = chat_in(&mut app, "alpha", "set up iroh");
    {
        let configs = configs.lock().unwrap();
        let config = configs.last().unwrap();
        assert_eq!(config.project.as_deref(), Some("alpha"), "New chat starts in the screen's project");
        assert!(config.context.is_none(), "alpha has nothing to share yet");
        let tools: Vec<&str> = config.host_tools.iter().map(|tool| tool.name.as_str()).collect();
        assert!(tools.contains(&PROJECT_SEARCH) && tools.contains(&PROJECT_READ), "{tools:?}");
    }
    assert_eq!(app.ctx.shell.sidebar().projects(), ["work", "alpha"], "listed from the session's project");
    let titles: Vec<String> = app.areas.projects.palette_commands(&app.ctx).into_iter().map(|c| c.title).collect();
    assert!(titles.contains(&"Open project: alpha".to_owned()), "{titles:?}");
    assert!(titles.contains(&"Pin project: alpha".to_owned()), "{titles:?}");
    assert!(app.ctx.app_tools.iter().any(|tool| tool.name == PROJECT_SEARCH), "the Tools panel lists project_search");

    // The palette reopens the screen; the overview lists the session.
    for message in [palette::Message::Open, palette::Message::QueryChanged("Open project: alpha".into()), palette::Message::Submit] {
        update(&mut app, AppMessage::Palette(message));
    }
    assert_eq!(app.ctx.screen(), Some(AreaId::Projects));
    let rows = &app.areas.projects.screen().unwrap().overview().expect("loaded").sessions;
    assert_eq!(rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), [session.as_str()]);
    assert_eq!(rows[0].turns, 1);
    assert_eq!(rows[0].title.as_deref(), Some("set up iroh"), "titled like Recents: the first prompt");

    // Shared instructions and a pinned file, saved and delivered as context.
    let instructions = Edit::Paste(Arc::new("Always use port 31415.".to_owned()));
    on_screen(&mut app, screen::Message::InstructionsEdited(Action::Edit(instructions)));
    on_screen(&mut app, screen::Message::PinPathChanged("src/net.rs".into()));
    on_screen(&mut app, screen::Message::AddPin);
    on_screen(&mut app, screen::Message::Save);
    let screen = app.areas.projects.screen().unwrap();
    assert!(!screen.is_dirty(), "saved");
    assert_eq!(screen.overview().unwrap().settings.instructions, "Always use port 31415.");
    let stored = std::fs::read_dir(dir.path().join("work/loa/projects")).unwrap().count();
    assert_eq!(stored, 1, "settings written under <data>/projects");
    let context = app.areas.projects.context("alpha").expect("context from the settings");
    assert!(context.contains("Always use port 31415.") && context.contains("src/net.rs"), "{context}");

    // The exclude toggle reaches the session and the screen.
    on_screen(&mut app, screen::Message::SetIncluded { session: session.clone(), included: false });
    let row = &app.areas.projects.screen().unwrap().overview().unwrap().sessions[0];
    assert!(row.excluded);
    let dom = app.ctx.sessions.get(&session).unwrap().writer().with_dom(Dom::clone);
    assert!(loa_projects::is_excluded(&dom), "recorded in the session");
    on_screen(&mut app, screen::Message::SetIncluded { session: session.clone(), included: true });
    let dom = app.ctx.sessions.get(&session).unwrap().writer().with_dom(Dom::clone);
    assert!(!loa_projects::is_excluded(&dom));

    // Opening a session from the screen shows its chat.
    on_screen(&mut app, screen::Message::OpenSession(session.clone()));
    assert_eq!(active(&app), Some(session.as_str()));
    assert_eq!(app.ctx.screen(), None);
}

#[test]
fn a_turn_end_digest_reaches_the_next_chat_with_project_search() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, configs) = project_app(dir.path());
    let mut outcomes = app.areas.projects.outcomes.take().expect("the subscription hasn't taken them");
    let first = chat_in(&mut app, "alpha", "set up iroh");

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let outcome = runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), outcomes.recv()).await })
        .expect("rebuilt in time")
        .expect("driver runs");
    assert!(matches!(&outcome, RebuildOutcome::Rebuilt { session, .. } if *session == first), "{outcome:?}");
    projects(&mut app, Message::Rebuild(outcome));
    let context = app.areas.projects.context("alpha").expect("the digest is context now");
    assert!(context.contains("Set up the iroh endpoint on port 31415."), "{context}");

    let second = chat_in(&mut app, "alpha", "what port?");
    assert_ne!(second, first);
    let config = configs.lock().unwrap().last().cloned().unwrap();
    assert_eq!(config.session_id, second);
    assert!(config.context.as_deref().is_some_and(|context| context.contains("port 31415")), "{:?}", config.context);
    let handler = config.host_tool_handler.expect("host tools installed");
    let hits = runtime
        .block_on(handler.call(&second, PROJECT_SEARCH, r#"{"query":"iroh"}"#))
        .expect("project_search answers");
    assert!(hits.contains(&first), "hits come from the first chat: {hits}");
}

/// The live test's chat model: `LOA_LIVE_MODEL`, else Claude Haiku.
fn live_model() -> String {
    std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".into())
}

/// Runs `task` on `runtime` as the iced runtime would, feeding every message
/// back through `App::update`. The runtime outlives the call, so harness
/// processes started by a task keep running.
fn drive(app: &mut App, runtime: &tokio::runtime::Runtime, task: Task<AppMessage>) {
    use iced::futures::StreamExt;
    let mut pending = vec![task];
    while let Some(task) = pending.pop() {
        let Some(stream) = iced_runtime::task::into_stream(task) else { continue };
        let outputs = runtime.block_on(async {
            let outputs = stream.filter_map(|action| async move {
                match action {
                    iced_runtime::Action::Output(message) => Some(message),
                    _ => None,
                }
            });
            tokio::time::timeout(LIVE_TIMEOUT, outputs.collect::<Vec<_>>()).await.expect("task finished")
        });
        for message in outputs {
            pending.push(app.update(message));
        }
    }
}

const LIVE_TIMEOUT: Duration = Duration::from_secs(240);

fn live(app: &mut App, runtime: &tokio::runtime::Runtime, message: AppMessage) {
    let task = app.update(message);
    drive(app, runtime, task);
}

/// Picks the live model for the next session in the composer.
fn pick_model(app: &mut App, runtime: &tokio::runtime::Runtime) {
    let model = live_model();
    let option = loa_ui_composer::ModelOption { id: model.clone(), name: model, provider: String::new(), role: None };
    live(app, runtime, AppMessage::Models(crate::areas::models::Message::Model(option)));
}

/// Types `text` into the composer, submits it, and delivers the session's
/// feed to the app until the turn ended. Approval cards the turn raises
/// are allowed through the card's first option.
fn live_turn(app: &mut App, runtime: &tokio::runtime::Runtime, text: &str) -> Dom {
    let session = app.ctx.sessions.active().expect("a session runs");
    let id = session.id().clone();
    let writer = Arc::clone(session.writer());
    let (_, mut feed) = writer.subscribe(Policy::every_patch(4096));
    let turns = writer.with_dom(|dom| crate::session::dom_turns(dom).count());
    for c in text.chars() {
        live(app, runtime, AppMessage::Composer(loa_ui_composer::Message::Edit(Action::Edit(Edit::Insert(c)))));
    }
    live(app, runtime, AppMessage::Composer(loa_ui_composer::Message::Submit));
    let deadline = tokio::time::Instant::now() + LIVE_TIMEOUT;
    loop {
        let ended = writer.with_dom(|dom| {
            let all: Vec<_> = crate::session::dom_turns(dom).collect();
            all.len() > turns
                && all.last().and_then(|turn| turn.str_attr(attr::STATUS)).and_then(TurnStatus::parse).is_some_and(|status| {
                    matches!(status, TurnStatus::Done | TurnStatus::Error | TurnStatus::Cancelled)
                })
        });
        while let Ok(event) = feed.try_recv() {
            live(app, runtime, AppMessage::Session(id.clone(), crate::session::Feed::Event(event)));
        }
        if let Some(decision) = app.areas.decisions.focused() {
            eprintln!("allowing decision {decision:?}");
            let choose = loa_ui_decisions::desk::Message::Card(id.to_string(), loa_ui_decisions::Message::Choose(decision, 0));
            live(app, runtime, AppMessage::Decisions(crate::areas::decisions::Message::Desk(choose)));
        }
        if ended {
            return writer.with_dom(Dom::clone);
        }
        assert!(tokio::time::Instant::now() < deadline, "turn finished in time");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn assistant_text(dom: &Dom) -> String {
    dom.iter()
        .filter(|n| n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == Some("assistant") && n.str_attr(attr::KIND) != Some("thinking"))
        .map(|n| n.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Files named `name` under `dir`, recursively.
fn find_files(dir: &Path, name: &str) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(find_files(&path, name));
        } else if path.file_name().is_some_and(|file| file == name) {
            found.push(path);
        }
    }
    found
}

#[test]
#[ignore = "runs the installed omp with real model calls (LOA_LIVE_MODEL, default anthropic/claude-haiku-5-5)"]
fn live_two_omp_chats_share_a_smol_digest_and_project_search() {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("liveproj");
    std::fs::create_dir_all(&cwd).unwrap();
    let node = crate::services::Node::new(loa_identity::NodeIdentity::from_secret_bytes(&[9; 32]), "live".into());
    let dirs = crate::services::Dirs::new(dir.path().join("loa"));
    let harnesses = crate::services::Harnesses::detect(&node.key, &dirs.omp);
    let services = crate::services::Services::new(node, dirs.clone(), harnesses, cwd.clone()).unwrap();
    let (mut app, _) = runtime.block_on(async { App::build(services) });
    assert_eq!(app.ctx.sessions.harness.as_deref(), Some("omp"), "omp is installed");
    // The real omp smol summarizer, without the 10 s quiet time.
    let config = ServiceConfig { debounce: Duration::from_millis(200), ..ServiceConfig::default() };
    let summarizer = OmpSummarizer::new("omp", dirs.data.clone());
    let (projects, task) = runtime.block_on(async { State::with(&mut app.ctx, Arc::new(summarizer), config) });
    app.areas.projects = projects;
    drive(&mut app, &runtime, task.map(AppMessage::Projects));
    let mut outcomes = app.areas.projects.outcomes.take().unwrap();

    // Shared instructions from the project screen.
    let open = shell::Message::Sidebar(shell::sidebar::Message::OpenProject("liveproj".into()));
    live(&mut app, &runtime, AppMessage::Shell(open));
    let instructions = Edit::Paste(Arc::new("Answer briefly.".to_owned()));
    for message in [screen::Message::InstructionsEdited(Action::Edit(instructions)), screen::Message::Save] {
        live(&mut app, &runtime, AppMessage::Projects(Message::Screen(message)));
    }

    // Chat 1: its turn end rebuilds the digest with omp's smol role.
    pick_model(&mut app, &runtime);
    live(&mut app, &runtime, AppMessage::Projects(Message::Screen(screen::Message::NewChat)));
    let first = active(&app).expect("chat 1 runs").to_owned();
    live_turn(
        &mut app,
        &runtime,
        "We decided: the control port is 31415 and the code lives in src/net.rs. Reply only with: noted.",
    );
    let outcome = runtime
        .block_on(async { tokio::time::timeout(LIVE_TIMEOUT, outcomes.recv()).await })
        .expect("rebuild in time")
        .expect("driver runs");
    assert!(matches!(&outcome, RebuildOutcome::Rebuilt { session, .. } if *session == first), "{outcome:?}");
    live(&mut app, &runtime, AppMessage::Projects(Message::Rebuild(outcome)));
    let dom = app.ctx.sessions.get(&first).unwrap().writer().with_dom(Dom::clone);
    let digest = loa_projects::digest_of(&dom).expect("a digest was written");
    eprintln!("digest: {:?} (model {:?})", digest.content, digest.model);
    assert_eq!(digest.model.as_deref(), Some("smol"));
    let context = app.areas.projects.context("liveproj").expect("project context").to_owned();
    assert!(context.contains("Answer briefly.") && context.contains(&digest.content.summary), "{context}");
    let rows = &app.areas.projects.screen().unwrap().overview().unwrap().sessions;
    assert_eq!(rows[0].digest_state, loa_projects::DigestState::Fresh, "the screen shows the fresh digest");

    // Chat 2 from the project screen: the digest is its context; it searches the project.
    pick_model(&mut app, &runtime);
    live(&mut app, &runtime, AppMessage::Shell(shell::Message::Sidebar(shell::sidebar::Message::OpenProject("liveproj".into()))));
    live(&mut app, &runtime, AppMessage::Projects(Message::Screen(screen::Message::NewChat)));
    let second = active(&app).expect("chat 2 runs").to_owned();
    assert_ne!(second, first);
    let dom = live_turn(
        &mut app,
        &runtime,
        "First, from the project context you were given, which control port did the other session pick? \
         Then call the project_search tool with query \"31415\" and tell me the session id of the first hit.",
    );
    let reply = assistant_text(&dom);
    eprintln!("reply: {reply}");
    assert!(reply.contains("31415"), "assistant said {reply:?}");
    let call = dom
        .iter()
        .find(|n| n.kind == ElementKind::Call && n.str_attr(attr::TOOL) == Some(PROJECT_SEARCH))
        .expect("the model called project_search");
    let result = dom.children_of_kind(call.id, ElementKind::Result).next().expect("project_search has a result");
    eprintln!("project_search result: {}", result.text);
    assert!(result.text.contains(&first), "{}", result.text);

    // omp's own memory is off for app sessions.
    let overlays = find_files(&dirs.omp, "memory-off.yml");
    assert!(!overlays.is_empty(), "the memory-off overlay was written");
    assert!(std::fs::read_to_string(&overlays[0]).unwrap().contains("backend: \"off\""));

    for id in [first, second] {
        if let Some(harness) = app.ctx.close_session(&id) {
            runtime.block_on(harness.shutdown()).ok();
        }
    }
    runtime.shutdown_background();
}
