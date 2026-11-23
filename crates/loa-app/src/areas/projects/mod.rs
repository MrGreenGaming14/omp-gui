//! Projects (P1, P2): projects are groups of sessions, not folders. A session's project is its
//! `<session project>`; the sidebar lists every stored project and every project a session names.
//! A project may have a folder, where its new chats start and whose `.omgui` folder links to the
//! directories its sessions run in. This area creates projects (the New project screen), moves
//! sessions between them, changes a project's folder, deletes projects (keeping their sessions),
//! selects a project (it becomes the active project, with its folder when it has one, and its
//! screen lists its chats), and runs the project screen (shared instructions, the ideas notes,
//! pinned files, sessions with their digests and exclude toggle), digest rebuilds at turn end and
//! compaction, the project context new chats receive, and the `project_search` / `project_read`
//! host tools.

use loa_util::LockExt;
use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use iced::{Element, Subscription, Task};
use loa_contracts::harness::{HostToolSpec, HostTools, SessionConfig};
use loa_projects::{Error, FileSettingsStore, LiveSessions, LocalOnly, ProjectService, ProjectSummary, RebuildOutcome, ServiceConfig, Summarizers};
use loa_settings::projects::{Ideas, ProjectOverview, ProjectSettings};
use loa_util::fs::Snapshot;
use loa_ui_kit::widgets::Screen;
use loa_ui_settings::projects::{self as screen, create};
use loa_view::settings::projects::{CreateIntent, Intent};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::area::{perform, Area, AreaCommand, AreaMessage, Ctx, HostToolSet, Listed, Request, Target};
use loa_runtime::session::SessionEvent;
use crate::watch::{PtrKey, watch};
use crate::view::settings::projects::{overview_to_view, settings_from_view, settings_to_view};

mod history;

#[cfg(test)]
mod tests;

/// Palette command id prefixes that open a project and toggle its sidebar
/// pin, followed by its name.
const OPEN_COMMAND: &str = "open:";
const PIN_COMMAND: &str = "pin:";

#[derive(Clone, Debug)]
pub enum Message {
    /// The user picked this project in the sidebar or the palette.
    Open(String),
    /// Every project, most recently active first.
    Listed(Result<Vec<ProjectSummary>, String>),
    /// The project screen's data for this project.
    Overview(u64, String, Result<ProjectOverview, String>),
    Screen(screen::Message),
    /// A save of the project screen with these settings.
    Saved(String, ProjectSettings, Saving),
    /// A session's exclude toggle was recorded, in this project.
    Excluded(String, Result<bool, String>),
    /// The context new chats in this project receive; `None` when it has
    /// nothing to share yet.
    Context(String, Result<Option<String>, String>),
    /// The rebuild driver finished with a session's digest.
    Rebuild(RebuildOutcome),
    /// Make this project the active one without showing its screen: the
    /// composer's project chip with no session shown.
    Activate(String),
    /// A new session in this project, in its folder: the `+` on its row.
    NewChatIn(String),
    /// Show the project's screen asking to confirm its deletion.
    AskDelete(String),
    /// A project was deleted, with how many sessions it had.
    Deleted(String, Result<usize, String>),
    /// Open the New project screen.
    New,
    Form(create::Message),
    /// A project was created, with the folder it stored.
    Created(String, Result<Option<PathBuf>, String>),
    /// A folder dialog closed, with the folder picked if any.
    FolderChosen(Pick, Result<Option<PathBuf>, String>),
    /// A project's folder was stored, possibly isolated inside the temp folder.
    FolderSet(String, Result<PathBuf, String>),
    /// Move a session to a project, or out of every project with `None`.
    Move { session: String, project: Option<String> },
    Moved(Result<(), String>),
    HistoryLoaded(u64, Result<Vec<loa_harness::omp::native::ProjectSession>, String>),
    HistoryOpened(u64, Result<String, String>),
}

/// What a folder dialog picks for.
#[derive(Clone, Debug)]
pub enum Pick {
    /// The New project form's folder.
    Form,
    /// The folder of this existing project.
    Project(String),
}

/// How saving the project screen went.
#[derive(Clone, Debug)]
pub enum Saving {
    /// Everything sent was stored; the ideas notes, when sent, with their file as written.
    Stored(Option<Ideas>),
    /// The settings were stored, but the ideas notes changed outside since they were loaded and were left alone.
    IdeasChanged,
    Failed(String),
}

/// The project and directory the next new session starts in, set by the
/// project screen's "New chat" and a project row's `+`.
#[derive(Clone, Debug)]
struct NewChat {
    project: String,
    cwd: Option<PathBuf>,
}

pub struct State {
    service: ProjectService,
    sessions: LiveSessions,
    history: history::History,
    /// `project_search` and `project_read`, served to every session.
    tools: (Vec<HostToolSpec>, Arc<dyn HostTools>),
    /// The rebuild driver's outcomes, until the subscription takes them.
    outcomes: Arc<Mutex<Option<UnboundedReceiver<RebuildOutcome>>>>,
    projects: Vec<ProjectSummary>,
    /// The project screen, drawn while the app shows this area's screen.
    screen: Option<screen::State>,
    overview_request: Cell<u64>,
    /// The New project form, drawn instead of the project screen while open.
    creating: Option<create::State>,
    /// Context for new chats, by project.
    contexts: HashMap<String, Option<String>>,
    /// The shown project's ideas notes file as seen by the screen's notes, which saving them checks it against.
    /// Kept while the notes have an unsaved edit; `None` until loaded, and to take the next overview's notes over
    /// an edit.
    ideas_file: Option<Snapshot>,
    /// Taken by the next new session's `contribute`.
    new_chat: Mutex<Option<NewChat>>,
    /// Owns the runtime the rebuild driver runs on when the app had none.
    _runtime: OwnRuntime,
}

impl State {
    /// The project whose page this area shows, if any.
    pub(crate) fn shown(&self) -> Option<&str> {
        if self.creating.is_some() {
            return None;
        }
        self.screen.as_ref().map(|screen| screen.project())
    }

    /// The projects of this node with digests from `summarizer`.
    pub fn with(ctx: &mut Ctx, summarizer: Arc<dyn Summarizers>, config: ServiceConfig) -> (Self, Task<Message>) {
        let library = ctx.runtime.library();
        let sessions = LiveSessions::new(Arc::clone(library.bus()), library.store().clone());
        let settings = Arc::new(FileSettingsStore::new(ctx.services.storage.dirs.data.join("projects")));
        let (runtime, handle) = OwnRuntime::current_or_new();
        let _entered = handle.enter();
        let (service, outcomes) = ProjectService::start(sessions.clone(), settings, Arc::new(LocalOnly), summarizer, config);
        let tools = service.host_tools();
        for spec in &tools.0 {
            ctx.set_app_tool(loa_view::panel::ToolInfo::from(spec));
        }
        let state = Self {
            service,
            sessions,
            history: history::History::default(),
            tools,
            outcomes: Arc::new(Mutex::new(Some(outcomes))),
            projects: Vec::new(),
            screen: None,
            overview_request: Cell::new(0),
            creating: None,
            contexts: HashMap::new(),
            ideas_file: None,
            new_chat: Mutex::new(None),
            _runtime: runtime,
        };
        let context = ctx.runtime.project().map_or_else(Task::none, |project| state.refresh_context(project.to_owned()));
        let task = Task::batch([state.list(), context]);
        (state, task)
    }

    /// The projects the sidebar lists.
    pub fn projects(&self) -> &[ProjectSummary] {
        &self.projects
    }

    /// The cached context new chats in `project` receive.
    pub fn context(&self, project: &str) -> Option<&str> {
        self.contexts.get(project).and_then(Option::as_deref)
    }

    /// The project screen, once a project was opened.
    pub fn screen(&self) -> Option<&screen::State> {
        self.screen.as_ref()
    }

    /// The New project form while it is open.
    pub fn form(&self) -> Option<&create::State> {
        self.creating.as_ref()
    }

    fn list(&self) -> Task<Message> {
        let service = self.service.clone();
        perform(async move { service.projects().await }, Message::Listed)
    }

    fn load_overview(&self, project: String) -> Task<Message> {
        let request = self.overview_request.get() + 1;
        self.overview_request.set(request);
        let service = self.service.clone();
        Task::perform(
            async move {
                let result = service.overview(&project).await.map_err(|error| error.to_string());
                (project, result)
            },
            move |(project, result)| Message::Overview(request, project, result),
        )
    }

    fn refresh_context(&self, project: String) -> Task<Message> {
        let service = self.service.clone();
        Task::perform(
            async move {
                let result = service.context(&project, None, "").await.map_err(|error| error.to_string());
                (project, result)
            },
            |(project, result)| Message::Context(project, result),
        )
    }

    /// Rebuilds the cached context of every project already cached.
    fn refresh_contexts(&self) -> Task<Message> {
        Task::batch(self.contexts.keys().map(|project| self.refresh_context(project.clone())))
    }

    /// Reloads the open project screen.
    fn reload_screen(&self) -> Task<Message> {
        match &self.screen {
            Some(screen) => self.load_overview(screen.project().to_owned()),
            None => Task::none(),
        }
    }

    /// The folder of `project` when it still exists: the one it was given, else that of its most recent session.
    fn folder(&self, project: &str) -> Option<PathBuf> {
        self.projects.iter().find(|summary| summary.name == project).and_then(|summary| summary.cwd.as_deref()).map(PathBuf::from).filter(|folder| folder.is_dir())
    }

    /// Puts the project names and the current project into the composer's project chip: the shown session's
    /// project, else the active one.
    fn sync_chip(&self, ctx: &mut Ctx) {
        let current = match ctx.runtime.sessions().active() {
            Some(session) => session.summary().and_then(|summary| summary.project.clone()),
            None => ctx.runtime.project().map(str::to_owned),
        };
        ctx.composer.set_projects(self.names(), current);
    }

    /// Makes `project` the active project, with its folder, and shows its screen: the project's chats, most recent
    /// first.
    fn open(&mut self, project: String, ctx: &mut Ctx) -> Task<Message> {
        let folder = self.folder(&project);
        self.open_in(project, folder, ctx)
    }

    /// [`Self::open`] with the folder given, for a project not listed yet.
    fn open_in(&mut self, project: String, folder: Option<PathBuf>, ctx: &mut Ctx) -> Task<Message> {
        ctx.request(Request::ActivateProject { project: project.clone(), cwd: folder });
        self.show(project, ctx)
    }

    /// Shows `project`'s screen without making it the active project.
    fn show(&mut self, project: String, ctx: &mut Ctx) -> Task<Message> {
        self.creating = None;
        if self.screen.as_ref().is_none_or(|screen| screen.project() != project) {
            self.screen = Some(screen::State::new(project.clone()));
            self.ideas_file = None;
        }
        ctx.show_screen(Self::ID);
        self.clear_history();
        Task::batch([self.load_overview(project.clone()), self.refresh_context(project)])
    }

    /// The desktop's folder dialog, starting at `start`, answering for `pick`.
    fn choose_folder(ctx: &Ctx, start: PathBuf, pick: Pick) -> Task<Message> {
        let picker = ctx.services.host.picker.clone();
        Task::perform(async move { picker.pick(&start).await }, move |result| Message::FolderChosen(pick.clone(), result))
    }

    fn screen_event(&mut self, event: Intent, ctx: &mut Ctx) -> Task<Message> {
        match event {
            Intent::SaveSettings { project, settings, ideas } => {
                let service = self.service.clone();
                let settings = settings_from_view(settings);
                let notes = ideas.map(|text| (self.ideas_file.clone(), text));
                Task::perform(
                    async move {
                        let seen = notes.as_ref().and_then(|(seen, text)| Some((seen.clone()?, text.as_str())));
                        let saving = match (service.save_settings(&project, &settings, seen).await, notes) {
                            (Ok(Some(file)), Some((_, text))) => Saving::Stored(Some(Ideas { text, file })),
                            // Notes edited while no file was seen for them, after a reload asked for one.
                            (Ok(None), Some(_)) | (Err(Error::ChangedOnDisk { .. }), _) => Saving::IdeasChanged,
                            (Ok(_), None) => Saving::Stored(None),
                            (Err(error), _) => Saving::Failed(error.to_string()),
                        };
                        (project, settings, saving)
                    },
                    |(project, settings, saving)| Message::Saved(project, settings, saving),
                )
            }
            Intent::ReloadIdeas => {
                self.ideas_file = None;
                self.reload_screen()
            }
            Intent::SetExcluded { session, excluded } => {
                let service = self.service.clone();
                let project = self.screen.as_ref().map(|screen| screen.project().to_owned()).unwrap_or_default();
                Task::perform(
                    async move {
                        let result = service.set_excluded(&session, excluded).await.map_err(|error| error.to_string());
                        (project, result)
                    },
                    |(project, result)| Message::Excluded(project, result),
                )
            }
            Intent::OpenSession(id) => {
                // The active session's chat comes back too.
                ctx.close_screen();
                ctx.request(Request::OpenSession(id));
                Task::none()
            }
            Intent::OpenOmpHistory(id) => self.open_history(&id, ctx),
            Intent::NewChat { project, cwd } => {
                *self.new_chat.locked() = Some(NewChat { project, cwd: cwd.map(PathBuf::from) });
                ctx.request(Request::NewSession);
                Task::none()
            }
            Intent::ChangeFolder { project, current } => {
                let start = current.map_or_else(|| ctx.runtime.cwd().to_path_buf(), PathBuf::from);
                Self::choose_folder(ctx, start, Pick::Project(project))
            }
            Intent::Delete { project } => {
                if !self.history.begin_change(&project) {
                    ctx.notify("Wait for the current project operation to finish");
                    return Task::none();
                }
                let service = self.service.clone();
                Task::perform(
                    async move {
                        let result = service.delete(&project).await.map_err(|error| error.to_string());
                        (project, result)
                    },
                    |(project, result)| Message::Deleted(project, result),
                )
            }
        }
    }

    fn form_event(&mut self, event: CreateIntent, ctx: &mut Ctx) -> Task<Message> {
        match event {
            CreateIntent::ChooseFolder(start) => Self::choose_folder(ctx, PathBuf::from(start), Pick::Form),
            CreateIntent::Create { name, settings } => {
                let settings = settings_from_view(settings);
                if let Some(folder) = settings.folder.as_ref().filter(|folder| !folder.is_dir()) {
                    let error = format!("{} isn't a folder", folder.display());
                    if let Some(form) = self.creating.as_mut() {
                        form.failed(error);
                    }
                    return Task::none();
                }
                let service = self.service.clone();
                Task::perform(
                    async move {
                        let result = service.create(&name, &settings).await.map_err(|error| error.to_string());
                        (name, result)
                    },
                    |(name, result)| Message::Created(name, result),
                )
            }
            CreateIntent::Cancel => {
                self.creating = None;
                // Back to the project screen it was opened over, if any.
                if self.screen.is_none() {
                    ctx.close_screen();
                }
                Task::none()
            }
        }
    }

    /// The project names, most recently active first.
    fn names(&self) -> Vec<String> {
        self.projects.iter().map(|project| project.name.clone()).collect()
    }
}

impl AreaMessage for Message {
    type Area = State;
}

impl Area for State {
    const ID: &'static str = "projects";
    const COMMANDS: &'static [AreaCommand<Self>] = &[AreaCommand::<Self>::new("projects.", "", "Projects", |state, arg, ctx| {
        if let Some(project) = arg.strip_prefix(PIN_COMMAND) {
            let toggle = loa_ui_shell::sidebar::Message::ToggleProjectPin(project.to_owned());
            ctx.request(Request::Forward(Box::new(crate::app::Message::Shell(loa_ui_shell::Message::Sidebar(toggle)))));
            return Task::none();
        }
        match arg.strip_prefix(OPEN_COMMAND) {
            Some(project) => state.open(project.to_owned(), ctx),
            None => {
                ctx.notify(format!("Unknown command projects.{arg}"));
                Task::none()
            }
        }
    })
    .listed(Listed::Each(|state, ctx| {
        let pinned = ctx.shell.sidebar().pinned_projects();
        let rows = state.names().into_iter().flat_map(|name| {
            let verb = if pinned.contains(&name) { "Unpin" } else { "Pin" };
            [(format!("{OPEN_COMMAND}{name}"), format!("Open project: {name}")), (format!("{PIN_COMMAND}{name}"), format!("{verb} project: {name}"))]
        });
        rows.collect()
    }))];

    type Message = Message;

    /// The projects of this node, digests built by the app's summarizer.
    fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let summarizer = Arc::clone(&ctx.backends.summarizer);
        Self::with(ctx, summarizer, ServiceConfig::default())
    }

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        // A "New chat" that didn't start a session (no harness) is dropped.
        if !matches!(message, Message::Screen(_)) {
            self.new_chat.locked().take();
        }
        match message {
            Message::Open(project) => self.open(project, ctx),
            Message::Listed(Ok(projects)) => {
                self.projects = projects;
                ctx.shell.sidebar_mut().set_projects(self.names());
                self.sync_chip(ctx);
                let uncached: Vec<String> =
                    self.projects.iter().filter(|p| !self.contexts.contains_key(&p.name)).map(|p| p.name.clone()).collect();
                Task::batch(uncached.into_iter().map(|project| self.refresh_context(project)))
            }
            Message::Listed(Err(error)) => {
                ctx.notify(format!("Can't list projects: {error}"));
                Task::none()
            }
            Message::Overview(request, project, result) => {
                if request != self.overview_request.get() { return Task::none(); }
                let Some(screen) = self.screen.as_mut().filter(|screen| screen.project() == project) else {
                    return Task::none();
                };
                match result {
                    Ok(overview) => {
                        let folder = overview.settings.folder.clone().filter(|folder| folder.is_dir());
                        let (reload_ideas, edited) = (self.ideas_file.is_none(), screen.ideas_edited());
                        screen.set_overview(overview_to_view(&overview), loa_core::now_ms());
                        if reload_ideas {
                            screen.revert_ideas();
                        }
                        if reload_ideas || !edited {
                            self.ideas_file = overview.ideas.ok().map(|ideas| ideas.file);
                        }
                        if !self.history.matches_folder(folder.as_deref()) {
                            return self.scan_history(folder, ctx);
                        }
                    }
                    Err(error) => screen.set_error(format!("Can't load project {project}: {error}")),
                }
                Task::none()
            }
            Message::Screen(message) => {
                let Some(event) = self.screen.as_mut().and_then(|screen| screen.update(message)) else {
                    return Task::none();
                };
                self.screen_event(event, ctx)
            }
            Message::Saved(project, settings, saving) => {
                if let Some(screen) = self.screen.as_mut().filter(|screen| screen.project() == project) {
                    let settings = settings_to_view(&settings);
                    match saving {
                        Saving::Stored(ideas) => {
                            screen.saved(Ok((settings, ideas.as_ref().map(|ideas| ideas.text.clone()))));
                            if let Some(ideas) = ideas {
                                self.ideas_file = Some(ideas.file);
                            }
                        }
                        Saving::IdeasChanged => screen.ideas_changed_outside(settings),
                        Saving::Failed(error) => screen.saved(Err(error)),
                    }
                }
                // Also after a failure: the settings may be stored while the notes weren't.
                self.refresh_context(project)
            }
            Message::Excluded(project, result) => {
                if let Err(error) = result
                    && let Some(screen) = self.screen.as_mut()
                {
                    screen.set_error(format!("Can't change the session's context toggle: {error}"));
                }
                Task::batch([self.reload_screen(), self.refresh_context(project)])
            }
            Message::Context(project, Ok(context)) => {
                self.contexts.insert(project, context);
                Task::none()
            }
            Message::Context(project, Err(error)) => {
                ctx.notify(format!("Can't build the context of project {project}: {error}"));
                Task::none()
            }
            Message::Rebuild(outcome) => match outcome {
                RebuildOutcome::Rebuilt { .. } => Task::batch([self.refresh_contexts(), self.reload_screen()]),
                RebuildOutcome::Failed { session, error } => {
                    ctx.notify(format!("Digest of session {session} failed: {error}"));
                    self.reload_screen()
                }
                RebuildOutcome::Skipped { .. } => Task::none(),
            },
            Message::Activate(project) => {
                ctx.request(Request::ActivateProject { cwd: self.folder(&project), project });
                Task::none()
            }
            Message::NewChatIn(project) => {
                *self.new_chat.locked() = Some(NewChat { cwd: self.folder(&project), project });
                ctx.request(Request::NewSession);
                Task::none()
            }
            Message::AskDelete(project) => {
                let task = self.show(project, ctx);
                if let Some(screen) = self.screen.as_mut() {
                    screen.ask_delete();
                }
                task
            }
            Message::Deleted(project, Ok(_)) => {
                self.history.end_change(&project);
                self.contexts.remove(&project);
                if self.screen.as_ref().is_some_and(|screen| screen.project() == project) {
                    self.screen = None;
                    ctx.close_screen();
                }
                ctx.request(Request::ForgetProject(project));
                self.list()
            }
            Message::Deleted(project, Err(error)) => {
                self.history.end_change(&project);
                ctx.notify(format!("Can't delete project {project}: {error}"));
                Task::none()
            }
            Message::New => {
                self.creating = Some(create::State::new(ctx.runtime.cwd().display().to_string()));
                ctx.show_screen(Self::ID);
                iced::widget::operation::focus(create::NAME_ID)
            }
            Message::Form(message) => {
                let Some(event) = self.creating.as_mut().and_then(|form| form.update(message)) else {
                    return Task::none();
                };
                self.form_event(event, ctx)
            }
            Message::Created(name, Ok(folder)) => {
                let open = self.open_in(name, folder, ctx);
                Task::batch([self.list(), open])
            }
            Message::Created(name, Err(error)) => {
                match self.creating.as_mut() {
                    Some(form) => form.failed(error),
                    None => ctx.notify(format!("Can't create project {name}: {error}")),
                }
                Task::none()
            }
            Message::FolderChosen(_, Ok(None)) => Task::none(),
            Message::FolderChosen(Pick::Form, Ok(Some(folder))) => {
                if let Some(form) = self.creating.as_mut() {
                    form.set_folder(folder.display().to_string());
                }
                Task::none()
            }
            Message::FolderChosen(Pick::Project(project), Ok(Some(folder))) => {
                if !self.history.begin_change(&project) {
                    ctx.notify("Wait for the current project operation to finish, then choose the folder again");
                    return Task::none();
                }
                let service = self.service.clone();
                Task::perform(
                    async move {
                        let result = service.set_folder(&project, folder).await.map_err(|error| error.to_string());
                        (project, result)
                    },
                    |(project, result)| Message::FolderSet(project, result),
                )
            }
            Message::FolderChosen(_, Err(error)) => {
                ctx.notify(error);
                Task::none()
            }
            Message::FolderSet(project, Ok(folder)) => {
                self.history.end_change(&project);
                // New chats in the active project start in its new folder.
                if ctx.runtime.project() == Some(project.as_str()) {
                    ctx.request(Request::ActivateProject { project: project.clone(), cwd: Some(folder) });
                }
                if self.shown() == Some(project.as_str()) { self.clear_history(); }
                Task::batch([self.list(), self.reload_screen()])
            }
            Message::FolderSet(project, Err(error)) => {
                self.history.end_change(&project);
                ctx.notify(format!("Can't change the folder of {project}: {error}"));
                Task::none()
            }
            Message::Move { session, project } => {
                let service = self.service.clone();
                perform(async move { service.move_session(&session, project.as_deref()).await }, Message::Moved)
            }
            Message::Moved(Ok(())) => self.list(),
            Message::Moved(Err(error)) => {
                ctx.notify(format!("Can't move the session: {error}"));
                Task::none()
            }
            Message::HistoryLoaded(scan, result) => {
                self.history_loaded(scan, result);
                Task::none()
            }
            Message::HistoryOpened(scan, result) => self.history_opened(scan, result, ctx),
        }
    }

    fn view_screen<'a>(&'a self, _: &'a Ctx) -> Option<Element<'a, Message>> {
        if let Some(form) = &self.creating {
            return Some(form.view().map(Message::Form));
        }
        self.screen.as_ref().map(|screen| screen.view().map(Message::Screen))
    }

    fn contribute(&self, config: &mut SessionConfig, target: &Target<'_>, _: &Ctx) {
        if target.resume {
            return;
        }
        if let Some(chat) = self.new_chat.locked().take() {
            config.project = Some(chat.project);
            if let Some(cwd) = chat.cwd {
                config.cwd = cwd;
            }
        }
        let Some(context) = config.project.as_deref().and_then(|project| self.context(project)) else { return };
        config.context = Some(match config.context.take() {
            Some(other) => format!("{other}\n\n{context}"),
            None => context.to_owned(),
        });
    }

    fn host_tools(&self, _: &SessionConfig, target: &Target<'_>, _: &Ctx) -> Option<HostToolSet> {
        target
            .capabilities
            .host_tools
            .then(|| HostToolSet { specs: self.tools.0.clone(), handler: Arc::clone(&self.tools.1) })
    }

    fn on_session_event(&mut self, event: &SessionEvent, ctx: &mut Ctx) -> Task<Message> {
        match event {
            SessionEvent::TurnEnded { session, .. } => {
                self.service.notify(session.as_str());
                Task::batch([self.list(), self.reload_screen()])
            }
            SessionEvent::Compaction { session, .. } => {
                self.service.notify(session.as_str());
                Task::none()
            }
            SessionEvent::Activated(_) | SessionEvent::Closed(_) => {
                self.sync_chip(ctx);
                self.list()
            }
            // A session moved between projects.
            SessionEvent::Patch(_, patch) if matches!(&patch.op, loa_core::Op::Set { name, .. } if name == loa_core::attr::PROJECT) => {
                self.sync_chip(ctx);
                self.list()
            }
            _ => Task::none(),
        }
    }

    /// The rebuild driver's outcomes; the receiver is taken once.
    fn subscription(&self) -> Subscription<Message> {
        let outcomes = Arc::clone(&self.outcomes);
        watch(PtrKey(&self.outcomes), move || outcomes.locked().take(), |outcome| Some(Message::Rebuild(outcome)))
    }
}

/// A runtime of the area's own, made only when the app has none (tests,
/// tools that build the app headless).
struct OwnRuntime(Option<tokio::runtime::Runtime>);

impl OwnRuntime {
    fn current_or_new() -> (Self, tokio::runtime::Handle) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            return (Self(None), handle);
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("loa-projects")
            .enable_all()
            .build()
            .unwrap_or_else(|error| panic!("can't start the project digest runtime: {error}"));
        let handle = runtime.handle().clone();
        (Self(Some(runtime)), handle)
    }
}

impl Drop for OwnRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}
