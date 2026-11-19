//! The application: the shell frame, the chat, composer and decision cards of
//! the active session, the palette, the built-in panel tabs (Diff, Files,
//! Terminal) and the areas. Sessions start, resume and stream here; the
//! areas react through the [`Area`] contract.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use iced::keyboard::{self, key};
use iced::widget::{button, center, column, container, stack, text};
use iced::{Element, Fill, Font, Padding, Subscription, Task, Theme};
use loa_adapter::{AdapterError, HarnessSession, HostToolRouter, Prompt, SessionConfig};
use loa_bus::SessionId;
use loa_core::{Dom, ElementKind, Op, SessionMode, TurnStatus, attr};
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

/// Width of the composer: the transcript's column width.
const COMPOSER_WIDTH: f32 = 700.0;

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
    WorkingDiff(Option<String>),
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
}

pub struct App {
    pub(crate) ctx: Ctx,
    pub(crate) areas: Areas,
    pub(crate) chat: chat::State,
    pub(crate) palette: palette::State,
    files: files::State,
    diff: diff::State,
    terminal: terminal::state::State,
    usage: usage::State,
    overlay: Option<windows::Overlay>,
    configs: Option<windows::ConfigsTab>,
    docs: windows::Docs,
    main_window: Option<iced::window::Id>,
    detached: Vec<(iced::window::Id, detach::Detached)>,
    /// Transcripts of sessions shown in their own window, by session id.
    detached_chats: std::collections::HashMap<String, chat::State>,
}

impl App {
    /// Boots the daemon: the app on this machine's services, with its main
    /// window open.
    pub fn boot() -> (Self, Task<Message>) {
        let (mut app, task) = Self::new();
        let open = app.open_main();
        (app, Task::batch([open, task]))
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
    pub fn build(services: Services) -> (Self, Task<Message>) {
        // Sidebar pins and the theme are app data in the central config.
        let saved = services.config.load().map(|loaded| loaded.config).unwrap_or_default();
        let (shell, setup) =
            shell::State::new(preference(saved.theme), shell::PanelPrefs { open: true, ..Default::default() });
        let theme = shell.theme();
        let mut composer = composer::State::new();
        let mut sessions = Sessions::default();
        sessions.harness = services.harnesses.default_id();
        composer.set_harnesses(services.harnesses.options(), sessions.harness.as_deref());
        let (recents, problems) = session::recents(&services.library);
        let metrics = terminal::canvas::Metrics::measure(Font::MONOSPACE, 13.0);
        let terminal = terminal::state::State::new(metrics, terminal::local::LocalShell::login_shell(services.cwd.clone()));
        let files = files::State::new(services.cwd.clone(), Box::new(files::LocalFs));
        let cwd = services.cwd.clone();
        let mut ctx = Ctx {
            services,
            sessions,
            shell,
            composer,
            keymap: palette::Keymap::defaults(palette::Platform::current()),
            app_tools: Vec::new(),
            peers: Vec::new(),
            node: loa_node::NodeSlot::default(),
            screen: None,
            notice: (!problems.is_empty()).then(|| problems.join("; ")),
            requests: Vec::new(),
            events: Vec::new(),
        };
        ctx.shell.set_pins(saved.pinned);
        ctx.shell.set_sessions(recents);
        let (areas, area_tasks) = Areas::new(&mut ctx);
        let mut app = Self {
            ctx,
            areas,
            chat: chat::State::new(&theme),
            palette: palette::State::new(palette::Platform::current()),
            files,
            diff: diff::State::new(),
            terminal,
            usage: usage::State::default(),
            overlay: None,
            configs: None,
            docs: windows::Docs::default(),
            main_window: None,
            detached: Vec::new(),
            detached_chats: std::collections::HashMap::new(),
        };
        let shown = app.tabs().into_iter().filter(|tab| tab.shown).map(|tab| shell::PanelTab::new(tab.id, tab.title)).collect();
        app.ctx.shell.set_tabs(shown);
        app.refresh_commands();
        let settle = app.settle();
        let usage = app.usage.refresh().map(Message::Usage);
        (app, Task::batch([setup.map(Message::Shell), area_tasks, working_diff(cwd), settle, usage]))
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

    /// The config of a session on harness `harness`: this node and
    /// workspace, the session's own `cwd`, `project` and `mode` when it is
    /// resumed from `dom`, then every area's contribution and host tools.
    fn session_config(&mut self, session_id: &str, harness: &str, dom: Option<&Dom>) -> SessionConfig {
        let services = &self.ctx.services;
        let root = dom.and_then(Dom::root);
        let attr = |name| root.and_then(|root| root.str_attr(name));
        let mut config = SessionConfig {
            session_id: session_id.to_owned(),
            node: services.node.key.clone(),
            cwd: attr(attr::CWD).map_or_else(|| services.cwd.clone(), PathBuf::from),
            project: Some(attr(attr::PROJECT).unwrap_or(&services.project).to_owned()),
            mode: Some(attr(attr::MODE).and_then(SessionMode::parse).unwrap_or(SessionMode::Normal)),
            executable: services.harnesses.get(harness).and_then(|entry| entry.executable.clone()),
            ..SessionConfig::default()
        };
        let capabilities = services.harnesses.registry.get(harness).map(|adapter| adapter.capabilities()).unwrap_or_default();
        let target = Target { harness, capabilities: &capabilities, resume: dom.is_some() };
        let ctx = &self.ctx;
        with_areas!(each_area!(self.areas, ref |area, wrap| area.contribute(&mut config, &target, ctx)));
        let mut router = HostToolRouter::new();
        let mut duplicates = Vec::new();
        for set in with_areas!(each_area!(self.areas, ref |area, wrap| area.host_tools(&config, &target, ctx))).into_iter().flatten() {
            for spec in set.specs {
                if let Err(duplicate) = router.add(spec, Arc::clone(&set.handler)) {
                    duplicates.push(duplicate.0);
                }
            }
        }
        router.install(&mut config);
        if !duplicates.is_empty() {
            self.ctx.notify(format!("Host tools offered twice, first one kept: {}", duplicates.join(", ")));
        }
        config
    }

    /// Starts a session on the selected harness; `prompt` is sent once it
    /// runs.
    fn start(&mut self, prompt: Option<Prompt>) -> Task<Message> {
        if self.ctx.sessions.starting().is_some() {
            return Task::none();
        }
        let Some(harness) = self.ctx.sessions.harness.clone() else {
            self.ctx.notify("No harness is available on this machine");
            return Task::none();
        };
        let Some(adapter) = self.ctx.services.harnesses.runnable(&harness) else {
            let label = self.ctx.services.harnesses.label(&harness).to_owned();
            self.ctx.notify(format!("{label} is unavailable"));
            return Task::none();
        };
        let (id, writer) = match self.ctx.sessions.create(&self.ctx.services) {
            Ok(created) => created,
            Err(error) => {
                self.ctx.notify(error);
                return Task::none();
            }
        };
        self.ctx.sessions.set_starting(Some(id.clone()));
        self.ctx.clear_notice();
        let config = self.session_config(id.as_str(), &harness, None);
        Task::perform(
            async move {
                let sink: Arc<dyn loa_core::PatchSink> = writer;
                adapter.start(config, sink).await.map(Arc::from).map_err(|error| error.to_string())
            },
            move |result| Message::Started { id, result, prompt },
        )
    }

    /// Resumes the active session on its harness, then sends `prompt`. A
    /// harness that can't resume it, or isn't here, gets a new session.
    fn resume(&mut self, prompt: Prompt) -> Task<Message> {
        if self.ctx.sessions.starting().is_some() {
            self.ctx.notify("A session is already starting");
            return Task::none();
        }
        let Some(session) = self.ctx.sessions.active() else { return Task::none() };
        let id = session.id().clone();
        let harness = session.harness_id().map(str::to_owned);
        let adapter = harness.as_deref().and_then(|harness| self.ctx.services.harnesses.runnable(harness));
        let (Some(harness), Some(adapter)) = (harness, adapter) else {
            self.ctx.notify("This session's harness isn't available here; continuing in a new session");
            return self.start(Some(prompt));
        };
        let dom = session.dom().clone();
        let sink: Arc<dyn loa_core::PatchSink> = Arc::clone(session.writer()) as _;
        let config = self.session_config(id.as_str(), &harness, Some(&dom));
        self.ctx.sessions.set_starting(Some(id.clone()));
        self.ctx.sessions.harness = Some(harness);
        Task::perform(
            async move { adapter.resume(config, &dom, sink).await.map(Arc::from).map_err(ResumeError::from) },
            move |result| Message::Resumed { id, result, prompt },
        )
    }

    /// Sends `prompt` to the active session, resuming it first when no
    /// harness runs it.
    fn send(&mut self, mut prompt: Prompt) -> Task<Message> {
        let Some(session) = self.ctx.sessions.active() else { return Task::none() };
        let Some(harness) = session.harness().cloned() else { return self.resume(prompt) };
        let id = session.id().clone();
        with_areas!(each_area!(self.areas, mut |area, wrap| area.before_send(&id, &mut prompt, &mut self.ctx)));
        if let Some(session) = self.ctx.sessions.get_mut(id.as_str()) {
            session.set_busy(true);
        }
        self.ctx.composer.turn_started();
        let send = Task::perform(async move { harness.send(prompt).await.map_err(|error| error.to_string()) }, Message::Operation);
        Task::batch([send, self.settle()])
    }

    /// Starts a session an area creates itself: its config gets every
    /// area's contribution, then `spawn.start` creates and runs it.
    fn spawn(&mut self, spawn: Spawn) -> Task<Message> {
        if self.ctx.sessions.starting().is_some() {
            self.ctx.notify("A session is already starting");
            return Task::none();
        }
        let Spawn { id, harness, start, prompt } = spawn;
        let config = self.session_config(id.as_str(), &harness, None);
        self.ctx.sessions.set_starting(Some(id.clone()));
        self.ctx.clear_notice();
        Task::perform(start(config), move |result| Message::Spawned { id, result, prompt })
    }

    fn interrupt(&mut self) -> Task<Message> {
        let Some(session) = self.ctx.sessions.active() else { return Task::none() };
        let Some(harness) = session.harness().filter(|_| session.is_busy()).cloned() else { return Task::none() };
        Task::perform(async move { harness.interrupt().await.map_err(|error| error.to_string()) }, Message::Operation)
    }

    /// Shows session `id`, loading it from its journal when needed.
    fn open_session(&mut self, id: &str) -> Task<Message> {
        if self.ctx.sessions.is_active(id) {
            return Task::none();
        }
        if let Err(error) = self.ctx.sessions.load(&self.ctx.services, id) {
            self.ctx.notify(error);
            return Task::none();
        }
        let id = SessionId::from(id);
        self.ctx.sessions.set_active(Some(id.clone()));
        self.ctx.close_screen();
        self.dispatch(SessionEvent::Activated(Some(id)))
    }

    /// Loads the active session's DOM into the chat and the frame.
    fn present(&mut self) {
        let Some(session) = self.ctx.sessions.active() else {
            self.chat.load(&Dom::new());
            return;
        };
        self.chat.load(session.dom());
        if let Some(summary) = session.summary() {
            self.ctx.shell.upsert_session(summary);
        }
        self.ctx.shell.set_active_session(Some(session.id().to_string()));
        self.ctx.shell.set_session_status(turn_status(session.dom()).map(str::to_owned));
    }

    /// The app's own reaction to a session event, then every area's.
    fn dispatch(&mut self, event: SessionEvent) -> Task<Message> {
        match &event {
            SessionEvent::Activated(_) => self.present(),
            SessionEvent::Loaded(id) => {
                if self.ctx.sessions.is_active(id.as_str()) {
                    self.present();
                } else if let Some(summary) = self.ctx.sessions.get(id.as_str()).and_then(|session| session.summary()) {
                    self.ctx.shell.upsert_session(summary);
                }
            }
            SessionEvent::Patch(id, patch) => {
                let active = self.ctx.sessions.is_active(id.as_str());
                if let Some(session) = self.ctx.sessions.get(id.as_str()) {
                    if active {
                        self.chat.apply(patch, session.dom());
                    }
                    if let Some(chat) = self.detached_chats.get_mut(id.as_str()) {
                        chat.apply(patch, session.dom());
                    }
                    if changes_summary(session.dom(), patch) {
                        if let Some(summary) = session.summary() {
                            self.ctx.shell.upsert_session(summary);
                        }
                        if active {
                            self.ctx.shell.set_session_status(turn_status(session.dom()).map(str::to_owned));
                        }
                    }
                }
            }
            _ => {}
        }
        let tasks = with_areas!(each_area!(self.areas, mut |area, wrap| area.on_session_event(&event, &mut self.ctx).map(wrap)));
        Task::batch(tasks.into_iter().chain([self.settle()]))
    }

    /// Applies a session feed and reacts to the active session starting or
    /// finishing a turn.
    fn feed(&mut self, id: SessionId, feed: Feed) -> Task<Message> {
        let active = self.ctx.sessions.is_active(id.as_str());
        let was_busy = active && self.ctx.sessions.active().is_some_and(|session| session.is_busy());
        let mut tasks = Vec::new();
        match self.ctx.sessions.feed(id.as_str(), feed) {
            Change::None => return Task::none(),
            Change::Loaded => tasks.push(self.dispatch(SessionEvent::Loaded(id))),
            Change::Patch(patch, derived) => {
                tasks.push(self.dispatch(SessionEvent::Patch(id, patch)));
                if let Some(event) = derived {
                    tasks.push(self.dispatch(event));
                }
            }
        }
        let busy = active && self.ctx.sessions.active().is_some_and(|session| session.is_busy());
        if busy && !was_busy {
            self.ctx.composer.turn_started();
        }
        if active && was_busy && !busy {
            tasks.push(working_diff(self.ctx.services.cwd.clone()));
            // A queued `/` command runs like a typed one.
            let next = self.ctx.composer.turn_ended();
            tasks.push(self.composer_event(next));
        }
        Task::batch(tasks)
    }

    /// Runs what areas asked for and delivers the session events they
    /// caused, until none are left.
    fn settle(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        loop {
            if !self.ctx.events.is_empty() {
                for event in std::mem::take(&mut self.ctx.events) {
                    tasks.push(self.dispatch(event));
                }
                continue;
            }
            if self.ctx.requests.is_empty() {
                break;
            }
            for request in std::mem::take(&mut self.ctx.requests) {
                let task = match request {
                    Request::NewSession => self.start(None),
                    Request::OpenSession(id) => self.open_session(&id),
                    Request::DetachSession(id) => self.detach_session(&id),
                    Request::Prompt(prompt) => self.send(prompt),
                    Request::OpenTab(id) => self.open_tab(&id),
                    Request::RevealTab(id) => self.reveal_tab(&id),
                    Request::Spawn(spawn) => self.spawn(spawn),
                    Request::Forward(message) => self.update(*message),
                };
                tasks.push(task);
            }
        }
        Task::batch(tasks)
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
                None => self.send(prompt),
            },
            Some(composer::Event::Stop) => self.interrupt(),
            Some(composer::Event::HarnessChanged(id)) => self.update(Message::Harness(areas::harness::Message::Pick(id))),
            Some(composer::Event::ModelChanged(model)) => self.update(Message::Models(areas::models::Message::Model(model))),
            Some(composer::Event::ModelAction(id)) => self.update(Message::Models(areas::models::Message::Action(id))),
            Some(composer::Event::EffortChanged(effort)) => {
                self.update(Message::Models(areas::models::Message::Effort(effort)))
            }
            Some(composer::Event::ModeChanged(mode)) => self.update(Message::Sessions(areas::sessions::Message::Mode(mode))),
            Some(composer::Event::FilesRequested { .. }) => {
                self.files.refresh();
                self.ctx.composer.set_files(self.files.visible().map(|path| path.to_string_lossy().into_owned()).collect());
                Task::none()
            }
            Some(composer::Event::QueueChanged(_)) | None => Task::none(),
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
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
                    self.configs = None;
                    self.overlay = None;
                }
                let action = match event {
                    Some(shell::Event::NewSession) => self.start(None),
                    Some(shell::Event::OpenUsage) => {
                        self.overlay = Some(windows::Overlay::Usage);
                        self.usage.refresh().map(Message::Usage)
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
                    Some(shell::Event::SetTheme(preference)) => {
                        self.save_ui(|config| config.theme = theme_setting(preference));
                        Task::none()
                    }
                    Some(shell::Event::SetPinned { .. }) => {
                        let pins = self.ctx.shell.sidebar().pins().clone();
                        self.save_ui(|config| config.pinned = pins);
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
                    if self.overlay.take().is_some() {
                        return Task::none();
                    }
                    if self.docs.full {
                        self.docs.full = false;
                        return Task::none();
                    }
                    if self.configs.take().is_some() {
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
            Message::Session(id, feed) => self.feed(id, feed),
            Message::Spawned { id, result, prompt } => match result {
                Ok(harness) => match self.ctx.sessions.load(&self.ctx.services, id.as_str()) {
                    Ok(()) => self.update(Message::Started { id, result: Ok(harness), prompt }),
                    Err(error) => {
                        self.ctx.sessions.set_starting(None);
                        self.ctx.notify(error);
                        Task::perform(async move { harness.shutdown().await.map_err(|error| error.to_string()) }, Message::Operation)
                    }
                },
                Err(error) => {
                    self.ctx.sessions.set_starting(None);
                    // Leave no half created session behind; it may not exist.
                    let _ = self.ctx.services.library.delete(id.as_str());
                    self.ctx.notify(error);
                    Task::none()
                }
            },
            Message::Started { id, result, prompt } => {
                self.ctx.sessions.set_starting(None);
                let harness = match result {
                    Ok(harness) => harness,
                    Err(error) => {
                        // Nothing ran: drop the journal it was given.
                        let (_, closed) = self.ctx.sessions.close(&self.ctx.services, id.as_str(), true);
                        self.ctx.notify(match closed {
                            Some(closed) => format!("{error}; {closed}"),
                            None => error,
                        });
                        self.ctx.show_harness();
                        return Task::none();
                    }
                };
                let Some(session) = self.ctx.sessions.get_mut(id.as_str()) else {
                    // Discarded while starting.
                    return Task::perform(async move { harness.shutdown().await.map_err(|error| error.to_string()) }, Message::Operation);
                };
                session.attach(harness);
                self.ctx.sessions.set_active(Some(id.clone()));
                self.ctx.close_screen();
                self.files.refresh();
                let activated = self.dispatch(SessionEvent::Activated(Some(id)));
                let send = prompt.map_or_else(Task::none, |prompt| self.send(prompt));
                Task::batch([activated, working_diff(self.ctx.services.cwd.clone()), send])
            }
            Message::Resumed { id, result, prompt } => {
                self.ctx.sessions.set_starting(None);
                match result {
                    Ok(harness) => {
                        let Some(session) = self.ctx.sessions.get_mut(id.as_str()) else {
                            return Task::perform(async move { harness.shutdown().await.map_err(|error| error.to_string()) }, Message::Operation);
                        };
                        session.attach(harness);
                        let open = self.open_session(id.as_str());
                        Task::batch([open, self.send(prompt)])
                    }
                    Err(ResumeError::Unsupported(reason)) => {
                        self.ctx.notify(format!("Can't resume this session ({reason}); continuing in a new session"));
                        self.start(Some(prompt))
                    }
                    Err(ResumeError::Failed(error)) => {
                        self.ctx.notify(error);
                        Task::none()
                    }
                }
            }
            Message::Operation(result) => {
                if let Err(error) = result {
                    if let Some(session) = self.ctx.sessions.active_mut() {
                        session.set_busy(false);
                    }
                    self.ctx.notify(error);
                }
                Task::none()
            }
            Message::WorkingDiff(Some(patch)) => match diff::parse_unified(&patch) {
                Ok(files) => self.diff.set_files(files).map(Message::Diff),
                Err(error) => {
                    self.ctx.notify(format!("Can't read the working tree diff: {error}"));
                    Task::none()
                }
            },
            Message::WorkingDiff(None) => Task::none(),
            Message::Harness(message) => area!(harness, Harness, message),
            Message::Sessions(message) => area!(sessions, Sessions, message),
            Message::Decisions(message) => area!(decisions, Decisions, message),
            Message::Tools(message) => area!(tools, Tools, message),
            Message::Surfaces(message) => area!(surfaces, Surfaces, message),
            Message::Models(areas::models::Message::Close) if self.configs.is_some() || matches!(self.overlay, Some(windows::Overlay::Settings(_))) => {
                // The models screen's own close button, drawn inside Configs or Settings.
                self.configs = None;
                self.overlay = None;
                self.ctx.close_screen();
                Task::none()
            }
            Message::Models(message) => area!(models, Models, message),
            Message::Projects(message) => area!(projects, Projects, message),
            Message::Peers(message) => area!(peers, Peers, message),
            Message::Updater(message) => area!(updater, Updater, message),
            Message::Sandbox(message) => area!(sandbox, Sandbox, message),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let ctx = &self.ctx;
        let screen = ctx.screen().and_then(|screen| {
            with_areas!(on_area!(screen, self.areas, ref |area, wrap| area.view_screen(ctx).map(|view| view.map(wrap))))
        });
        let center_content: Element<'_, Message> = if let Some(tab) = self.configs {
            self.configs_view(tab)
        } else if let Some(screen) = screen {
            screen
        } else if let Some(session) = ctx.sessions.active() {
            let transcript = self.chat.view(session.dom(), Message::Chat, |slot| {
                self.areas
                    .decisions
                    .card(session.id().as_str(), slot.node)
                    .map(|card| card.map(Message::Decisions))
                    .unwrap_or_else(|| text("Decision no longer available").into())
            });
            column![
                container(transcript).height(Fill),
                container(container(ctx.composer.view().map(Message::Composer)).width(Fill).max_width(COMPOSER_WIDTH))
                    .center_x(Fill)
                    .padding(Padding { top: 8.0, right: 24.0, bottom: 16.0, left: 24.0 }),
            ]
            .into()
        } else {
            let starting = ctx.sessions.starting().is_some();
            center(
                column![
                    text(if starting { "Starting harness…" } else { "Start a session" }).size(20),
                    button(text("New session").size(14))
                        .style(style::accent_button)
                        .padding([8, 16])
                        .on_press_maybe((!starting).then_some(Message::NewSession)),
                ]
                .spacing(16)
                .align_x(iced::Alignment::Center),
            )
            .into()
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
                    windows::Overlay::Settings(tab) => self.settings_view(tab),
                };
                let scrim = iced::widget::mouse_area(container(iced::widget::space()).style(style::scrim).width(Fill).height(Fill))
                    .on_press(Message::Window(windows::WindowMessage::CloseSettings));
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
                iced::keyboard::listen().map(Message::Keyboard),
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

/// Whether `patch` may change the session's sidebar entry or status: its
/// title, project, waiting flag, last change or turn status.
fn changes_summary(dom: &Dom, patch: &loa_core::Patch) -> bool {
    match &patch.op {
        Op::Insert { kind, .. } => matches!(kind, ElementKind::Session | ElementKind::Turn | ElementKind::Message | ElementKind::Decision),
        Op::Set { name, .. } => {
            matches!(name.as_str(), attr::TITLE | attr::PROJECT | attr::STATUS | attr::STARTED | attr::ENDED)
                && dom.get(patch.node).is_some_and(|node| node.kind != ElementKind::Call)
        }
        Op::Remove => true,
        Op::Append { .. } => false,
    }
}

/// The top bar status of the session: the state of its latest turn while it
/// runs, waits, or ended interrupted or failed.
fn turn_status(dom: &Dom) -> Option<&'static str> {
    match dom_turns(dom).last()?.str_attr(attr::STATUS).and_then(TurnStatus::parse)? {
        TurnStatus::Running => Some("Running"),
        TurnStatus::Waiting => Some("Waiting"),
        TurnStatus::Cancelled => Some("Interrupted"),
        TurnStatus::Error => Some("Error"),
        TurnStatus::Done => None,
    }
}

/// Reads `git diff HEAD` of `cwd` for the Diff tab. `None` when `cwd` isn't a
/// git work tree or git isn't installed.
fn working_diff(cwd: PathBuf) -> Task<Message> {
    Task::perform(async move { git_diff(&cwd).await }, Message::WorkingDiff)
}

async fn git_diff(cwd: &Path) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(["diff", "--no-color", "--no-ext-diff", "HEAD"])
        .current_dir(cwd)
        .output()
        .await
        .ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

mod detach;
mod keys;
mod usage;
mod windows;

#[cfg(test)]
pub(crate) mod tests;
