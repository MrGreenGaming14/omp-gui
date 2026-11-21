//! The application: the shell frame, the chat, composer and decision cards of the active
//! session, the palette, the built-in panel tabs and the areas.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iced::keyboard::{self, key};
use iced::widget::{button, column, container, mouse_area, opaque, row, rule, scrollable, space, stack, text};
use iced::{Alignment, Element, Fill, Font, Padding, Subscription, Task, Theme};
use loa_contracts::harness::Prompt;
use loa_core::NodeId;
use loa_ui_kit::style::{ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, pad_x, sp, width};
use loa_ui_kit::widgets::label;
use loa_ui_kit::{Icon, icons::tinted};
use loa_ui_panel::{diff, files};
use loa_ui_session::composer;
use loa_ui_session::transcript as chat;
use loa_ui_shell::sidebar::Event as Nav;
use loa_ui_shell::theme::style;
use loa_ui_shell::{self as shell, ThemeRef, palette};
use loa_bus::SessionId;
use loa_harness::account_support::opener::SystemUrlOpener;
use loa_harness::omp::OneShot;
use loa_harness::omp::account::OmpLaunch;
use loa_models::login::UrlOpener;
use loa_models::service::Backend;
use loa_projects::Summarizers;
use loa_runtime::harnesses::Harnesses;
use loa_runtime::lifecycle::Command;
use loa_runtime::{Runtime, RuntimeParts};
use loa_session::library::Library;
use loa_ui_terminal as terminal;
use loa_util::time::{ago, now_ms};

use crate::adapters::summarizer::OmpSummarizers;
use crate::area::{Area, AreaMsg, Backends, Ctx, Request};
use crate::areas::{self, Registry};
use crate::services::{Services, harness, open_library};

mod keys;
pub(crate) mod lifecycle;
mod panes;
pub(crate) mod usage;
mod windows;
pub(crate) mod workspace;

#[cfg(test)]
pub(crate) mod tests;

use lifecycle::{After, SessionController};
use windows::{ConfigsTab, Overlay, SettingsTab, WindowMessage, Windows};
use workspace::Workspace;

/// How often the sidebar's usage meter re-reads usage.
const USAGE_REFRESH: Duration = Duration::from_secs(300);
/// How often running sessions redraw, so elapsed times advance.
const TICK: Duration = Duration::from_secs(1);
/// How many recent sessions the start screen lists.
const RECENTS: usize = 4;
/// The start screen's starters: an icon, a label and the prompt it puts in the composer.
const STARTERS: [(Icon, &str, &str); 3] = [
    (Icon::Files, "Explain how this project is laid out", "Explain how this project is laid out: the main parts, how they fit together, and where to start reading."),
    (Icon::GitCompare, "Review my uncommitted changes", "Review my uncommitted changes for bugs and anything I missed."),
    (Icon::TriangleAlert, "Find and fix a failing test", "Run the tests, find one that fails, and fix it."),
];

/// What [`App::build`] starts from besides the services: the runtime's parts
/// and the backends areas build on. [`App::new`] makes them from this
/// machine; tests pass offline ones.
pub struct AppParts {
    pub runtime: RuntimeParts,
    /// The model screens' backend.
    pub models: loa_models::service::Parts,
    /// Writes the projects' digests.
    pub summarizer: Arc<dyn Summarizers>,
    /// Home directory: native sessions, omp's rules and config.
    pub home: PathBuf,
    /// Opens links clicked in the transcript.
    pub opener: Arc<dyn UrlOpener>,
}

/// The runtime over `library` and `harnesses`: on the project selected last
/// run while its folder still exists, else the workspace, and on the saved
/// approval mode.
pub(crate) fn runtime_parts(services: &Services, library: Arc<Library>, harnesses: Harnesses) -> RuntimeParts {
    let saved = services.storage.config.load().unwrap_or_default();
    let (project, cwd) = match saved.active_project.filter(|active| active.cwd.is_dir()) {
        Some(active) => (active.name, active.cwd),
        None => (services.workspace_project(), services.workspace.clone()),
    };
    RuntimeParts { library, harnesses, node: services.node.key.clone(), cwd, project, access: saved.harness.approval_mode }
}

#[derive(Clone)]
pub enum Message {
    Shell(shell::Message),
    Chat(chat::Message),
    Composer(composer::Message),
    Files(files::Message),
    Diff(diff::Message),
    Terminal(terminal::state::Message),
    Palette(palette::Message),
    Keyboard(keyboard::Event),
    /// Redraw while a turn runs.
    Tick,
    Session(lifecycle::Message),
    WorkingDiff(Option<String>),
    /// A message for area `.0` ([`Message::area`]).
    Area(&'static str, AreaMsg),
    Usage(usage::Message),
    Window(WindowMessage),
    /// The transcript in a detached session's window.
    DetachedChat(String, chat::Message),
    /// The folder dialog closed with this folder, or none when cancelled.
    FolderChosen(Result<Option<PathBuf>, String>),
    /// A start screen starter, by index in [`STARTERS`].
    Starter(usize),
    /// A recent session picked on the start screen.
    OpenRecent(String),
}

/// What a shell or composer event asks of the app.
enum Intent {
    Start,
    OpenSession(String),
    Submit(Prompt),
    Interrupt,
    Access(String),
    Files,
    Tab(&'static str),
    Folder(PathBuf),
    Browse,
    Usage,
    Notice(&'static str),
    CloseProjectTab(String),
    /// A theme was picked (saved) or the active one changed.
    Theme(Option<ThemeRef>),
    SavePins,
    SaveProjectPins,
    /// Runs this message.
    Forward(Message),
}

/// The route table from shell events to intents.
fn route(event: shell::Event) -> Option<Intent> {
    let window = |message| Intent::Forward(Message::Window(message));
    Some(match event {
        shell::Event::Sidebar(Nav::NewSession) => Intent::Start,
        shell::Event::Sidebar(Nav::OpenUsage) => Intent::Usage,
        shell::Event::Sidebar(Nav::OpenConfigs) => window(WindowMessage::OpenConfigs(ConfigsTab::Models)),
        shell::Event::Sidebar(Nav::OpenSettings) => window(WindowMessage::OpenSettings(SettingsTab::General)),
        shell::Event::Sidebar(Nav::OpenPeers) => window(WindowMessage::OpenSettings(SettingsTab::Peers)),
        shell::Event::Sidebar(Nav::OpenTools) => Intent::Forward(Message::area(areas::tools::Message::Open)),
        shell::Event::Sidebar(Nav::OpenUpdates) => Intent::Forward(Message::area(areas::updater::Message::Open)),
        shell::Event::Sidebar(Nav::OpenSession(id)) if areas::peers::is_remote(&id) => Intent::Forward(Message::area(areas::peers::Message::OpenRecent(id))),
        shell::Event::Sidebar(Nav::OpenSession(id)) => Intent::OpenSession(id),
        shell::Event::Sidebar(Nav::DeleteSession(id)) if areas::peers::is_remote(&id) => {
            Intent::Notice("A peer's session is deleted on its own node; revoke or let the grant end to hide it")
        }
        shell::Event::Sidebar(Nav::DeleteSession(id)) => Intent::Forward(Message::area(areas::sessions::Message::Delete(id))),
        shell::Event::Sidebar(Nav::OpenProject(project)) => Intent::Forward(Message::area(areas::projects::Message::Open(project))),
        shell::Event::Sidebar(Nav::SetPinned { .. }) => Intent::SavePins,
        shell::Event::Sidebar(Nav::SetProjectPinned { .. }) => Intent::SaveProjectPins,
        shell::Event::DetachTab(tab) => window(WindowMessage::Detach(tab)),
        shell::Event::ShowDetached(tab) => window(WindowMessage::Show(tab)),
        shell::Event::ReattachTab(tab) => window(WindowMessage::ReattachTab(tab)),
        shell::Event::CloseProjectTab(project) => Intent::CloseProjectTab(project),
        shell::Event::SetTheme(theme) => Intent::Theme(Some(theme)),
        shell::Event::ThemeChanged(_) => Intent::Theme(None),
    })
}

/// The route table from composer events to intents.
fn compose(event: composer::Event) -> Option<Intent> {
    let area = |message| Some(Intent::Forward(message));
    match event {
        composer::Event::Submit(prompt) => Some(Intent::Submit(prompt)),
        composer::Event::Stop => Some(Intent::Interrupt),
        composer::Event::HarnessChanged(id) => area(Message::area(areas::harness::Message::Pick(id))),
        composer::Event::ModelChanged(model) => area(Message::area(areas::models::Message::Model(model))),
        composer::Event::ModelAction(id) => area(Message::area(areas::models::Message::Action(id))),
        composer::Event::EffortChanged(effort) => area(Message::area(areas::models::Message::Effort(effort))),
        composer::Event::ModeChanged(mode) => area(Message::area(areas::sessions::Message::Mode(mode))),
        composer::Event::AccessChanged(id) => Some(Intent::Access(id)),
        composer::Event::FilesRequested { .. } => Some(Intent::Files),
        composer::Event::OpenWorkspace => Some(Intent::Tab("files")),
        composer::Event::FolderPicked(path) => Some(Intent::Folder(PathBuf::from(path))),
        composer::Event::BrowseFolders => Some(Intent::Browse),
        composer::Event::QueueChanged(_) => None,
    }
}

pub struct App {
    pub(crate) ctx: Ctx,
    pub(crate) areas: Registry,
    pub(crate) sessions: SessionController,
    pub(crate) workspace: Workspace,
    pub(crate) palette: palette::State,
    pub(crate) files: files::State,
    diff: diff::State,
    pub(crate) terminal: terminal::state::State,
    usage: usage::State,
    windows: Windows,
}

impl App {
    /// Boots the daemon: the app on this machine's services, with its main window open.
    pub fn boot() -> (Self, Task<Message>) {
        let (mut app, task) = Self::new();
        let open = app.open_main();
        let catalog = app.areas.get::<areas::models::State>().read_catalog().map(Message::area);
        (app, Task::batch([open, task, catalog]))
    }

    /// The app on this machine's services.
    pub fn new() -> (Self, Task<Message>) {
        let (services, warnings) = Services::load().unwrap_or_else(|error| panic!("{error}"));
        let library = open_library(&services.storage.dirs, &services.node).unwrap_or_else(|error| panic!("{error}"));
        let harnesses = harness::detect(&services.node.key, &services.storage.dirs.omp);
        let runtime = runtime_parts(&services, library, harnesses);
        let executable = |id: &str| runtime.harnesses.get(id).and_then(|harness| harness.executable.clone());
        let launch = OmpLaunch {
            executable: executable(runtime.harnesses.roles_id()).unwrap_or_else(|| OmpLaunch::default().executable),
            cwd: Some(runtime.cwd.clone()),
            env: Vec::new(),
        };
        let env = loa_harness::HarnessEnv { node: services.node.key.clone(), omp_dir: services.storage.dirs.omp.clone() };
        let models = crate::adapters::models::models_parts(launch, &runtime.harnesses, &loa_harness::factories(&env));
        // Digests come from omp's `smol` role.
        let summarizer = OmpSummarizers(OneShot::new(executable("omp").unwrap_or_else(|| PathBuf::from("omp")), services.storage.dirs.data.clone()));
        let home = loa_util::fs::home().unwrap_or_else(|| PathBuf::from("."));
        let parts = AppParts { runtime, models, summarizer: Arc::new(summarizer), home, opener: Arc::new(SystemUrlOpener) };
        let (mut app, task) = Self::build(services, parts);
        if !warnings.is_empty() {
            app.ctx.notify(warnings.join("; "));
        }
        (app, task)
    }

    /// The app over `services` and `parts`: Recents from the session store,
    /// every area registered.
    pub fn build(services: Services, parts: AppParts) -> (Self, Task<Message>) {
        let saved = services.storage.config.load().unwrap_or_default();
        let AppParts { runtime, models, summarizer, home, opener } = parts;
        let mut runtime = Runtime::new(runtime);
        let (shell, setup) = shell::State::new(saved.appearance.clone());
        let theme = shell.theme();
        let mut composer = composer::State::new();
        runtime.sessions_mut().harness = runtime.harnesses().default_id();
        composer.set_harnesses(harness::options(runtime.harnesses()), runtime.sessions().harness.as_deref());
        let (recents, mut problems) = loa_runtime::session::recents(runtime.library());
        problems.extend(shell.theme_state().errors().iter().map(|e| format!("Theme: {e}")));
        let metrics = terminal::canvas::Metrics::measure(Font::MONOSPACE, loa_ui_kit::theme::text_size(TextRole::Label));
        let terminal = terminal::state::State::new(metrics, terminal::local::LocalShell::login_shell(runtime.cwd().to_path_buf()));
        let files = files::State::new(runtime.cwd().to_path_buf(), Box::new(files::LocalFs));
        let usage = usage::State::new(services.host.usage.clone());
        let mut ctx = Ctx {
            services,
            runtime,
            backends: Backends { models: Backend::new(models), summarizer, home, opener },
            shell,
            composer,
            keymap: palette::Keymap::defaults(palette::Platform::current()),
            app_tools: Vec::new(),
            peers: Vec::new(),
            node: loa_p2p::NodeSlot::default(),
            screen: None,
            notice: (!problems.is_empty()).then(|| problems.join("; ")),
            requests: Vec::new(),
            transitions: Vec::new(),
            slash: Vec::new(),
        };
        ctx.shell.set_project_tabs(saved.project_tabs);
        // The project selected last run, while its folder still exists, is the active one from the start.
        ctx.shell.set_active_project(saved.active_project.filter(|active| active.cwd.is_dir()).map(|active| active.name));
        ctx.shell.sidebar_mut().set_pins(saved.pinned);
        ctx.shell.sidebar_mut().set_pinned_projects(saved.pinned_projects);
        ctx.shell.sidebar_mut().set_sessions(recents);
        let (areas, area_tasks) = Registry::new(&mut ctx);
        ctx.slash = crate::area::slash_rows(keys::APP_COMMANDS).chain(areas.slash_rows()).collect();
        ctx.composer.set_commands(ctx.slash.clone());
        let mut app = Self {
            ctx,
            areas,
            sessions: SessionController::new(&theme),
            workspace: Workspace::default(),
            palette: palette::State::new(palette::Platform::current()),
            files,
            diff: diff::State::new(),
            terminal,
            usage,
            windows: Windows::default(),
        };
        let shown = app.tabs().into_iter().filter(|tab| tab.shown).map(|tab| shell::PanelTab::new(tab.id, tab.title, tab.icon)).collect();
        app.ctx.shell.set_tabs(shown);
        let cwd = app.ctx.runtime.cwd().to_path_buf();
        app.ctx.composer.set_workspace(Some(workspace::chip(app.ctx.services.host.git.as_ref(), &cwd)));
        app.remember(&cwd);
        app.refresh_commands();
        let settle = app.settle();
        let usage = app.usage.refresh().map(Message::Usage);
        let diff = app.working_diff();
        (app, Task::batch([setup.map(Message::Shell), area_tasks, diff, settle, usage]))
    }

    /// Every panel tab: the built-in ones, then each area's.
    fn tabs(&self) -> Vec<crate::area::PanelTab> {
        let tabs = panes::REGISTRY.iter().map(|pane| pane.tab(self));
        tabs.chain(self.areas.iter().flat_map(|area| area.panel_tabs())).collect()
    }

    /// Lists the panel tab `id` when it isn't; `false` when no such tab exists.
    fn list_tab(&mut self, id: &str) -> bool {
        if !self.ctx.shell.tabs().iter().any(|tab| tab.id == id) {
            let Some(tab) = self.tabs().into_iter().find(|tab| tab.id == id) else {
                self.ctx.notify(format!("There is no {id} tab"));
                return false;
            };
            self.ctx.shell.add_tab(shell::PanelTab::new(tab.id, tab.title, tab.icon));
        }
        true
    }

    /// Lists the panel tab `id` and shows it, scrolling the tab bar to it.
    fn open_tab(&mut self, id: &str) -> Task<Message> {
        if !self.list_tab(id) {
            return Task::none();
        }
        self.ctx.shell.show_tab(id);
        self.ctx.shell.reveal_active_tab()
    }

    /// Lists the panel tab `id` without taking an open panel from the tab the user reads.
    fn reveal_tab(&mut self, id: &str) -> Task<Message> {
        if !self.ctx.shell.panel_open() {
            return self.open_tab(id);
        }
        self.list_tab(id);
        Task::none()
    }

    /// The palette's commands: the app's, the keymap's reference rows, then each area's.
    fn refresh_commands(&mut self) {
        let mut commands: Vec<_> = keys::APP_COMMANDS.iter().flat_map(|spec| spec.palette(self, &self.ctx)).collect();
        commands.extend(self.reference_keys());
        commands.extend(self.areas.iter().flat_map(|area| area.commands(&self.ctx)));
        self.hint_commands(&mut commands);
        if self.palette.commands() != commands.as_slice() {
            self.palette.set_commands(commands);
        }
    }

    /// Runs a session lifecycle call with the context and the areas, then what it asked of the app.
    fn lifecycle(&mut self, call: impl FnOnce(&mut SessionController, &mut Ctx, &mut Registry) -> Task<Message>) -> Task<Message> {
        let task = call(&mut self.sessions, &mut self.ctx, &mut self.areas);
        self.after(task)
    }

    /// Does what the last lifecycle call asked of the app, then settles.
    fn after(&mut self, task: Task<Message>) -> Task<Message> {
        let mut tasks = vec![task];
        for after in std::mem::take(&mut self.sessions.after) {
            tasks.push(match after {
                After::Present(cwd) => self.follow(cwd),
                After::Diff => self.working_diff(),
                After::RefreshFiles => {
                    self.files.refresh();
                    Task::none()
                }
                After::TurnEnded => {
                    // A queued `/` command runs like a typed one.
                    let next = self.ctx.composer.turn_ended();
                    self.composer_event(next)
                }
                After::Continue(folder, prompt) => {
                    let moved = folder.map_or_else(Task::none, |folder| self.set_folder(folder));
                    Task::batch([moved, self.lifecycle(|s, ctx, areas| s.run(ctx, areas, Command::Start { prompt: Some(prompt) }))])
                }
            });
        }
        tasks.push(self.settle());
        Task::batch(tasks)
    }

    /// Runs what areas asked for and delivers the session events they caused, until none are left.
    fn settle(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        loop {
            if !self.ctx.transitions.is_empty() {
                for transition in std::mem::take(&mut self.ctx.transitions) {
                    tasks.push(self.lifecycle(|s, ctx, areas| s.apply(ctx, areas, transition)));
                }
                continue;
            }
            if self.ctx.requests.is_empty() {
                break;
            }
            for request in std::mem::take(&mut self.ctx.requests) {
                tasks.push(match request {
                    Request::NewSession => self.lifecycle(|s, ctx, areas| s.run(ctx, areas, Command::Start { prompt: None })),
                    Request::OpenSession(id) => self.lifecycle(|s, ctx, areas| s.run(ctx, areas, Command::Open(SessionId::from(id)))),
                    Request::DetachSession(id) => self.detach_session(&id),
                    Request::Prompt(prompt) => self.lifecycle(|s, ctx, areas| s.send(ctx, areas, prompt)),
                    Request::OpenTab(id) => self.open_tab(&id),
                    Request::RevealTab(id) => self.reveal_tab(&id),
                    Request::Spawn(spawn) => self.lifecycle(|s, ctx, areas| s.run(ctx, areas, Command::Spawn(spawn))),
                    Request::ActivateProject { project, cwd } => self.activate_project(project, cwd),
                    Request::Forward(message) => self.update(*message),
                });
            }
        }
        Task::batch(tasks)
    }

    /// Runs palette command `id`: one of [`keys::APP_COMMANDS`], else the command of the area its prefix names.
    pub(crate) fn command(&mut self, id: &palette::CommandId) -> Task<Message> {
        let id = id.as_str();
        if let Some((spec, arg)) = crate::area::find_command(keys::APP_COMMANDS, id) {
            return (spec.run)(self, arg);
        }
        let area = id.split_once('.').and_then(|(area, _)| self.areas.find(area));
        match area.and_then(|area| area.run(id, &mut self.ctx)) {
            Some(task) => Task::batch([task, self.settle()]),
            None => {
                self.ctx.notify(format!("Unknown command {id}"));
                Task::none()
            }
        }
    }

    /// Runs a `/` command typed in the composer when it is the app's or an area's; `None` sends it to the harness.
    fn slash(&mut self, text: &str) -> Option<Task<Message>> {
        if let Some((spec, args)) = crate::area::find_slash(keys::APP_COMMANDS, text) {
            return Some((spec.run)(self, args));
        }
        let task = self.areas.iter_mut().find_map(|area| area.slash(text, &mut self.ctx))?;
        Some(Task::batch([task, self.settle()]))
    }

    fn composer_event(&mut self, event: Option<composer::Event>) -> Task<Message> {
        event.and_then(compose).map_or_else(Task::none, |intent| self.intent(intent))
    }

    fn intent(&mut self, intent: Intent) -> Task<Message> {
        match intent {
            Intent::Start => self.lifecycle(|s, ctx, areas| s.run(ctx, areas, Command::Start { prompt: None })),
            Intent::OpenSession(id) => self.lifecycle(|s, ctx, areas| s.run(ctx, areas, Command::Open(SessionId::from(id)))),
            Intent::Submit(prompt) => match self.slash(&prompt.text) {
                Some(task) => {
                    // Not a turn: the composer is ready again, and a message queued behind the command goes out now.
                    let next = self.ctx.composer.turn_ended();
                    Task::batch([task, self.composer_event(next)])
                }
                None if self.ctx.runtime.sessions().active().is_none() => self.lifecycle(|s, ctx, areas| s.run(ctx, areas, Command::Start { prompt: Some(prompt) })),
                None => self.lifecycle(|s, ctx, areas| s.send(ctx, areas, prompt)),
            },
            Intent::Interrupt => self.lifecycle(|s, ctx, areas| s.run(ctx, areas, Command::Interrupt)),
            Intent::Access(id) => self.lifecycle(|s, ctx, areas| s.set_access(ctx, areas, id)),
            Intent::Files => {
                self.files.refresh();
                self.ctx.composer.set_files(self.files.visible().map(|path| path.to_string_lossy().into_owned()).collect());
                Task::none()
            }
            Intent::Tab(tab) => self.open_tab(tab),
            Intent::Folder(path) => self.switch_folder(path),
            Intent::Browse => self.browse(),
            Intent::Usage => {
                self.windows.overlay = Some(Overlay::Usage);
                self.usage.refresh().map(Message::Usage)
            }
            Intent::Notice(notice) => {
                self.ctx.notify(notice);
                Task::none()
            }
            Intent::CloseProjectTab(project) => {
                self.close_project_tab(&project);
                Task::none()
            }
            Intent::Theme(picked) => {
                let active = self.ctx.shell.theme();
                self.sessions.chat.set_theme(&active);
                self.sessions.chat.set_bottom_inset(width(LayoutRole::ComposerInset));
                self.areas.get_mut::<areas::decisions::State>().set_theme(&active);
                if let Some(theme) = picked {
                    self.ctx.edit_config("settings", |config| config.appearance.theme = theme);
                }
                Task::none()
            }
            Intent::SavePins => {
                let pins = self.ctx.shell.sidebar().pins().clone();
                self.ctx.edit_config("settings", |config| config.pinned = pins);
                Task::none()
            }
            Intent::SaveProjectPins => {
                let pins = self.ctx.shell.sidebar().pinned_projects().clone();
                self.ctx.edit_config("settings", |config| config.pinned_projects = pins);
                Task::none()
            }
            Intent::Forward(message) => self.update(message),
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.handle(message);
        // The sidebar and top bar follow a project page in the center.
        let projects = (self.ctx.screen() == Some(areas::projects::State::ID)).then(|| self.areas.get::<areas::projects::State>().shown().map(str::to_owned));
        self.ctx.shell.set_shown_project(projects.flatten());
        self.sessions.sync_access(&mut self.ctx);
        self.sync_project_tabs();
        task
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Shell(message) => {
                let (task, event) = self.ctx.shell.update(message);
                if matches!(event, Some(shell::Event::Sidebar(Nav::NewSession | Nav::OpenSession(_) | Nav::OpenProject(_)))) {
                    (self.windows.configs, self.windows.overlay) = (None, None);
                }
                let action = event.and_then(route).map_or_else(Task::none, |intent| self.intent(intent));
                Task::batch([task.map(Message::Shell), action])
            }
            Message::Chat(message) => {
                let (task, event) = self.sessions.chat.update(message);
                let action = event.map_or_else(Task::none, |event| self.update(Message::area(areas::sessions::Message::Chat(event))));
                Task::batch([task.map(Message::Chat), action])
            }
            Message::Composer(message) => {
                let (task, event) = self.ctx.composer.update(message);
                Task::batch([task.map(Message::Composer), self.composer_event(event)])
            }
            Message::Files(message) => {
                let (task, event) = self.files.update(message);
                if let Some(files::Event::Open(path)) = event {
                    self.ctx.notify(path.display().to_string());
                }
                task.map(Message::Files)
            }
            Message::Diff(message) => self.diff.update(message).0.map(Message::Diff),
            Message::Terminal(message) => self.terminal.update(message).map(Message::Terminal),
            Message::Palette(message) => {
                if matches!(message, palette::Message::Open | palette::Message::OpenShortcuts) || matches!(message, palette::Message::Toggle) && !self.palette.is_open() {
                    self.refresh_commands();
                }
                let (task, event) = self.palette.update(message);
                let action = match event {
                    Some(palette::Event::Run(id)) => self.command(&id),
                    _ => Task::none(),
                };
                Task::batch([task.map(Message::Palette), action])
            }
            Message::Keyboard(keyboard::Event::KeyPressed { key: keyboard::Key::Named(key::Named::Escape), modifiers, .. })
                if !modifiers.control() && !modifiers.alt() && !modifiers.logo() =>
            {
                self.escape()
            }
            Message::Keyboard(event) => {
                // The palette is modal: only its own bindings apply while it shows.
                let scope = if self.palette.is_open() { palette::Scope::Palette } else { palette::Scope::Global };
                match self.ctx.keymap.resolve(&event, scope).cloned() {
                    Some(id) => self.command(&id),
                    None => Task::none(),
                }
            }
            Message::Tick => Task::none(),
            Message::Usage(message) => {
                let (task, event) = self.usage.update(message);
                match event {
                    Some(usage::Event::Close) => self.windows.overlay = None,
                    Some(usage::Event::Meter(meter)) => self.ctx.shell.sidebar_mut().set_usage(meter),
                    None => {}
                }
                task.map(Message::Usage)
            }
            Message::Window(message) => self.window_update(message),
            Message::FolderChosen(Ok(Some(path))) => self.switch_folder(path),
            Message::FolderChosen(Ok(None)) => Task::none(),
            Message::FolderChosen(Err(error)) => {
                self.ctx.notify(error);
                Task::none()
            }
            Message::DetachedChat(session, message) => {
                let Some(chat) = self.sessions.detached.get_mut(&session) else { return Task::none() };
                let (task, event) = chat.update(message);
                let task = task.map(move |message| Message::DetachedChat(session.clone(), message));
                // Rewinds act on the active session, so only links pass on.
                let action = match event {
                    Some(event @ chat::Event::OpenLink(_)) => self.update(Message::area(areas::sessions::Message::Chat(event))),
                    _ => Task::none(),
                };
                Task::batch([task, action])
            }
            Message::Starter(index) => match STARTERS.get(index) {
                Some((_, _, prompt)) => self.ctx.composer.set_text(prompt).map(Message::Composer),
                None => Task::none(),
            },
            Message::OpenRecent(id) => self.lifecycle(|s, ctx, areas| s.run(ctx, areas, Command::Open(SessionId::from(id)))),
            Message::Session(message) => self.lifecycle(|s, ctx, areas| s.update(ctx, areas, message)),
            Message::WorkingDiff(patch) => self.show_diff(patch),
            // The models screen's own close button, drawn inside Configs or Settings.
            Message::Area(_, message)
                if matches!(message.downcast_ref(), Some(areas::models::Message::Close))
                    && (self.windows.configs.is_some() || matches!(self.windows.overlay, Some(Overlay::Settings(_)))) =>
            {
                (self.windows.configs, self.windows.overlay) = (None, None);
                self.ctx.close_screen();
                Task::none()
            }
            Message::Area(id, message) => {
                let task = self.areas.update(id, message, &mut self.ctx);
                Task::batch([task, self.settle()])
            }
        }
    }

    /// Esc closes the innermost thing open, else interrupts the harness.
    fn escape(&mut self) -> Task<Message> {
        if self.palette.is_open() {
            return self.update(Message::Palette(palette::Message::Close));
        }
        let windows = &mut self.windows;
        if windows.overlay.take().is_some() || std::mem::take(&mut windows.docs.full) || windows.configs.take().is_some() {
            return Task::none();
        }
        if let Some(pane) = self.ctx.shell.full() {
            return self.update(Message::Shell(shell::Message::ToggleFull(pane)));
        }
        if self.areas.get::<areas::decisions::State>().focused().is_some() {
            return self.update(Message::area(areas::decisions::State::dismiss()));
        }
        if self.ctx.screen().is_some() {
            self.ctx.close_screen();
            return Task::none();
        }
        // Interrupt the harness itself, even before the turn-start patch arrives.
        self.lifecycle(|s, ctx, areas| s.run(ctx, areas, Command::Interrupt))
    }

    /// The decision card at `node` of session `session`.
    fn decision_card(&self, session: &str, node: NodeId) -> Element<'_, Message> {
        let card = self.areas.get::<areas::decisions::State>().card(session, node);
        card.map_or_else(|| text("Decision no longer available").into(), |card| card.map(Message::area))
    }

    pub fn view(&self) -> Element<'_, Message> {
        let ctx = &self.ctx;
        let screen = ctx.screen().and_then(|screen| self.areas.iter().find(|area| area.id() == screen)?.view_screen(ctx));
        let center = if let Some(tab) = self.windows.configs {
            self.configs_view(tab)
        } else if let Some(screen) = screen {
            screen
        } else if let Some(session) = ctx.runtime.sessions().active() {
            let transcript = self.sessions.chat.view(session.dom(), Message::Chat, |slot| self.decision_card(session.id().as_str(), slot.node));
            // The composer floats over the transcript's bottom edge; the transcript scrolls on underneath it.
            let composer = container(container(ctx.composer.view().map(Message::Composer)).width(Fill).max_width(width(LayoutRole::ComposerWidth)));
            let composer = composer.center_x(Fill).padding(Padding::from([sp(SpaceRole::M), sp(SpaceRole::Xl)]).top(sp(SpaceRole::Xl))).style(style::composer_fade);
            stack![container(transcript).height(Fill), container(composer).align_bottom(Fill)].into()
        } else {
            self.start_view()
        };
        let center = match ctx.notice() {
            Some(notice) => column![center, container(label(notice, TextRole::Caption, Tone::Muted)).padding([sp(SpaceRole::Xs), sp(SpaceRole::Xl)])].into(),
            None => center,
        };
        // A peer's project never lists its files: its disk is only reached through the grant-checked folder path.
        let project_files = (!areas::peers::is_remote(ctx.runtime.project())).then(|| self.files.view_compact().map(Message::Files));
        let frame = ctx.shell.view(center, |tab| self.view_tab(tab), project_files, Message::Shell);
        let frame: Element<'_, Message> = match self.windows.overlay {
            Some(overlay) => {
                let window = match overlay {
                    Overlay::Usage => self.usage.view().map(Message::Usage),
                    Overlay::Settings(tab) => self.settings_view(tab),
                };
                let scrim = mouse_area(container(space()).style(style::scrim).width(Fill).height(Fill)).on_press(Message::Window(WindowMessage::CloseSettings));
                let top = Padding::from(sp(SpaceRole::Xl)).top(width(LayoutRole::NavWidth) / 4.0);
                stack![frame, scrim, container(opaque(window)).center_x(Fill).padding(top)].into()
            }
            None => frame,
        };
        if self.palette.is_open() { stack![frame, self.palette.view().map(Message::Palette)].into() } else { frame }
    }

    /// The center with no session open: a heading, the composer, starters and recent sessions here.
    fn start_view(&self) -> Element<'_, Message> {
        let ctx = &self.ctx;
        let starting = ctx.runtime.sessions().starting().is_some();
        let project = ctx.runtime.project();
        let starters = STARTERS.iter().enumerate().map(|(index, (icon, title, _))| {
            let content = row![tinted(*icon, TextRole::Body, Tone::Muted), label(*title, TextRole::Body, Tone::Normal).width(Fill), tinted(Icon::ChevronRight, TextRole::Body, Tone::Muted)];
            list_row(content, (!starting).then_some(Message::Starter(index)))
        });
        let now = now_ms();
        let recents = ctx.shell.sidebar().recents().into_iter().filter(|summary| summary.project.as_deref() == Some(project)).take(RECENTS).map(|summary| {
            let content = row![label(summary.title.as_str(), TextRole::Body, Tone::Normal).width(Fill).wrapping(text::Wrapping::None), label(ago(now, summary.updated), TextRole::Caption, Tone::Muted)];
            list_row(content, Some(Message::OpenRecent(summary.id.clone())))
        });
        let recents: Vec<_> = recents.collect();
        let recent = (!recents.is_empty()).then(|| column![label(format!("Recent in {project}"), TextRole::Caption, Tone::Muted), divided(recents)].spacing(sp(SpaceRole::S)));
        let status = if starting { format!("Starting a session in {project}…") } else { format!("in {project}") };
        let heading = column![label("What should we work on?", TextRole::Title, Tone::Normal), label(status, TextRole::Body, Tone::Muted)];
        let body = column![heading.spacing(sp(SpaceRole::Xs)).width(Fill).align_x(Alignment::Center), ctx.composer.view().map(Message::Composer), divided(starters.collect())];
        let body = body.push(recent).spacing(sp(SpaceRole::L)).width(Fill).max_width(width(LayoutRole::ComposerWidth));
        container(scrollable(container(body).center_x(Fill).padding(pad_x(SpaceRole::Xl))).height(iced::Length::Shrink)).center(Fill).into()
    }

    fn view_tab<'a>(&'a self, tab: &'a str) -> Element<'a, Message> {
        match panes::find(self, tab) {
            Some(pane) => pane.view(self),
            None => self.areas.iter().find_map(|area| area.view_tab(tab, &self.ctx)).unwrap_or_else(|| text("Unknown tab").into()),
        }
    }

    pub fn theme(&self) -> Theme {
        self.ctx.shell.theme()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let busy = self.ctx.runtime.sessions().active().is_some_and(|session| session.is_busy());
        let areas = self.areas.iter().map(|area| area.subscription());
        let own = [
            self.ctx.shell.subscription().map(Message::Shell),
            self.ctx.composer.subscription().map(Message::Composer),
            iced::keyboard::listen().map(Message::Keyboard),
            crate::watch::sessions(self.ctx.runtime.sessions()).map(|(id, feed)| Message::Session(lifecycle::Message::Feed(id, feed))),
            if busy { iced::time::every(TICK).map(|_| Message::Tick) } else { Subscription::none() },
            iced::time::every(USAGE_REFRESH).map(|_| Message::Usage(usage::Message::Refresh)),
            iced::window::close_events().map(|id| Message::Window(WindowMessage::Closed(id))),
        ];
        Subscription::batch(own.into_iter().chain(areas))
    }
}

/// A full-width row button of a [`divided`] list.
fn list_row<'a>(content: iced::widget::Row<'a, Message>, on: Option<Message>) -> Element<'a, Message> {
    let content = content.spacing(sp(SpaceRole::M)).align_y(Alignment::Center);
    button(content).width(Fill).padding([sp(SpaceRole::S), sp(SpaceRole::M)]).style(loa_ui_kit::style::button(ButtonKind::Row { selected: false })).on_press_maybe(on).into()
}

/// `rows` in a card, divided by rules.
fn divided(rows: Vec<Element<'_, Message>>) -> Element<'_, Message> {
    let mut list = column![];
    for (index, row) in rows.into_iter().enumerate() {
        list = list.push((index > 0).then(|| rule::horizontal(1).style(style::divider))).push(row);
    }
    container(list).style(loa_ui_kit::style::container(Surface::Card)).width(Fill).into()
}
