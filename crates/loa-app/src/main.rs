//! Desktop application wiring: the components own presentation state while the
//! adapter owns execution and the session DOM remains the source of truth.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use iced::keyboard::{self, key};
use iced::widget::{button, center, column, container, stack, text};
use iced::{Element, Fill, Font, Padding, Subscription, Task, Theme};
use loa_adapter::{HarnessSession, ModelSelection, Prompt, SessionConfig};
use loa_core::{Dom, ElementKind, MemorySink, NodeId, SessionMode, TurnStatus, attr};
use loa_ui_chat as chat;
use loa_ui_composer as composer;
use loa_ui_decisions as decisions;
use loa_ui_models as models;
use loa_ui_palette as palette;
use loa_ui_panel::{agents, diff, files, session_tree};
use loa_ui_peers as peers;
use loa_ui_surface as surface;
use loa_ui_shell as shell;
use loa_ui_shell::theme::style;
use loa_ui_terminal as terminal;
use loa_ui_tools as tools;

mod harness;
mod host;

use harness::Harnesses;

/// Key of this node in `<session node>`.
const NODE: &str = "local";

/// Panel tabs shown from the start, in order.
const TABS: [(&str, &str); 4] = [("diff", "Diff"), ("files", "Files"), ("terminal", "Terminal"), ("tools", "Tools")];

/// Panel tabs added when a palette command or sidebar item asks for them.
const EXTRA_TABS: [(&str, &str); 4] = [("agents", "Agents"), ("session", "Session tree"), ("peers", "Peers"), ("surface", "Surface")];

/// Theme choices offered in the palette, by command id suffix.
const THEMES: [(&str, &str, shell::Preference); 3] = [
    ("dark", "Dark", shell::Preference::Dark),
    ("light", "Light", shell::Preference::Light),
    ("system", "System", shell::Preference::System),
];

const TAB_COMMAND: &str = "tab.";
const HARNESS_COMMAND: &str = "harness.";
const THEME_COMMAND: &str = "theme.";
const NEW_SESSION: palette::CommandId = palette::CommandId::from_static("session.new");

/// Width of the composer: the transcript's column width.
const COMPOSER_WIDTH: f32 = 700.0;

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("Loa")
        .theme(App::theme)
        .subscription(App::subscription)
        .default_font(Font::with_name("Noto Sans"))
        .window_size((1440.0, 900.0))
        .run()
}

// A session carries its own writer and DOM. Tasks retain the harness handle so
// interrupt and answer can proceed concurrently with a streaming send.
struct Session {
    id: String,
    sink: Arc<MemorySink>,
    dom: Dom,
    harness: Arc<dyn HarnessSession>,
    busy: bool,
}

type StartedSession = Result<(String, Arc<MemorySink>, Arc<dyn HarnessSession>), String>;

#[derive(Clone)]
enum Message {
    Shell(shell::layout::Message),
    Chat(chat::Message),
    Composer(composer::Message),
    Decisions(decisions::Message),
    Agents(agents::Message),
    Files(files::Message),
    Diff(diff::Message),
    Tree(session_tree::Message),
    Peers(peers::Message),
    Surface(surface::Message),
    Terminal(terminal::state::Message),
    Palette(palette::Message),
    Tools(tools::Message),
    Keyboard(keyboard::Event),
    Poll,
    NewSession,
    Started(StartedSession),
    Operation(Result<(), String>),
    Answered(NodeId, Result<(), String>),
    Closed(Result<(), String>),
    WorkingDiff(Option<String>),
}

struct App {
    harnesses: Harnesses,
    /// Harness new sessions start on.
    harness: Option<String>,
    state_dir: PathBuf,
    cwd: PathBuf,
    /// Name of the project, the last component of `cwd`.
    project: String,
    shell: shell::layout::State,
    chat: chat::State,
    composer: composer::State,
    decisions: decisions::State,
    agents: agents::State,
    files: files::State,
    diff: diff::State,
    tree: session_tree::State,
    terminal: terminal::state::State,
    palette: palette::State,
    keymap: palette::Keymap,
    tools: tools::State,
    peers: peers::State,
    surface: Option<surface::State>,
    session: Option<Session>,
    sessions: HashMap<String, Session>,
    starting: bool,
    notice: Option<String>,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let state_dir = host::data_dir();
        let harnesses = Harnesses::detect(NODE, &state_dir.join("omp"));
        Self::build(harnesses, state_dir, host::hostname())
    }

    fn build(harnesses: Harnesses, state_dir: PathBuf, hostname: String) -> (Self, Task<Message>) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let project = cwd.file_name().map_or_else(|| cwd.display().to_string(), |name| name.to_string_lossy().into_owned());
        let (mut shell, setup) = shell::layout::State::new(
            shell::theme::Preference::System,
            shell::layout::PanelPrefs { open: true, ..Default::default() },
        );
        shell.set_tabs(TABS.into_iter().map(|(id, title)| shell::layout::PanelTab::new(id, title)).collect());
        shell.set_projects([project.clone()]);
        shell.set_nodes(vec![shell::NodeStatus { key: NODE.into(), name: hostname, online: true, local: true }]);
        let theme = shell.theme();
        let keymap = palette::Keymap::defaults(palette::Platform::current());
        let mut palette = palette::State::new(palette::Platform::current());
        palette.set_commands(commands(&harnesses, &keymap));
        let harness = harnesses.default_id();
        let mut composer = composer::State::new();
        composer.set_harnesses(harnesses.options(), harness.as_deref());
        let metrics = terminal::canvas::Metrics::measure(Font::MONOSPACE, 13.0);
        let terminal = terminal::state::State::new(metrics, terminal::local::LocalShell::login_shell(cwd.clone()));
        let dom = Dom::new();
        let app = Self {
            harnesses, harness, state_dir, cwd: cwd.clone(), project, shell,
            chat: chat::State::new(&theme), composer,
            decisions: decisions::State::new(decisions::Viewer { by: NODE.into(), view_only: false }, &theme),
            agents: agents::State::from_dom(&dom), files: files::State::new(cwd.clone(), Box::new(files::LocalFs)),
            diff: diff::State::new(), tree: session_tree::State::new(), terminal, palette, keymap,
            tools: tools::State::new(), session: None, sessions: HashMap::new(), peers: peers::State::default(),
            surface: None, starting: false, notice: None,
        };
        (app, Task::batch([setup.map(Message::Shell), working_diff(cwd)]))
    }

    fn start(&mut self) -> Task<Message> {
        if self.starting { return Task::none(); }
        let Some(id) = self.harness.clone() else {
            self.notice = Some("No harness is available on this machine".into());
            return Task::none();
        };
        let Some(adapter) = self.harnesses.registry.get(&id) else {
            self.notice = Some(format!("{} is unavailable", self.harnesses.label(&id)));
            return Task::none();
        };
        if let Err(error) = std::fs::create_dir_all(&self.state_dir) {
            self.notice = Some(format!("Can't create {}: {error}", self.state_dir.display()));
            return Task::none();
        }
        self.starting = true;
        self.notice = None;
        let session_id = format!("loa-{}-{}", std::process::id(), loa_core::now_ms());
        let sink = Arc::new(MemorySink::new());
        let config = SessionConfig {
            session_id: session_id.clone(), node: NODE.into(), cwd: self.cwd.clone(),
            project: Some(self.project.clone()),
            model: None, model_role: None, effort: None, mode: Some(SessionMode::Normal),
            context: None, mcp_servers: Vec::new(), host_tools: Vec::new(),
            host_tool_handler: None, disabled_tools: Vec::new(), env: Default::default(),
            executable: self.harnesses.get(&id).and_then(|harness| harness.executable.clone()),
        };
        Task::perform(async move {
            let harness = adapter.start(config, sink.clone()).await.map_err(|e| e.to_string())?;
            Ok((session_id, sink, Arc::<dyn HarnessSession>::from(harness)))
        }, Message::Started)
    }

    /// Routes new sessions to harness `id`. An active session without turns
    /// is restarted on it; one with turns stays in Recents beside a new one.
    fn switch_harness(&mut self, id: String) -> Task<Message> {
        if !self.harnesses.get(&id).is_some_and(|harness| harness.option.available) {
            self.notice = Some(format!("{} is unavailable", self.harnesses.label(&id)));
            return Task::none();
        }
        if self.starting {
            self.notice = Some("A session is already starting".into());
            self.show_harness();
            return Task::none();
        }
        self.harness = Some(id.clone());
        self.composer.set_harnesses(self.harnesses.options(), Some(&id));
        let Some(session) = &self.session else { return self.start(); };
        if session.harness.harness() == id { return Task::none(); }
        let empty = !session.busy && dom_turns(&session.dom).next().is_none();
        let close = if empty && let Some(old) = self.session.take() {
            self.shell.remove_session(&old.id);
            self.shell.set_active_session(None);
            self.shell.set_session_status(None);
            Task::perform(async move { old.harness.shutdown().await.map_err(|e| e.to_string()) }, Message::Closed)
        } else {
            Task::none()
        };
        Task::batch([close, self.start()])
    }

    /// Points the composer's harness picker and new sessions at the active
    /// session's harness.
    fn show_harness(&mut self) {
        if let Some(session) = &self.session {
            self.harness = Some(session.harness.harness().to_owned());
        }
        self.composer.set_harnesses(self.harnesses.options(), self.harness.as_deref());
    }

    /// Loads the active session's DOM into every view.
    fn present(&mut self) {
        let Some(session) = &self.session else { return; };
        let dom = &session.dom;
        self.chat.load(dom);
        self.decisions.sync(dom);
        self.agents.reset(dom);
        sync_composer(&mut self.composer, dom);
        if let Some(summary) = shell::data::SessionSummary::from_dom(dom, false) {
            self.shell.upsert_session(summary);
        }
        self.shell.set_active_session(Some(session.id.clone()));
        self.shell.set_session_status(turn_status(dom).map(str::to_owned));
    }

    fn open_session(&mut self, id: String) {
        if self.session.as_ref().is_some_and(|session| session.id == id) { return; }
        let Some(next) = self.sessions.remove(&id) else {
            self.notice = Some(format!("Session {id} is not loaded"));
            return;
        };
        if let Some(previous) = self.session.replace(next) {
            self.sessions.insert(previous.id.clone(), previous);
        }
        self.present();
        self.show_harness();
    }

    fn refresh(&mut self) -> Task<Message> {
        let Some(session) = &mut self.session else { return Task::none(); };
        let Some(dom) = session.sink.with_dom(|dom| (dom.seq() != session.dom.seq()).then(|| dom.clone())) else {
            return Task::none();
        };
        let was_busy = session.busy;
        session.busy = dom_turns(&dom).any(|turn|
            matches!(turn.str_attr(attr::STATUS).and_then(TurnStatus::parse), Some(TurnStatus::Running | TurnStatus::Waiting)));
        let busy = session.busy;
        session.dom = dom;
        self.present();
        if busy && !was_busy { self.composer.turn_started(); }
        if !busy && was_busy {
            let diff = working_diff(self.cwd.clone());
            if let Some(composer::Event::Submit(prompt)) = self.composer.turn_ended() {
                return Task::batch([diff, self.send(prompt)]);
            }
            return diff;
        }
        Task::none()
    }

    fn send(&mut self, prompt: Prompt) -> Task<Message> {
        let Some(session) = &mut self.session else { return Task::none(); };
        session.busy = true;
        self.composer.turn_started();
        let harness = session.harness.clone();
        Task::perform(async move { harness.send(prompt).await.map_err(|e| e.to_string()) }, Message::Operation)
    }

    fn interrupt(&mut self) -> Task<Message> {
        let Some(session) = &self.session else { return Task::none(); };
        if !session.busy { return Task::none(); }
        let harness = session.harness.clone();
        Task::perform(async move { harness.interrupt().await.map_err(|e| e.to_string()) }, Message::Operation)
    }

    /// Adds the panel tab `id` when it isn't there and shows it.
    fn open_tab(&mut self, id: &str) {
        if let Some((id, title)) = EXTRA_TABS.into_iter().find(|(extra, _)| *extra == id)
            && !self.shell.tabs().iter().any(|tab| tab.id == id)
        {
            self.shell.add_tab(shell::layout::PanelTab::new(id, title));
        }
        self.shell.show_tab(id);
    }

    fn command(&mut self, id: &palette::CommandId) -> Task<Message> {
        let id = id.as_str();
        if let Some(tab) = id.strip_prefix(TAB_COMMAND) {
            self.open_tab(tab);
            return Task::none();
        }
        if let Some(harness) = id.strip_prefix(HARNESS_COMMAND) {
            return self.switch_harness(harness.to_owned());
        }
        if let Some((_, _, preference)) = id.strip_prefix(THEME_COMMAND)
            .and_then(|theme| THEMES.into_iter().find(|(key, _, _)| *key == theme))
        {
            return self.update(Message::Shell(shell::layout::Message::SetTheme(preference)));
        }
        match id {
            "palette.toggle" => self.palette.update(palette::Message::Toggle).0.map(Message::Palette),
            "panel.toggle" => {
                self.shell.set_panel_open(!self.shell.panel_open());
                Task::none()
            }
            "session.new" => self.start(),
            "session.interrupt" => {
                let (task, event) = self.composer.update(composer::Message::Interrupt);
                Task::batch([task.map(Message::Composer), self.composer_event(event)])
            }
            _ => Task::none(),
        }
    }

    fn composer_event(&mut self, event: Option<composer::Event>) -> Task<Message> {
        match event {
            Some(composer::Event::Submit(prompt)) => self.send(prompt),
            Some(composer::Event::Stop) => self.interrupt(),
            Some(composer::Event::HarnessChanged(id)) => self.switch_harness(id),
            Some(composer::Event::ModelChanged(model)) => {
                let Some(session) = &self.session else { return Task::none(); };
                let harness = session.harness.clone();
                Task::perform(async move { harness.set_model(ModelSelection { model: model.id, role: model.role }).await.map_err(|e| e.to_string()) }, Message::Operation)
            }
            Some(composer::Event::ModeChanged(mode)) => {
                let Some(session) = &self.session else { return Task::none(); };
                let harness = session.harness.clone();
                Task::perform(async move { harness.set_mode(mode).await.map_err(|e| e.to_string()) }, Message::Operation)
            }
            Some(composer::Event::FilesRequested { .. }) => {
                self.files.refresh();
                self.composer.set_files(self.files.visible().map(|p| p.to_string_lossy().into_owned()).collect());
                Task::none()
            }
            Some(composer::Event::EffortChanged(_)) => {
                self.notice = Some("This harness does not expose effort changes".into());
                Task::none()
            }
            Some(composer::Event::QueueChanged(_)) | None => Task::none(),
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Shell(message) => {
                let (task, event) = self.shell.update(message);
                let action = match event {
                    Some(shell::layout::Event::NewSession) => self.start(),
                    Some(shell::layout::Event::OpenTools) => {
                        self.open_tab("tools");
                        Task::none()
                    }
                    Some(shell::layout::Event::OpenPeers) => {
                        self.open_tab("peers");
                        self.notice = Some("Peer discovery and grants require a peer service, which is not configured".into());
                        Task::none()
                    }
                    Some(shell::layout::Event::OpenSession(id)) => {
                        self.open_session(id);
                        Task::none()
                    }
                    Some(shell::layout::Event::OpenProject(project)) => {
                        self.notice = Some(format!("Project: {project}"));
                        Task::none()
                    }
                    _ => Task::none(),
                };
                Task::batch([task.map(Message::Shell), action])
            }
            Message::Chat(message) => {
                let (task, event) = self.chat.update(message);
                if let Some(event) = event { self.notice = Some(format!("{event:?}")); }
                task.map(Message::Chat)
            }
            Message::Composer(message) => {
                let (task, event) = self.composer.update(message);
                Task::batch([task.map(Message::Composer), self.composer_event(event)])
            }
            Message::Decisions(message) => {
                let (task, event) = self.decisions.update(message);
                let answer = if let Some(decisions::Event::Answer { decision, answer }) = event {
                    self.session.as_ref().map(|s| {
                        let harness = s.harness.clone();
                        Task::perform(async move { harness.answer(decision, answer).await.map_err(|e| e.to_string()) }, move |result| Message::Answered(decision, result))
                    }).unwrap_or_else(Task::none)
                } else { Task::none() };
                Task::batch([task.map(Message::Decisions), answer])
            }
            Message::Agents(message) => self.agents.update(message).0.map(Message::Agents),
            Message::Files(message) => {
                let (task, event) = self.files.update(message);
                if let Some(files::Event::Open(path)) = event { self.notice = Some(path.display().to_string()); }
                task.map(Message::Files)
            }
            Message::Diff(message) => self.diff.update(message).0.map(Message::Diff),
            Message::Tree(message) => {
                let (task, event) = self.tree.update(message);
                if let Some(event) = event { self.notice = Some(format!("{event:?}")); }
                task.map(Message::Tree)
            }
            Message::Terminal(message) => self.terminal.update(message).0.map(Message::Terminal),
            Message::Palette(message) => {
                let (task, event) = self.palette.update(message);
                let action = match event { Some(palette::Event::Run(id)) => self.command(&id), _ => Task::none() };
                Task::batch([task.map(Message::Palette), action])
            }
            Message::Tools(message) => self.tools.update(message).0.map(Message::Tools),
            Message::Peers(message) => {
                let (task, event) = self.peers.update(message);
                if event.is_some() {
                    self.notice = Some("Peer actions require a configured peer service".into());
                }
                task.map(Message::Peers)
            }
            Message::Surface(message) => self.surface.as_mut()
                .map(|surface| surface.update(message).0.map(Message::Surface))
                .unwrap_or_else(Task::none),
            Message::Keyboard(event) => {
                if let keyboard::Event::KeyPressed { key: keyboard::Key::Named(key::Named::Escape), modifiers, .. } = &event
                    && !modifiers.control() && !modifiers.alt() && !modifiers.logo()
                {
                    if self.palette.is_open() {
                        return self.palette.update(palette::Message::Close).0.map(Message::Palette);
                    }
                    if self.decisions.focused().is_some() {
                        return self.update(Message::Decisions(decisions::Message::Shortcut(decisions::Shortcut::Dismiss)));
                    }
                    // Interrupt the harness itself, even before the turn-start patch arrives.
                    return self.interrupt();
                }
                if let Some(id) = self.keymap.resolve(&event, palette::Scope::Global).cloned() {
                    return self.command(&id);
                }
                Task::none()
            }
            Message::Poll => self.refresh(),
            Message::NewSession => self.start(),
            Message::Started(result) => {
                self.starting = false;
                match result {
                    Ok((id, sink, harness)) => {
                        if let Some(previous) = self.session.replace(Session { id, sink, dom: Dom::new(), harness, busy: false }) {
                            self.sessions.insert(previous.id.clone(), previous);
                        }
                        self.show_harness();
                        self.files.refresh();
                        Task::batch([self.refresh(), working_diff(self.cwd.clone())])
                    }
                    Err(error) => {
                        self.notice = Some(error);
                        self.show_harness();
                        Task::none()
                    }
                }
            }
            Message::Operation(result) => {
                if let Err(error) = result {
                    if let Some(session) = &mut self.session { session.busy = false; }
                    self.notice = Some(error);
                }
                Task::none()
            }
            Message::Answered(decision, result) => {
                if let Err(error) = result {
                    self.decisions.answer_failed(decision);
                    self.notice = Some(error);
                }
                Task::none()
            }
            Message::Closed(result) => {
                if let Err(error) = result { self.notice = Some(error); }
                Task::none()
            }
            Message::WorkingDiff(Some(patch)) => match diff::parse_unified(&patch) {
                Ok(files) => self.diff.set_files(files).map(Message::Diff),
                Err(error) => {
                    self.notice = Some(format!("Can't read the working tree diff: {error}"));
                    Task::none()
                }
            },
            Message::WorkingDiff(None) => Task::none(),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let center_content: Element<'_, Message> = if let Some(session) = &self.session {
            let transcript = self.chat.view(&session.dom, Message::Chat, |slot| {
                self.decisions.view_card(slot.node)
                    .map(|card| card.map(Message::Decisions))
                    .unwrap_or_else(|| text("Decision no longer available").into())
            });
            column![
                container(transcript).height(Fill),
                container(container(self.composer.view().map(Message::Composer)).width(Fill).max_width(COMPOSER_WIDTH))
                    .center_x(Fill)
                    .padding(Padding { top: 8.0, right: 24.0, bottom: 16.0, left: 24.0 }),
            ].into()
        } else {
            center(column![
                text(if self.starting { "Starting harness…" } else { "Start a session" }).size(20),
                button(text("New session").size(14)).style(button::secondary)
                    .on_press_maybe((!self.starting).then_some(Message::NewSession)),
            ].spacing(16).align_x(iced::Alignment::Center)).into()
        };
        let center_content: Element<'_, Message> = if let Some(notice) = &self.notice {
            column![center_content, container(text(notice.as_str()).size(12).style(style::muted)).padding([6, 24])].into()
        } else { center_content };
        let frame = self.shell.view(center_content, |tab| match tab {
            "agents" => self.agents.view().map(Message::Agents),
            "files" => self.files.view().map(Message::Files),
            "diff" => self.diff.view().map(Message::Diff),
            "terminal" => self.terminal.view().map(Message::Terminal),
            "session" => self.tree.view().map(Message::Tree),
            "tools" => self.tools.view().map(Message::Tools),
            "peers" => self.peers.view().map(Message::Peers),
            "surface" => self.surface.as_ref().map(|surface| surface.view().map(Message::Surface))
                .unwrap_or_else(|| container(text("No surface is available from the current harness").size(13).style(style::muted)).padding(16).into()),
            _ => text("Unknown tab").into(),
        }, Message::Shell);
        if self.palette.is_open() {
            stack![frame, self.palette.view().map(Message::Palette)].into()
        } else { frame }
    }

    fn theme(&self) -> Theme { self.shell.theme() }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            self.shell.subscription().map(Message::Shell),
            self.composer.subscription().map(Message::Composer),
            iced::keyboard::listen().map(Message::Keyboard),
            iced::time::every(Duration::from_millis(50)).map(|_| Message::Poll),
        ])
    }
}

/// The palette's commands: panel tabs, harnesses that can run, themes and the
/// session and view actions.
fn commands(harnesses: &Harnesses, keymap: &palette::Keymap) -> Vec<palette::Command> {
    let mut commands = vec![
        palette::Command::new(NEW_SESSION, "New session", "Commands"),
        palette::Command::new(palette::ids::PANEL_TOGGLE, "Toggle right panel", "Commands")
            .shortcut(keymap.chord_for(&palette::ids::PANEL_TOGGLE)),
        palette::Command::new(palette::ids::SESSION_INTERRUPT, "Interrupt session", "Commands"),
        palette::Command::new(palette::ids::PALETTE_TOGGLE, "Command palette", "Commands")
            .shortcut(keymap.chord_for(&palette::ids::PALETTE_TOGGLE)),
    ];
    commands.extend(TABS.into_iter().chain(EXTRA_TABS).map(|(id, title)| {
        palette::Command::new(palette::CommandId::new(format!("{TAB_COMMAND}{id}")), format!("Open {title} tab"), "Commands")
    }));
    commands.extend(harnesses.list.iter().filter(|harness| harness.option.available).map(|harness| {
        let id = palette::CommandId::new(format!("{HARNESS_COMMAND}{}", harness.option.id));
        palette::Command::new(id, format!("Switch harness: {}", harness.option.label), "Harness")
    }));
    commands.extend(THEMES.into_iter().map(|(key, label, _)| {
        palette::Command::new(palette::CommandId::new(format!("{THEME_COMMAND}{key}")), format!("Theme: {label}"), "Theme")
    }));
    commands
}

/// The `<turn>` children of the session root, oldest first.
fn dom_turns(dom: &Dom) -> impl Iterator<Item = &loa_core::Node> {
    dom.root().into_iter().flat_map(|root| dom.children_of_kind(root.id, ElementKind::Turn))
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

/// Shows the session's model, effort and mode in the composer pickers.
fn sync_composer(composer: &mut composer::State, dom: &Dom) {
    let Some(root) = dom.root() else { return; };
    let current = models::session::SessionModel::from_dom(dom).map(|(_, model)| model).unwrap_or_default();
    let options = current.model.iter().map(|id| {
        let (provider, name) = id.rsplit_once('/').unwrap_or(("", id));
        composer::ModelOption { id: id.clone(), name: name.to_owned(), provider: provider.to_owned(), role: current.role.clone() }
    }).collect();
    composer.set_models(options, current.model.as_deref());
    let effort = root.str_attr(attr::EFFORT).map(str::to_owned);
    composer.set_efforts(effort.iter().cloned().collect(), effort);
    composer.set_mode(root.str_attr(attr::MODE).and_then(SessionMode::parse).unwrap_or(SessionMode::Normal));
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

#[cfg(test)]
mod tests {
    use super::*;
    use loa_adapter::{Adapter, Answer, Capabilities, Registry, SessionState};
    use loa_core::{Draft, PatchSink, Value};

    struct FakeAdapter(&'static str);

    #[async_trait::async_trait]
    impl Adapter for FakeAdapter {
        fn harness(&self) -> &'static str { self.0 }
        fn capabilities(&self) -> Capabilities { Capabilities::default() }
        async fn start(&self, _: SessionConfig, _: Arc<dyn loa_core::PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(FakeSession(self.0)))
        }
        async fn resume(&self, _: SessionConfig, _: &Dom, _: Arc<dyn loa_core::PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
            Ok(Box::new(FakeSession(self.0)))
        }
    }

    struct FakeSession(&'static str);

    #[async_trait::async_trait]
    impl HarnessSession for FakeSession {
        async fn send(&self, _: Prompt) -> loa_adapter::Result<()> { Ok(()) }
        async fn interrupt(&self) -> loa_adapter::Result<()> { Ok(()) }
        async fn answer(&self, _: NodeId, _: Answer) -> loa_adapter::Result<()> { Ok(()) }
        fn harness(&self) -> &'static str { self.0 }
        fn state(&self) -> SessionState { SessionState::Idle }
        async fn wait_idle(&self) -> loa_adapter::Result<()> { Ok(()) }
        async fn shutdown(&self) -> loa_adapter::Result<()> { Ok(()) }
    }

    fn fake_app(ids: &[&'static str]) -> App {
        let mut harnesses = Harnesses { registry: Registry::new(), list: Vec::new() };
        for id in ids {
            harnesses.add(Arc::new(FakeAdapter(id)), id, true, None, None);
        }
        App::build(harnesses, std::env::temp_dir().join("loa-app-test"), "testhost".into()).0
    }

    /// Delivers a started session on `harness` the way `start` would, with one
    /// finished turn when `turns` is set.
    fn deliver(app: &mut App, id: &str, harness: &'static str, turns: bool) {
        let sink = Arc::new(MemorySink::new());
        let attrs: [(&str, Value); 4] =
            [("id", id.into()), ("node", NODE.into()), ("harness", harness.into()), ("created", Value::Int(1))];
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "")).unwrap().node;
        if turns {
            let turn = sink.commit(Draft::insert(root, ElementKind::Turn, [("status", Value::from("done"))], "")).unwrap().node;
            sink.commit(Draft::insert(turn, ElementKind::Message, [("role", Value::from("user"))], "hello")).unwrap();
        }
        let _ = app.update(Message::Started(Ok((id.to_owned(), sink, Arc::new(FakeSession(harness))))));
    }

    fn pick(app: &mut App, harness: &str) {
        let _ = app.update(Message::Composer(composer::Message::HarnessPicked(harness.to_owned())));
    }

    #[test]
    fn panel_command_toggles_without_starting_a_harness() {
        let (mut app, _) = App::new();
        let initial = app.shell.panel_open();
        let _ = app.command(&palette::ids::PANEL_TOGGLE);
        assert_ne!(app.shell.panel_open(), initial);
        let _ = app.command(&palette::ids::PANEL_TOGGLE);
        assert_eq!(app.shell.panel_open(), initial);
    }

    #[test]
    fn escape_closes_palette_before_attempting_interrupt() {
        let (mut app, _) = App::new();
        let _ = app.palette.update(palette::Message::Toggle);
        assert!(app.palette.is_open());
        let _ = app.update(Message::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(key::Named::Escape),
            modified_key: keyboard::Key::Named(key::Named::Escape),
            physical_key: key::Physical::Code(key::Code::Escape),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::default(),
            text: None,
            repeat: false,
        }));
        assert!(!app.palette.is_open());
    }

    #[test]
    fn switching_harness_restarts_an_empty_session() {
        let mut app = fake_app(&["fake-a", "fake-b"]);
        deliver(&mut app, "s1", "fake-a", false);
        assert_eq!(app.composer.harness().map(|h| h.id.as_str()), Some("fake-a"));

        pick(&mut app, "fake-b");
        assert!(app.session.is_none(), "the empty session is closed");
        assert!(app.sessions.is_empty());
        assert!(app.shell.sidebar().session("s1").is_none());
        assert!(app.starting);
        assert_eq!(app.harness.as_deref(), Some("fake-b"));

        deliver(&mut app, "s2", "fake-b", false);
        assert_eq!(app.session.as_ref().map(|s| s.id.as_str()), Some("s2"));
        assert_eq!(app.composer.harness().map(|h| h.id.as_str()), Some("fake-b"));
    }

    #[test]
    fn switching_harness_keeps_a_session_with_turns_in_recents() {
        let mut app = fake_app(&["fake-a", "fake-b"]);
        deliver(&mut app, "s1", "fake-a", true);

        pick(&mut app, "fake-b");
        assert_eq!(app.session.as_ref().map(|s| s.id.as_str()), Some("s1"), "kept until the new one starts");
        assert!(app.starting);

        deliver(&mut app, "s2", "fake-b", false);
        assert_eq!(app.session.as_ref().map(|s| s.id.as_str()), Some("s2"));
        assert!(app.sessions.contains_key("s1"));
        assert!(app.shell.sidebar().session("s1").is_some());

        app.open_session("s1".into());
        assert_eq!(app.composer.harness().map(|h| h.id.as_str()), Some("fake-a"));
        assert_eq!(app.harness.as_deref(), Some("fake-a"));
    }

    #[test]
    fn palette_lists_available_harnesses_tabs_and_themes() {
        let app = fake_app(&["fake-a", "fake-b"]);
        let titles: Vec<&str> = app.palette.commands().iter().map(|c| c.title.as_str()).collect();
        for title in ["Switch harness: fake-a", "Switch harness: fake-b", "Open Terminal tab", "Open Agents tab", "Theme: Dark", "Theme: Light", "Theme: System"] {
            assert!(titles.contains(&title), "{title} missing from {titles:?}");
        }
    }

    #[test]
    fn palette_tab_command_adds_and_shows_the_tab() {
        let mut app = fake_app(&["fake-a"]);
        assert!(!app.shell.tabs().iter().any(|tab| tab.id == "agents"));
        let _ = app.command(&palette::CommandId::new("tab.agents"));
        assert_eq!(app.shell.active_tab(), Some("agents"));
        assert!(app.shell.panel_open());
    }
}
