//! The application: the shell frame, the chat, composer and decision cards of
//! the active session, the palette, the built-in panel tabs (Diff, Files,
//! Terminal) and the areas. Sessions start, resume and stream here; the
//! areas react through the [`Area`] contract.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use iced::keyboard::{self, key};
use iced::widget::{column, container, stack, text};
use iced::{Element, Fill, Font, Padding, Subscription, Task, Theme};
use loa_adapter::{AdapterError, HarnessSession, HostToolRouter, Prompt, SessionConfig};
use loa_bus::SessionId;
use loa_core::{Dom, Op, Patch, SessionMode, TurnStatus, attr};
use loa_ui_chat as chat;
use loa_ui_composer as composer;
use loa_ui_palette as palette;
use loa_ui_panel::{diff, files};
use loa_ui_shell as shell;
use loa_ui_shell::theme::style;
use loa_ui_terminal as terminal;

use crate::area::{Area, AreaId, Ctx, Request, SessionEvent, Sessions, Spawn, Target};
use crate::areas::{self, Areas, with_areas};
use crate::services::Services;
use crate::session::{self, Change, Feed, dom_turns};
use feed::turn_status;
pub(crate) use folders::choose_folder;
use folders::{working_diff, workspace};

/// Panel tabs the app draws itself, before the areas' tabs.
/// How often the sidebar's usage meter re-reads `omp usage`.
const USAGE_REFRESH: Duration = Duration::from_secs(300);

const BUILTIN_TABS: [(&str, &str); 3] = [("diff", "Diff"), ("files", "Files"), ("terminal", "Terminal")];

/// Theme choices offered in the palette, by command id suffix.
const THEMES: [(&str, &str, shell::Preference); 3] = [
    ("dark", "Dark", shell::Preference::Dark),
    ("light", "Light", shell::Preference::Light),
    ("system", "System", shell::Preference::System),
];

const TAB_COMMAND: &str = "tab.";
const THEME_COMMAND: &str = "theme.";
const NEW_SESSION: palette::CommandId = palette::CommandId::from_static("session.new");
const PROJECT_TABS_TOGGLE: palette::CommandId = palette::CommandId::from_static("project.tabs.toggle");

/// Width of the composer: the transcript's column width.
pub(crate) const COMPOSER_WIDTH: f32 = 700.0;

/// Room under the transcript's last row for the floating composer.
const COMPOSER_INSET: f32 = 190.0;

/// How often running sessions redraw, so elapsed times advance.
const TICK: Duration = Duration::from_secs(1);

/// `&$e` for `ref`, `&mut $e` for `mut`.
macro_rules! borrow {
    (ref, $e:expr) => {
        &$e
    };
    (mut, $e:expr) => {
        &mut $e
    };
}

/// `[$body; each area]`, with `$area` bound to the area (borrowed `ref` or
/// `mut`) and `$wrap` to its `Message` variant.
macro_rules! each_area {
    ($areas:expr, $borrow:ident |$area:ident, $wrap:ident| $body:expr; $($field:ident $variant:ident),+) => {
        [$({
            let $area = borrow!($borrow, $areas.$field);
            let $wrap = Message::$variant;
            let _ = $wrap;
            $body
        }),+]
    };
}

/// `$body` for the area `$id`, bound as in [`each_area`].
macro_rules! on_area {
    ($id:expr, $areas:expr, $borrow:ident |$area:ident, $wrap:ident| $body:expr; $($field:ident $variant:ident),+) => {
        match $id {
            $(AreaId::$variant => {
                let $area = borrow!($borrow, $areas.$field);
                let $wrap = Message::$variant;
                let _ = $wrap;
                $body
            })+
        }
    };
}

/// Why a session couldn't be resumed.
#[derive(Clone, Debug)]
pub enum ResumeError {
    /// The harness can't resume it; a new session takes the prompt.
    Unsupported(String),
    Failed(String),
}

impl From<AdapterError> for ResumeError {
    fn from(error: AdapterError) -> Self {
        match error {
            AdapterError::Unsupported { .. } | AdapterError::Format(_) => ResumeError::Unsupported(error.to_string()),
            error => ResumeError::Failed(error.to_string()),
        }
    }
}

type Started = Result<Arc<dyn HarnessSession>, String>;

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
    NewSession,
    /// A session's feed delivered something.
    Session(SessionId, Feed),
    /// A new session's harness started; `prompt` is sent once it runs.
    Started { id: SessionId, result: Started, prompt: Option<Prompt> },
    /// A session's harness resumed; `prompt` is sent once it runs.
    Resumed { id: SessionId, result: Result<Arc<dyn HarnessSession>, ResumeError>, prompt: Prompt },
    /// A session an area spawned (fork, rewind, compaction) started.
    Spawned { id: SessionId, result: Started, prompt: Option<Prompt> },
    /// A harness call returned.
    Operation(Result<(), String>),
    /// The working tree's changes against HEAD, already parsed off the UI thread.
    WorkingDiff(Option<Result<Vec<diff::FileDiff>, String>>),
    /// Sessions read from disk at startup. `empty` lists sessions with no turns.
    Recents {
        summaries: Vec<shell::SessionSummary>,
        problems: Vec<String>,
        empty: Vec<String>,
    },
    Harness(areas::harness::Message),
    Sessions(areas::sessions::Message),
    Decisions(areas::decisions::Message),
    Tools(areas::tools::Message),
    Surfaces(areas::surfaces::Message),
    Models(areas::models::Message),
    Projects(areas::projects::Message),
    Peers(areas::peers::Message),
    Updater(areas::updater::Message),
    Sandbox(areas::sandbox::Message),
    /// The Usage window.
    Usage(usage::Message),
    /// Configs and Settings.
    Window(windows::WindowMessage),
    /// A window closed (the main one or a detached one).
    WindowClosed(iced::window::Id),
    /// A detached window's Reattach button.
    Reattach(iced::window::Id),
    /// The transcript in a detached session's window.
    DetachedChat(String, chat::Message),
    /// The folder dialog closed with this folder, or none when cancelled.
    FolderChosen(Result<Option<PathBuf>, String>),
    /// The start screen.
    Start(start::StartMessage),
    Agents(areas::agents::Message),
    /// The clipboard image a paste asked for, saved to disk, or none.
    ImagePasted(Result<Option<media::Pasted>, String>),
    /// A file was dropped on a window.
    FileDropped(PathBuf),
}

pub struct App {
    pub(crate) ctx: Ctx,
    pub(crate) areas: Areas,
    pub(crate) chat: chat::State,
    pub(crate) palette: palette::State,
    pub(crate) files: files::State,
    diff: diff::State,
    pub(crate) terminal: terminal::state::State,
    usage: usage::State,
    overlay: Option<windows::Overlay>,
    docs: windows::Docs,
    main_window: Option<iced::window::Id>,
    detached: Vec<(iced::window::Id, detach::Detached)>,
    /// Transcripts of sessions shown in their own window, by session id.
    detached_chats: std::collections::HashMap<String, chat::State>,
    /// Folders worked in this run, most recent first, for the folder menu.
    recent_folders: Vec<PathBuf>,
    /// omp's approval mode for sessions started from now on; `None` uses
    /// the adapter's default (`always-ask`).
    access: Option<String>,
    /// The active session restarts on the new approval mode once its turn
    /// ends.
    restart_for_access: bool,
    /// What `sync_access` last pushed to the composer: whether the active session
    /// runs omp, and the permission level it showed.
    access_synced: Option<(bool, Option<String>)>,
    /// The projects with a tab in the project strip, in order. Not saved:
    /// projects selected this run plus the project of every session opened
    /// this run. Closing a tab only removes the entry here.
    pub(crate) open_projects: Vec<String>,
    /// Sessions whose project was already offered a tab, so a closed tab
    /// stays closed until its project is activated again.
    tabbed_sessions: HashSet<String>,
}

impl App {
    /// Boots the daemon: the app on this machine's services, with its main
    /// window open.
    pub fn boot() -> (Self, Task<Message>) {
        let (mut app, task) = Self::new();
        let open = app.open_main();
        let catalog = app.areas.models.read_catalog().map(Message::Models);
        (app, Task::batch([open, task, catalog]))
    }

    /// The app on this machine's services.
    pub fn new() -> (Self, Task<Message>) {
        let (services, warnings) = Services::load().unwrap_or_else(|error| panic!("{error}"));
        let (mut app, task) = Self::build(services);
        if !warnings.is_empty() {
            app.ctx.notify(warnings.join("; "));
        }
        (app, task)
    }

    /// The app over `services`: Recents from the session store, every area
    /// registered.
    pub fn build(mut services: Services) -> (Self, Task<Message>) {
        // Sidebar pins, the theme and the active project are app data in the central config.
        let saved = services.config.load().map(|loaded| loaded.config).unwrap_or_default();
        // The project selected last run, while its folder still exists, is the active one from the start.
        let restored = saved.active_project.filter(|active| active.cwd.is_dir());
        if let Some(active) = &restored {
            services.set_active(active.name.clone(), Some(active.cwd.clone()));
        }
        let (shell, setup) =
            shell::State::new(preference(saved.theme), shell::PanelPrefs { open: true, ..Default::default() });
        let theme = shell.theme();
        let mut composer = composer::State::new();
        let mut sessions = Sessions::default();
        sessions.harness = services.harnesses.default_id();
        composer.set_harnesses(services.harnesses.options(), sessions.harness.as_deref());
        let library = Arc::clone(&services.library);
        let metrics = terminal::canvas::Metrics::measure(Font::MONOSPACE, 13.0);
        let terminal = terminal::state::State::new(metrics, terminal::local::LocalShell::login_shell(services.cwd.clone()));
        let files = files::State::new(services.cwd.clone(), Box::new(files::LocalFs));
        let cwd = services.cwd.clone();
        let mut ctx = Ctx::new(
            services,
            sessions,
            shell,
            composer,
            palette::Keymap::defaults(palette::Platform::current()),
        );
        ctx.shell.set_project_tabs(saved.project_tabs);
        ctx.shell.set_active_project(restored.map(|active| active.name));
        ctx.shell.set_pins(saved.pinned);
        ctx.shell.set_pinned_projects(saved.pinned_projects);
        let (areas, area_tasks) = Areas::new(&mut ctx);
        let mut app = Self {
            ctx,
            areas,
            chat: { let mut chat = chat::State::new(&theme); chat.set_bottom_inset(COMPOSER_INSET); chat },
            palette: palette::State::new(palette::Platform::current()),
            files,
            diff: diff::State::new(),
            terminal,
            usage: usage::State::default(),
            overlay: None,
            docs: windows::Docs::default(),
            main_window: None,
            detached: Vec::new(),
            detached_chats: std::collections::HashMap::new(),
            recent_folders: Vec::new(),
            access: saved.harness.approval_mode.clone(),
            restart_for_access: false,
            access_synced: None,
            open_projects: Vec::new(),
            tabbed_sessions: HashSet::new(),
        };
        let shown = app.tabs().into_iter().filter(|tab| tab.shown).map(|tab| shell::PanelTab::new(tab.id, tab.title)).collect();
        app.ctx.shell.set_tabs(shown);
        let folder = workspace(&app.ctx.services.cwd);
        app.ctx.composer.set_workspace(Some(folder));
        app.recent_folders.push(app.ctx.services.cwd.clone());
        app.refresh_folders();
        app.refresh_commands();
        let settle = app.settle();
        let usage = app.usage.refresh().map(Message::Usage);
        // The sidebar fills in once the sessions on disk have been read.
        let recents = Task::perform(
            async move {
                tokio::task::spawn_blocking(move || session::recents(&library))
                    .await
                    .unwrap_or_else(|error| (Vec::new(), vec![format!("Can't read sessions: {error}")], Vec::new()))
            },
            |(summaries, problems, empty)| Message::Recents { summaries, problems, empty },
        );
        (app, Task::batch([setup.map(Message::Shell), area_tasks, working_diff(cwd), recents, settle, usage]))
    }

    /// Every panel tab: the built-in ones, then each area's.
    fn tabs(&self) -> Vec<crate::area::PanelTab> {
        let mut tabs: Vec<_> =
            BUILTIN_TABS.into_iter().map(|(id, title)| crate::area::PanelTab::new(id, title, true)).collect();
        for area_tabs in with_areas!(each_area!(self.areas, ref |area, wrap| area.panel_tabs())) {
            tabs.extend(area_tabs);
        }
        tabs
    }

    /// Lists the panel tab `id` when it isn't; `false` when no such tab
    /// exists.
    fn list_tab(&mut self, id: &str) -> bool {
        if !self.ctx.shell.tabs().iter().any(|tab| tab.id == id) {
            let Some(tab) = self.tabs().into_iter().find(|tab| tab.id == id) else {
                self.ctx.notify(format!("There is no {id} tab"));
                return false;
            };
            self.ctx.shell.add_tab(shell::PanelTab::new(tab.id, tab.title));
        }
        true
    }

    /// Adds the panel tab `id` when it isn't listed and shows it, scrolling
    /// the tab bar to it.
    fn open_tab(&mut self, id: &str) -> Task<Message> {
        if !self.list_tab(id) {
            return Task::none();
        }
        self.ctx.shell.show_tab(id);
        self.ctx.shell.reveal_active_tab()
    }

    /// Lists the panel tab `id` for something that just became relevant
    /// there (a job started, a branch was made), without taking the panel
    /// from the tab the user is reading: a closed panel opens on it, an
    /// open one only gains the tab.
    fn reveal_tab(&mut self, id: &str) -> Task<Message> {
        if !self.ctx.shell.panel_open() {
            return self.open_tab(id);
        }
        self.list_tab(id);
        Task::none()
    }

    /// Saves a change to the app's own config keys (theme, sidebar pins).
    fn save_ui(&mut self, change: impl FnOnce(&mut loa_config::Config)) {
        let saved = self.ctx.services.config.load().and_then(|mut loaded| {
            change(&mut loaded.config);
            self.ctx.services.config.save(&loaded.config)
        });
        if let Err(error) = saved {
            self.ctx.notify(format!("Can't save the settings: {error}"));
        }
    }

    /// The palette's commands: session and view actions, panel tabs, themes
    /// and each area's.
    fn refresh_commands(&mut self) {
        let keymap = &self.ctx.keymap;
        let mut commands = vec![
            palette::Command::new(NEW_SESSION, "New session", "Commands"),
            palette::Command::new(palette::ids::PANEL_TOGGLE, "Toggle right panel", "Commands")
                .shortcut(keymap.chord_for(&palette::ids::PANEL_TOGGLE)),
            palette::Command::new(palette::ids::SESSION_INTERRUPT, "Interrupt session", "Commands"),
            palette::Command::new(
                PROJECT_TABS_TOGGLE,
                if self.ctx.shell.project_tabs() { "Hide project tabs" } else { "Show project tabs" },
                "Commands",
            ),
            palette::Command::new(palette::ids::PALETTE_TOGGLE, "Command palette", "Commands")
                .shortcut(keymap.chord_for(&palette::ids::PALETTE_TOGGLE)),
        ];
        commands.extend(self.tabs().into_iter().map(|tab| {
            let id = palette::CommandId::new(format!("{TAB_COMMAND}{}", tab.id));
            palette::Command::new(id, format!("Open {} tab", tab.title), "Commands")
        }));
        commands.extend(THEMES.into_iter().map(|(key, label, _)| {
            palette::Command::new(palette::CommandId::new(format!("{THEME_COMMAND}{key}")), format!("Theme: {label}"), "Theme")
        }));
        commands.extend(self.keyboard_commands());
        for area_commands in with_areas!(each_area!(self.areas, ref |area, wrap| area.palette_commands(&self.ctx))) {
            commands.extend(area_commands);
        }
        self.hint_commands(&mut commands);
        if self.palette.commands() != commands.as_slice() {
            self.palette.set_commands(commands);
        }
    }

    /// Loads the active session's DOM into the chat and the frame. The
    /// composer follows its turn state, so it never waits on a session that
    /// isn't shown.
    fn present(&mut self) -> Task<Message> {
        self.ctx.composer.follow_session(self.ctx.sessions.active().is_some_and(|session| session.is_busy()));
        let Some(session) = self.ctx.sessions.active() else {
            self.chat.load(&Dom::new());
            let workspace = workspace(&self.ctx.services.cwd);
            self.ctx.composer.set_workspace(Some(workspace));
            let cwd = self.ctx.services.cwd.clone();
            return self.terminal_follow(cwd);
        };
        let cwd = session
            .dom()
            .root()
            .and_then(|root| root.str_attr(attr::CWD))
            .map_or_else(|| self.ctx.services.cwd.clone(), PathBuf::from);
        self.ctx.composer.set_workspace(Some(workspace(&cwd)));
        self.chat.load(session.dom());
        if let Some(summary) = session.summary() {
            self.ctx.shell.upsert_session(summary.clone());
        }
        self.ctx.shell.set_active_session(Some(session.id().to_string()));
        self.ctx.shell.set_session_status(turn_status(session.dom()).map(str::to_owned));
        self.terminal_follow(cwd)
    }

    pub(crate) fn command(&mut self, id: &palette::CommandId) -> Task<Message> {
        let id = id.as_str();
        if let Some(task) = self.keyboard_command(id) {
            return task;
        }
        if let Some(tab) = id.strip_prefix(TAB_COMMAND) {
            return self.open_tab(tab);
        }
        if let Some((_, _, preference)) =
            id.strip_prefix(THEME_COMMAND).and_then(|theme| THEMES.into_iter().find(|(key, _, _)| *key == theme))
        {
            return self.update(Message::Shell(shell::Message::SetTheme(preference)));
        }
        match id {
            "palette.toggle" => self.update(Message::Palette(palette::Message::Toggle)),
            "panel.toggle" => {
                self.ctx.shell.set_panel_open(!self.ctx.shell.panel_open());
                Task::none()
            }
            "session.new" => self.start(None),
            "project.tabs.toggle" => {
                let enabled = !self.ctx.shell.project_tabs();
                self.ctx.shell.set_project_tabs(enabled);
                self.save_ui(|config| config.project_tabs = enabled);
                Task::none()
            }
            "session.interrupt" => self.update(Message::Composer(composer::Message::Interrupt)),
            _ => {
                let Some(area) = AreaId::of_command(id) else {
                    self.ctx.notify(format!("Unknown command {id}"));
                    return Task::none();
                };
                let task = with_areas!(on_area!(area, self.areas, mut |area, wrap| area
                    .run_command(id, &mut self.ctx)
                    .map(|task| task.map(wrap))));
                let Some(task) = task else {
                    self.ctx.notify(format!("Unknown command {id}"));
                    return Task::none();
                };
                Task::batch([task, self.settle()])
            }
        }
    }

    /// Runs a `/` command typed in the composer when it is one of the
    /// app's: `/new`, `/model <id>` (the models area) and the sessions
    /// area's. `None` for anything else, which goes to the harness as typed,
    /// its own commands included.
    fn slash(&mut self, text: &str) -> Option<Task<Message>> {
        let (name, model) = areas::sessions::split_slash(text)?;
        match name {
            "new" => Some(self.start(None)),
            "model" if model.is_empty() => {
                self.ctx.notify("Name the model: /model <provider/model>, or pick one in the model picker");
                Some(Task::none())
            }
            "model" => {
                let option =
                    composer::ModelOption { id: model.to_owned(), name: model.to_owned(), provider: String::new(), role: None };
                Some(self.update(Message::Models(areas::models::Message::Model(option))))
            }
            _ => areas::sessions::slash(text).map(|message| self.update(Message::Sessions(message))),
        }
    }

    fn composer_event(&mut self, event: Option<composer::Event>) -> Task<Message> {
        match event {
            Some(composer::Event::Submit(prompt)) => match self.slash(&prompt.text) {
                Some(task) => {
                    // Not a turn: the composer is ready again, and a message
                    // queued behind the command goes out now.
                    let next = self.ctx.composer.turn_ended();
                    Task::batch([task, self.composer_event(next)])
                }
                None if self.ctx.sessions.active().is_none() => self.start(Some(prompt)),
                None => self.send(prompt),
            },
            Some(composer::Event::Stop) => self.interrupt(),
            Some(composer::Event::HarnessChanged(id)) => self.update(Message::Harness(areas::harness::Message::Pick(id))),
            Some(composer::Event::ModelChanged(model)) => self.update(Message::Models(areas::models::Message::Model(model))),
            Some(composer::Event::ModelAction(id)) => self.update(Message::Models(areas::models::Message::Action(id))),
            Some(composer::Event::EffortChanged(effort)) => {
                self.update(Message::Models(areas::models::Message::Effort(effort)))
            }
            Some(composer::Event::AccessChanged(id)) => self.set_access(id),
            Some(composer::Event::ModeChanged(mode)) => self.update(Message::Sessions(areas::sessions::Message::Mode(mode))),
            Some(composer::Event::FilesRequested { .. }) => {
                self.files.refresh();
                self.ctx.composer.set_files(self.files.visible().map(|path| path.to_string_lossy().into_owned()).collect());
                Task::none()
            }
            Some(composer::Event::OpenWorkspace) => self.open_tab("files"),
            Some(composer::Event::FolderPicked(path)) => self.switch_folder(PathBuf::from(path)),
            Some(composer::Event::BrowseFolders) => Task::perform(choose_folder(self.ctx.services.cwd.clone()), Message::FolderChosen),
            Some(composer::Event::ImagePasteRequested) => self.paste_image(),
            Some(composer::Event::ProjectPicked(project)) => self.project_picked(project),
            Some(composer::Event::QueueChanged(_)) | None => Task::none(),
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.handle(message);
        // The sidebar and top bar follow a project page in the center.
        let shown = (self.ctx.screen() == Some(AreaId::Projects)).then(|| self.areas.projects.shown().map(str::to_owned)).flatten();
        self.ctx.shell.set_shown_project(shown);
        self.sync_access();
        self.sync_project_tabs();
        task
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        macro_rules! area {
            ($field:ident, $variant:ident, $message:expr) => {{
                let task = self.areas.$field.update($message, &mut self.ctx).map(Message::$variant);
                Task::batch([task, self.settle()])
            }};
        }
        match message {
            Message::Shell(message) => {
                let (task, event) = self.ctx.shell.update(message);
                if matches!(event, Some(shell::Event::NewSession | shell::Event::OpenSession(_) | shell::Event::OpenProject(_))) {
                    self.overlay = None;
                }
                let action = match event {
                    Some(shell::Event::NewSession) => self.start(None),
                    Some(shell::Event::OpenUsage) => {
                        self.overlay = Some(windows::Overlay::Usage);
                        self.usage.refresh().map(Message::Usage)
                    }
                    Some(shell::Event::NewProject) => self.update(Message::Projects(areas::projects::Message::New)),
                    Some(shell::Event::MoveSession { id, project }) => {
                        self.update(Message::Projects(areas::projects::Message::Move { session: id, project }))
                    }
                    Some(shell::Event::OpenConfigs) => {
                        self.window_update(windows::WindowMessage::OpenConfigs(windows::ConfigsTab::Models))
                    }
                    Some(shell::Event::OpenSettings) => {
                        self.window_update(windows::WindowMessage::OpenSettings(windows::SettingsTab::General))
                    }
                    Some(shell::Event::OpenTools) => self.update(Message::Tools(areas::tools::Message::Open)),
                    Some(shell::Event::DetachTab(tab)) => self.detach(detach::Detached::Tab(tab)),
                    Some(shell::Event::ShowDetached(tab)) => self.show_detached_tab(&tab),
                    Some(shell::Event::ReattachTab(tab)) => self.reattach_tab(&tab),
                    Some(shell::Event::OpenPeers) => {
                        self.window_update(windows::WindowMessage::OpenSettings(windows::SettingsTab::Peers))
                    }
                    Some(shell::Event::OpenUpdates) => self.update(Message::Updater(areas::updater::Message::Open)),
                    Some(shell::Event::OpenSession(id)) if areas::peers::is_remote(&id) => {
                        self.update(Message::Peers(areas::peers::Message::OpenRecent(id)))
                    }
                    Some(shell::Event::DeleteSession(id)) if areas::peers::is_remote(&id) => {
                        self.update(Message::Peers(areas::peers::Message::ForgetRecent(id)))
                    }
                    Some(shell::Event::OpenSession(id)) => self.open_session(&id),
                    Some(shell::Event::DeleteSession(id)) => self.update(Message::Sessions(areas::sessions::Message::Delete(id))),
                    Some(shell::Event::OpenProject(project)) => {
                        self.update(Message::Projects(areas::projects::Message::Open(project)))
                    }
                    Some(shell::Event::CloseProjectTab(project)) => {
                        self.close_project_tab(&project);
                        Task::none()
                    }
                    Some(shell::Event::SetTheme(preference)) => {
                        self.save_ui(|config| config.theme = theme_setting(preference));
                        Task::none()
                    }
                    Some(shell::Event::SetPinned { .. }) => {
                        let pins = self.ctx.shell.sidebar().pins().clone();
                        self.save_ui(|config| config.pinned = pins);
                        Task::none()
                    }
                    Some(shell::Event::SetProjectPinned { .. }) => {
                        let pins = self.ctx.shell.sidebar().pinned_projects().clone();
                        self.save_ui(|config| config.pinned_projects = pins);
                        Task::none()
                    }
                    _ => Task::none(),
                };
                Task::batch([task.map(Message::Shell), action])
            }
            Message::Chat(message) => {
                let (task, event) = self.chat.update(message);
                let action = match event {
                    Some(event) => self.update(Message::Sessions(areas::sessions::Message::Chat(event))),
                    None => Task::none(),
                };
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
            Message::Terminal(message) => self.terminal.update(message).0.map(Message::Terminal),
            Message::Palette(message) => {
                if matches!(message, palette::Message::Open | palette::Message::OpenShortcuts)
                    || matches!(message, palette::Message::Toggle) && !self.palette.is_open()
                {
                    self.refresh_commands();
                }
                let (task, event) = self.palette.update(message);
                let action = match event {
                    Some(palette::Event::Run(id)) => self.command(&id),
                    _ => Task::none(),
                };
                Task::batch([task.map(Message::Palette), action])
            }
            Message::Keyboard(event) => {
                if let keyboard::Event::KeyPressed { key: keyboard::Key::Named(key::Named::Escape), modifiers, .. } = &event
                    && !modifiers.control()
                    && !modifiers.alt()
                    && !modifiers.logo()
                {
                    if self.palette.is_open() {
                        return self.update(Message::Palette(palette::Message::Close));
                    }
                    // A file's full screen leaves before the Configs window it is drawn in.
                    if self.docs.full && matches!(self.overlay, Some(windows::Overlay::Configs(_))) {
                        self.docs.full = false;
                        return Task::none();
                    }
                    if self.overlay.take().is_some() {
                        return Task::none();
                    }
                    if let Some(pane) = self.ctx.shell.full() {
                        return self.update(Message::Shell(shell::Message::ToggleFull(pane)));
                    }
                    if self.areas.decisions.focused().is_some() {
                        return self.update(Message::Decisions(areas::decisions::State::dismiss()));
                    }
                    if self.ctx.screen().is_some() {
                        self.ctx.close_screen();
                        return Task::none();
                    }
                    // Interrupt the harness itself, even before the turn-start patch arrives.
                    return self.interrupt();
                }
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
                    Some(usage::Event::Close) => self.overlay = None,
                    Some(usage::Event::Meter(meter)) => self.ctx.shell.set_usage(meter),
                    None => {}
                }
                task.map(Message::Usage)
            }
            Message::Window(message) => self.window_update(message),
            Message::WindowClosed(id) => self.window_closed(id),
            Message::Reattach(id) => self.reattach(id),
            Message::ImagePasted(pasted) => self.image_pasted(pasted),
            Message::FileDropped(path) => self.file_dropped(path),
            Message::FolderChosen(Ok(Some(path))) => self.switch_folder(path),
            Message::FolderChosen(Ok(None)) => Task::none(),
            Message::FolderChosen(Err(error)) => {
                self.ctx.notify(error);
                Task::none()
            }
            Message::DetachedChat(session, message) => {
                let Some(chat) = self.detached_chats.get_mut(&session) else { return Task::none() };
                let (task, event) = chat.update(message);
                let task = task.map(move |message| Message::DetachedChat(session.clone(), message));
                // Rewinds act on the active session, so only links pass on.
                let action = match event {
                    Some(event @ chat::Event::OpenLink(_)) => self.update(Message::Sessions(areas::sessions::Message::Chat(event))),
                    _ => Task::none(),
                };
                Task::batch([task, action])
            }
            Message::NewSession => self.start(None),
            Message::Start(start::StartMessage::Open(id)) => self.open_session(&id),
            Message::Session(id, feed) => self.feed(id, feed),
            Message::Recents { summaries, problems, empty } => self.recents_loaded(summaries, problems, empty),
            Message::Spawned { id, result, prompt } => self.on_spawned(id, result, prompt),
            Message::Started { id, result, prompt } => self.on_started(id, result, prompt),
            Message::Resumed { id, result, prompt } => self.on_resumed(id, result, prompt),
            Message::Operation(result) => {
                if let Err(error) = result {
                    if let Some(session) = self.ctx.sessions.active_mut() {
                        session.set_busy(false);
                    }
                    self.ctx.notify(error);
                }
                Task::none()
            }
            Message::WorkingDiff(Some(Ok(files))) => {
                let _ = self.diff.set_note(None);
                self.diff.set_files(files).map(Message::Diff)
            }
            Message::WorkingDiff(Some(Err(error))) => {
                self.ctx.notify(error);
                Task::none()
            }
            // Not a git repository (or no commit yet): say so instead of
            // keeping another folder's changes.
            Message::WorkingDiff(None) => {
                let folder = workspace(&self.ctx.services.cwd).path;
                self.diff.set_note(Some(format!("No git changes to show: {folder} isn't a git repository with a commit"))).map(Message::Diff)
            }
            Message::Harness(message) => area!(harness, Harness, message),
            Message::Sessions(message) => area!(sessions, Sessions, message),
            Message::Decisions(message) => area!(decisions, Decisions, message),
            Message::Tools(message) => area!(tools, Tools, message),
            Message::Surfaces(message) => area!(surfaces, Surfaces, message),
            Message::Models(areas::models::Message::Close) if matches!(self.overlay, Some(windows::Overlay::Settings(_) | windows::Overlay::Configs(_))) => {
                // The models screen's own close button, drawn inside Configs or Settings.
                self.overlay = None;
                self.ctx.close_screen();
                Task::none()
            }
            Message::Models(message) => area!(models, Models, message),
            Message::Projects(message) => area!(projects, Projects, message),
            Message::Peers(message) => area!(peers, Peers, message),
            Message::Updater(message) => area!(updater, Updater, message),
            Message::Sandbox(message) => area!(sandbox, Sandbox, message),
            Message::Agents(message) => area!(agents, Agents, message),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let ctx = &self.ctx;
        let screen = ctx.screen().and_then(|screen| {
            with_areas!(on_area!(screen, self.areas, ref |area, wrap| area.view_screen(ctx).map(|view| view.map(wrap))))
        });
        let center_content: Element<'_, Message> = if let Some(screen) = screen {
            screen
        } else if let Some(session) = ctx.sessions.active() {
            let transcript = self.chat.view(session.dom(), Message::Chat, |slot| {
                self.areas
                    .decisions
                    .card(session.id().as_str(), slot.node)
                    .map(|card| card.map(Message::Decisions))
                    .unwrap_or_else(|| text("Decision no longer available").into())
            });
            // The composer floats over the transcript's bottom edge; the
            // transcript scrolls on underneath it (see `COMPOSER_INSET`).
            let composer = container(container(ctx.composer.view().map(Message::Composer)).width(Fill).max_width(COMPOSER_WIDTH))
                .center_x(Fill)
                .padding(Padding { top: 28.0, right: 24.0, bottom: 16.0, left: 24.0 })
                .style(style::composer_fade);
            stack![container(transcript).height(Fill), container(composer).align_bottom(Fill)].into()
        } else {
            self.start_view()
        };
        let center_content: Element<'_, Message> = match ctx.notice() {
            Some(notice) => {
                column![center_content, container(text(notice).size(12).style(style::muted)).padding([6, 24])].into()
            }
            None => center_content,
        };
        let frame = ctx.shell.view(center_content, |tab| self.view_tab(tab), Message::Shell);
        let frame: Element<'_, Message> = match self.overlay {
            Some(overlay) => {
                let window = match overlay {
                    windows::Overlay::Usage => self.usage.view().map(Message::Usage),
                    windows::Overlay::Configs(tab) => self.configs_view(tab),
                    windows::Overlay::Settings(tab) => self.settings_view(tab),
                };
                let scrim = iced::widget::mouse_area(container(iced::widget::space()).style(style::scrim).width(Fill).height(Fill))
                    .on_press(Message::Window(windows::WindowMessage::CloseOverlay));
                stack![frame, scrim, container(iced::widget::opaque(window)).center_x(Fill).padding(Padding { top: 64.0, right: 24.0, bottom: 24.0, left: 24.0 })].into()
            }
            None => frame,
        };
        if self.palette.is_open() { stack![frame, self.palette.view().map(Message::Palette)].into() } else { frame }
    }

    fn view_tab<'a>(&'a self, tab: &'a str) -> Element<'a, Message> {
        match tab {
            "files" => self.files.view().map(Message::Files),
            "diff" => self.diff.view().map(Message::Diff),
            "terminal" => self.terminal.view().map(Message::Terminal),
            _ => with_areas!(each_area!(self.areas, ref |area, wrap| area.view_tab(tab, &self.ctx).map(|view| view.map(wrap))))
                .into_iter()
                .flatten()
                .next()
                .unwrap_or_else(|| text("Unknown tab").into()),
        }
    }

    pub fn theme(&self) -> Theme {
        self.ctx.shell.theme()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let busy = self.ctx.sessions.active().is_some_and(|session| session.is_busy());
        let areas = with_areas!(each_area!(self.areas, ref |area, wrap| area.subscription().map(wrap)));
        Subscription::batch(
            [
                self.ctx.shell.subscription().map(Message::Shell),
                self.ctx.composer.subscription().map(Message::Composer),
                iced::event::listen_with(key_press),
                iced::event::listen_with(file_drop),
                self.ctx.sessions.watch().map(|(id, feed)| Message::Session(id, feed)),
                if busy { iced::time::every(TICK).map(|_| Message::Tick) } else { Subscription::none() },
                iced::time::every(USAGE_REFRESH).map(|_| Message::Usage(usage::Message::Refresh)),
                iced::window::close_events().map(Message::WindowClosed),
            ]
            .into_iter()
            .chain(areas),
        )
    }
}

/// The shell's theme preference for the saved setting.
fn preference(theme: loa_config::Theme) -> shell::Preference {
    match theme {
        loa_config::Theme::System => shell::Preference::System,
        loa_config::Theme::Dark => shell::Preference::Dark,
        loa_config::Theme::Light => shell::Preference::Light,
    }
}

/// The saved setting for the shell's theme preference.
fn theme_setting(preference: shell::Preference) -> loa_config::Theme {
    match preference {
        shell::Preference::System => loa_config::Theme::System,
        shell::Preference::Dark => loa_config::Theme::Dark,
        shell::Preference::Light => loa_config::Theme::Light,
    }
}

/// Keeps only key presses that no widget captured. Releases and modifier
/// changes would each cost a full update and view.
fn key_press(event: iced::event::Event, status: iced::event::Status, _: iced::window::Id) -> Option<Message> {
    match (event, status) {
        (iced::event::Event::Keyboard(event @ keyboard::Event::KeyPressed { .. }), iced::event::Status::Ignored) => {
            Some(Message::Keyboard(event))
        }
        _ => None,
    }
}

/// Files dropped on a window go to the composer.
fn file_drop(event: iced::event::Event, _: iced::event::Status, _: iced::window::Id) -> Option<Message> {
    match event {
        iced::event::Event::Window(iced::window::Event::FileDropped(path)) => Some(Message::FileDropped(path)),
        _ => None,
    }
}

mod detach;
mod start;
mod keys;
mod usage;
mod windows;
mod access;
mod feed;
mod flow;
mod media;
mod folders;
mod projects;

#[cfg(test)]
pub(crate) mod tests;
