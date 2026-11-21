//! Projects (P1, P2): the sidebar's project list from the sessions' projects
//! and working directories, selecting a project (it becomes the active
//! project with its folder, and its screen lists its chats), the project screen (shared instructions, pinned
//! files, sessions with their digests and exclude toggle), digest rebuilds
//! at turn end and compaction, the project context new chats receive, and
//! the `project_search` / `project_read` host tools.

use loa_util::LockExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use iced::{Element, Subscription, Task};
use loa_contracts::harness::{HostToolSpec, HostTools, SessionConfig};
use loa_projects::{
    FileSettingsStore, LiveSessions, LocalOnly, ProjectOverview, ProjectService, ProjectSettings,
    ProjectSummary, RebuildOutcome, ServiceConfig, Summarizers,
};
use loa_ui_kit::widgets::Screen;
use loa_ui_settings::projects as screen;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::area::{perform, Area, AreaCommand, AreaMessage, Ctx, HostToolSet, Listed, Request, Target};
use loa_runtime::session::SessionEvent;
use crate::watch::{PtrKey, watch};

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
    /// Every project with sessions, most recently active first.
    Listed(Result<Vec<ProjectSummary>, String>),
    /// The project screen's data for this project.
    Overview(String, Result<ProjectOverview, String>),
    Screen(screen::Message),
    /// The project's settings were stored.
    Saved(String, Result<ProjectSettings, String>),
    /// A session's exclude toggle was recorded, in this project.
    Excluded(String, Result<bool, String>),
    /// The context new chats in this project receive; `None` when it has
    /// nothing to share yet.
    Context(String, Result<Option<String>, String>),
    /// The rebuild driver finished with a session's digest.
    Rebuild(RebuildOutcome),
}

/// The project and directory the next new session starts in, set by the
/// project screen's "New chat".
#[derive(Clone, Debug)]
struct NewChat {
    project: String,
    cwd: Option<PathBuf>,
}

pub struct State {
    service: ProjectService,
    /// `project_search` and `project_read`, served to every session.
    tools: (Vec<HostToolSpec>, Arc<dyn HostTools>),
    /// The rebuild driver's outcomes, until the subscription takes them.
    outcomes: Arc<Mutex<Option<UnboundedReceiver<RebuildOutcome>>>>,
    projects: Vec<ProjectSummary>,
    /// The project screen, drawn while the app shows this area's screen.
    screen: Option<screen::State>,
    /// Context for new chats, by project.
    contexts: HashMap<String, Option<String>>,
    /// Taken by the next new session's `contribute`.
    new_chat: Mutex<Option<NewChat>>,
    /// Owns the runtime the rebuild driver runs on when the app had none.
    _runtime: OwnRuntime,
}

impl State {
    /// The project whose page this area shows, if any.
    pub(crate) fn shown(&self) -> Option<&str> {
        self.screen.as_ref().map(|screen| screen.project())
    }

    /// The projects of this node with digests from `summarizer`.
    pub fn with(ctx: &mut Ctx, summarizer: Arc<dyn Summarizers>, config: ServiceConfig) -> (Self, Task<Message>) {
        let library = ctx.runtime.library();
        let sessions = LiveSessions::new(Arc::clone(library.bus()), library.store().clone());
        let settings = Arc::new(FileSettingsStore::new(ctx.services.storage.dirs.data.join("projects")));
        let (runtime, handle) = OwnRuntime::current_or_new();
        let _entered = handle.enter();
        let (service, outcomes) = ProjectService::start(sessions, settings, Arc::new(LocalOnly), summarizer, config);
        let tools = service.host_tools();
        for spec in &tools.0 {
            ctx.set_app_tool(loa_ui_panel::tools::ToolInfo::from(spec));
        }
        ctx.shell.sidebar_mut().set_projects([ctx.services.workspace_project()]);
        let state = Self {
            service,
            tools,
            outcomes: Arc::new(Mutex::new(Some(outcomes))),
            projects: Vec::new(),
            screen: None,
            contexts: HashMap::new(),
            new_chat: Mutex::new(None),
            _runtime: runtime,
        };
        let task = Task::batch([state.list(), state.refresh_context(ctx.runtime.project().to_owned())]);
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

    fn list(&self) -> Task<Message> {
        let service = self.service.clone();
        perform(async move { service.projects().await }, Message::Listed)
    }

    fn load_overview(&self, project: String) -> Task<Message> {
        let service = self.service.clone();
        Task::perform(
            async move {
                let result = service.overview(&project).await.map_err(|error| error.to_string());
                (project, result)
            },
            |(project, result)| Message::Overview(project, result),
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

    /// The folder of `project`: the launch folder for the workspace project,
    /// else the folder of its most recent session, when it still exists.
    fn folder(&self, project: &str, ctx: &Ctx) -> Option<PathBuf> {
        let folder = if project == ctx.services.workspace_project() {
            Some(ctx.services.workspace.clone())
        } else {
            self.projects.iter().find(|summary| summary.name == project).and_then(|summary| summary.cwd.as_deref()).map(PathBuf::from)
        };
        folder.filter(|folder| folder.is_dir())
    }

    /// Makes `project` the active project, with its folder, and shows its
    /// screen: the project's chats, most recent first.
    fn open(&mut self, project: String, ctx: &mut Ctx) -> Task<Message> {
        match self.folder(&project, ctx) {
            Some(cwd) => ctx.request(Request::ActivateProject { project: project.clone(), cwd }),
            None => ctx.notify(format!("{project} has no folder yet; start a chat in it first")),
        }
        if self.screen.as_ref().is_none_or(|screen| screen.project() != project) {
            self.screen = Some(screen::State::new(project.clone()));
        }
        ctx.show_screen(Self::ID);
        Task::batch([self.load_overview(project.clone()), self.refresh_context(project)])
    }

    fn screen_event(&mut self, event: screen::Event, ctx: &mut Ctx) -> Task<Message> {
        match event {
            screen::Event::SaveSettings { project, settings } => {
                let service = self.service.clone();
                Task::perform(
                    async move {
                        let result = service.save_settings(&project, &settings).await.map(|()| settings);
                        (project, result.map_err(|error| error.to_string()))
                    },
                    |(project, result)| Message::Saved(project, result),
                )
            }
            screen::Event::SetExcluded { session, excluded } => {
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
            screen::Event::OpenSession(id) => {
                // The active session's chat comes back too.
                ctx.close_screen();
                ctx.request(Request::OpenSession(id));
                Task::none()
            }
            screen::Event::NewChat { project, cwd } => {
                *self.new_chat.locked() = Some(NewChat { project, cwd: cwd.map(PathBuf::from) });
                ctx.request(Request::NewSession);
                Task::none()
            }
        }
    }

    /// The sidebar's project names: the listed projects, and this
    /// workspace's project first when it has no sessions yet.
    fn names(&self, workspace: &str) -> Vec<String> {
        let mut names: Vec<String> = Vec::with_capacity(self.projects.len() + 1);
        if !self.projects.iter().any(|project| project.name == workspace) {
            names.push(workspace.to_owned());
        }
        names.extend(self.projects.iter().map(|project| project.name.clone()));
        names
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
        let rows = state.names(&ctx.services.workspace_project()).into_iter().flat_map(|name| {
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
                ctx.shell.sidebar_mut().set_projects(self.names(&ctx.services.workspace_project()));
                let uncached: Vec<String> =
                    self.projects.iter().filter(|p| !self.contexts.contains_key(&p.name)).map(|p| p.name.clone()).collect();
                Task::batch(uncached.into_iter().map(|project| self.refresh_context(project)))
            }
            Message::Listed(Err(error)) => {
                ctx.notify(format!("Can't list projects: {error}"));
                Task::none()
            }
            Message::Overview(project, result) => {
                let Some(screen) = self.screen.as_mut().filter(|screen| screen.project() == project) else {
                    return Task::none();
                };
                match result {
                    Ok(overview) => screen.set_overview(overview, loa_core::now_ms()),
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
            Message::Saved(project, result) => {
                let saved = result.is_ok();
                if let Some(screen) = self.screen.as_mut().filter(|screen| screen.project() == project) {
                    screen.saved(result);
                }
                if saved { self.refresh_context(project) } else { Task::none() }
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
        }
    }

    fn view_screen<'a>(&'a self, _: &'a Ctx) -> Option<Element<'a, Message>> {
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

    fn on_session_event(&mut self, event: &SessionEvent, _: &mut Ctx) -> Task<Message> {
        match event {
            SessionEvent::TurnEnded { session, .. } => {
                self.service.notify(session.as_str());
                Task::batch([self.list(), self.reload_screen()])
            }
            SessionEvent::Compaction { session, .. } => {
                self.service.notify(session.as_str());
                Task::none()
            }
            SessionEvent::Activated(Some(_)) | SessionEvent::Closed(_) => self.list(),
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
