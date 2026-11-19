//! Desktop application wiring: the components own presentation state while the
//! adapter owns execution and the session DOM remains the source of truth.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iced::keyboard::{self, key};
use iced::widget::{button, center, column, container, stack, text};
use iced::{Element, Fill, Font, Subscription, Task, Theme};
use loa_adapter::{HarnessSession, ModelSelection, Prompt, Registry, SessionConfig};
use loa_core::{Dom, MemorySink, NodeId, SessionMode};
use loa_ui_chat as chat;
use loa_ui_composer as composer;
use loa_ui_decisions as decisions;
use loa_ui_models as models;
use loa_ui_palette as palette;
use loa_ui_panel::{agents, diff, files, session_tree};
use loa_ui_peers as peers;
use loa_ui_surface as surface;
use loa_ui_shell as shell;
use loa_ui_terminal as terminal;
use loa_ui_tools as tools;

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("Loa")
        .theme(App::theme)
        .subscription(App::subscription)
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
    Model(models::chip::Message),
    Keyboard(keyboard::Event),
    Poll,
    NewSession,
    Started(StartedSession),
    Operation(Result<(), String>),
    Answered(NodeId, Result<(), String>),
}

struct App {
    registry: Registry,
    default_harness: &'static str,
    cwd: PathBuf,
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
    model: models::chip::State,
    peers: peers::State,
    surface: Option<surface::State>,
    session: Option<Session>,
    sessions: HashMap<String, Session>,
    starting: bool,
    notice: Option<String>,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut registry = Registry::new();
        registry.register(Arc::new(loa_adapter_codex::CodexAdapter::new("local")));
        registry.register(Arc::new(loa_adapter_acp::AcpAdapter::new("opencode", std::iter::empty::<String>())));
        let (mut shell, setup) = shell::layout::State::new(
            shell::theme::Preference::System,
            shell::layout::PanelPrefs::default(),
        );
        shell.set_tabs(["agents", "files", "terminal", "session", "diff", "tools", "peers", "surface"]
            .into_iter().map(|id| shell::layout::PanelTab::new(id, match id {
                "session" => "Session", "agents" => "Agents", "files" => "Files",
                "terminal" => "Terminal", "diff" => "Diff", "peers" => "Peers",
                "surface" => "Surface", _ => "Tools",
            })).collect());
        let theme = shell.theme();
        let mut palette = palette::State::new(palette::Platform::current());
        let keymap = palette::Keymap::defaults(palette::Platform::current());
        palette.set_commands(vec![
            palette::Command::new(palette::ids::PALETTE_TOGGLE, "Command palette", "App"),
            palette::Command::new(palette::ids::PANEL_TOGGLE, "Toggle right panel", "View")
                .shortcut(keymap.chord_for(&palette::ids::PANEL_TOGGLE)),
            palette::Command::new(palette::ids::SESSION_INTERRUPT, "Interrupt session", "Session"),
        ]);
        let metrics = terminal::canvas::Metrics::measure(Font::MONOSPACE, 13.0);
        let terminal = terminal::state::State::new(metrics, terminal::local::LocalShell::login_shell(cwd.clone()));
        let dom = Dom::new();
        let app = Self {
            registry, default_harness: "codex", cwd: cwd.clone(), shell,
            chat: chat::State::new(&theme), composer: composer::State::new(),
            decisions: decisions::State::new(decisions::Viewer { by: "local".into(), view_only: false }, &theme),
            agents: agents::State::from_dom(&dom), files: files::State::new(cwd, Box::new(files::LocalFs)),
            diff: diff::State::new(), tree: session_tree::State::new(), terminal, palette, keymap,
            tools: tools::State::new(), model: models::chip::State::from_dom(&dom),
            session: None, sessions: HashMap::new(), peers: peers::State::default(),
            surface: None, starting: false, notice: None,
        };
        (app, setup.map(Message::Shell))
    }

    fn start(&mut self) -> Task<Message> {
        if self.starting { return Task::none(); }
        let Some(adapter) = self.registry.get(self.default_harness) else {
            self.notice = Some(format!("Harness {} is unavailable", self.default_harness));
            return Task::none();
        };
        self.starting = true;
        self.notice = None;
        let id = format!("loa-{}-{}", std::process::id(), loa_core::now_ms());
        let sink = Arc::new(MemorySink::new());
        let config = SessionConfig {
            session_id: id.clone(), node: "local".into(), cwd: self.cwd.clone(),
            project: Some(self.cwd.to_string_lossy().into_owned()),
            model: None, model_role: None, effort: None, mode: Some(SessionMode::Normal),
            context: None, mcp_servers: Vec::new(), host_tools: Vec::new(),
            host_tool_handler: None, disabled_tools: Vec::new(), env: Default::default(), executable: None,
        };
        Task::perform(async move {
            let harness = adapter.start(config, sink.clone()).await.map_err(|e| e.to_string())?;
            Ok((id, sink, Arc::<dyn HarnessSession>::from(harness)))
        }, Message::Started)
    }

    fn refresh(&mut self) -> Task<Message> {
        let Some(session) = &mut self.session else { return Task::none(); };
        let Some(dom) = session.sink.with_dom(|dom| (dom.seq() != session.dom.seq()).then(|| dom.clone())) else {
            return Task::none();
        };
        let was_busy = session.busy;
        session.busy = dom.iter().any(|node| node.kind == loa_core::ElementKind::Turn &&
            matches!(node.str_attr(loa_core::attr::STATUS), Some("running" | "waiting")));
        self.chat.load(&dom);
        self.decisions.sync(&dom);
        self.agents.reset(&dom);
        self.model = models::chip::State::from_dom(&dom);
        if let Some(summary) = shell::data::SessionSummary::from_dom(&dom, false) {
            self.shell.upsert_session(summary);
            self.shell.set_active_session(Some(session.id.clone()));
        }
        session.dom = dom;
        if session.busy && !was_busy { self.composer.turn_started(); }
        if !session.busy && was_busy
            && let Some(composer::Event::Submit(prompt)) = self.composer.turn_ended()
        {
            return self.send(prompt);
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

    fn command(&mut self, id: &palette::CommandId) -> Task<Message> {
        match id.as_str() {
            "palette.toggle" => self.palette.update(palette::Message::Toggle).0.map(Message::Palette),
            "panel.toggle" => {
                self.shell.set_panel_open(!self.shell.panel_open());
                Task::none()
            }
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
                if let Some(event) = event {
                    match event {
                        shell::layout::Event::NewSession => return Task::batch([task.map(Message::Shell), self.start()]),
                        shell::layout::Event::OpenTools => self.shell.show_tab("tools"),
                        shell::layout::Event::OpenPeers => {
                            self.shell.show_tab("peers");
                            self.notice = Some("Peer discovery and grants require a peer service, which is not configured".into());
                        }
                        shell::layout::Event::OpenSession(id) if self.session.as_ref().is_none_or(|s| s.id != id) => {
                            if let Some(next) = self.sessions.remove(&id) {
                                if let Some(previous) = self.session.replace(next) {
                                    self.sessions.insert(previous.id.clone(), previous);
                                }
                                self.chat.load(&self.session.as_ref().unwrap().dom);
                                self.decisions.sync(&self.session.as_ref().unwrap().dom);
                                self.agents.reset(&self.session.as_ref().unwrap().dom);
                                self.model = models::chip::State::from_dom(&self.session.as_ref().unwrap().dom);
                                self.shell.set_active_session(Some(id));
                            } else {
                                self.notice = Some(format!("Session {id} is not loaded"));
                            }
                        }
                        shell::layout::Event::OpenProject(project) => self.notice = Some(format!("Project: {project}")),
                        _ => {}
                    }
                }
                task.map(Message::Shell)
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
            Message::Model(message) => {
                if let Some(models::chip::Event::OpenPicker) = self.model.update(message).1 {
                    self.notice = Some("Use the composer model selector to switch models".into());
                }
                Task::none()
            }
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
                        self.files.refresh();
                        self.refresh()
                    }
                    Err(error) => { self.notice = Some(error); Task::none() }
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
                self.model.view().map(Message::Model),
                self.composer.view().map(Message::Composer),
            ].into()
        } else {
            center(column![
                text(if self.starting { "Starting harness…" } else { "Start a session" }).size(22),
                button("New session").on_press_maybe((!self.starting).then_some(Message::NewSession)),
            ].spacing(16)).into()
        };
        let center_content: Element<'_, Message> = if let Some(notice) = &self.notice {
            column![center_content, container(text(notice.as_str())).padding(8)].into()
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
                .unwrap_or_else(|| text("No surface is available from the current harness").into()),
            _ => text("Unknown tab").into(),
        }, Message::Shell);
        if self.palette.is_open() {
            stack![frame, container(self.palette.view().map(Message::Palette)).center(Fill)].into()
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
