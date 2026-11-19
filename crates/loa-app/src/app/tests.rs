use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use iced::futures::StreamExt;
use iced::widget::text_editor::{Action, Edit};
use iced_runtime::Action as RuntimeAction;
use loa_adapter::{Adapter, Answer, Capabilities, Registry, SessionState};
use loa_bus::Policy;
use loa_core::{DecisionKind, DecisionStatus, Draft, NodeId, PatchSink, Role, Value, now_ms};
use loa_identity::NodeIdentity;
use loa_ui_decisions as decisions;

use super::*;
use crate::services::{Dirs, Harnesses, Node};

/// A harness that answers every prompt at once. A prompt starting with
/// "ask" waits on a decision instead, until it is answered: "ask question",
/// "ask multi", "ask plan" and "ask form" raise those kinds, any other "ask"
/// an approval.
pub(crate) struct FakeAdapter {
    id: &'static str,
    resumes: bool,
    resumed: Arc<AtomicUsize>,
}

impl FakeAdapter {
    pub(crate) fn new(id: &'static str) -> Self {
        Self { id, resumes: true, resumed: Arc::default() }
    }
}

#[async_trait::async_trait]
impl Adapter for FakeAdapter {
    fn harness(&self) -> &'static str {
        self.id
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        let attrs: [(&str, Value); 5] = [
            ("id", config.session_id.into()),
            ("node", config.node.into()),
            ("harness", self.id.into()),
            ("created", Value::Int(now_ms())),
            ("native_id", "native-1".into()),
        ];
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?.node;
        Ok(Box::new(FakeSession::new(self.id, sink, root)))
    }

    async fn resume(&self, _: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        if !self.resumes {
            return Err(AdapterError::Unsupported { harness: self.id, what: "resume" });
        }
        self.resumed.fetch_add(1, Ordering::SeqCst);
        let root = dom.root().map(|root| root.id).ok_or_else(|| AdapterError::Format("empty DOM".into()))?;
        Ok(Box::new(FakeSession::new(self.id, sink, root)))
    }
}

struct FakeSession {
    id: &'static str,
    sink: Arc<dyn PatchSink>,
    root: NodeId,
    /// The turn waiting on a decision.
    waiting: Mutex<Option<NodeId>>,
}

impl FakeSession {
    fn new(id: &'static str, sink: Arc<dyn PatchSink>, root: NodeId) -> Self {
        Self { id, sink, root, waiting: Mutex::new(None) }
    }

    fn end(&self, turn: NodeId) -> loa_adapter::Result<()> {
        let reply: [(&str, Value); 2] = [("role", Role::Assistant.into()), ("status", "done".into())];
        self.sink.commit(Draft::insert(turn, ElementKind::Message, reply, "ok"))?;
        self.sink.commit(Draft::set(turn, "status", TurnStatus::Done))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl HarnessSession for FakeSession {
    async fn send(&self, prompt: Prompt) -> loa_adapter::Result<()> {
        let turn = self.sink.commit(Draft::insert(self.root, ElementKind::Turn, [("status", Value::from(TurnStatus::Running))], ""))?.node;
        self.sink.commit(Draft::insert(turn, ElementKind::Message, [("role", Value::from(Role::User))], prompt.text.as_str()))?;
        if let Some(ask) = prompt.text.strip_prefix("ask") {
            let options = |labels: &[&str]| ("options", Value::List(labels.iter().map(|label| Value::from(*label)).collect()));
            let (kind, title, body, mut attrs): (DecisionKind, &str, &str, Vec<(&str, Value)>) = match ask.split_whitespace().next() {
                Some("question") => (DecisionKind::Question, "Color", "Which color?", vec![options(&["red", "green"])]),
                Some("multi") => {
                    (DecisionKind::Question, "Languages", "Which languages?", vec![options(&["rust", "go", "zig"]), ("multi", true.into())])
                }
                Some("plan") => (DecisionKind::Plan, "Plan", "# Plan\n\n1. Read the code\n2. Fix the bug", Vec::new()),
                Some("form") => (DecisionKind::Form, "Release", "Release settings", vec![("fields", FAKE_FORM.into())]),
                _ => (DecisionKind::Approval, "Run ls", "ls", Vec::new()),
            };
            attrs.extend([("kind", kind.into()), ("status", DecisionStatus::Waiting.into()), ("title", title.into())]);
            self.sink.commit(Draft::insert(turn, ElementKind::Decision, attrs, body))?;
            self.sink.commit(Draft::set(turn, "status", TurnStatus::Waiting))?;
            *self.waiting.lock().unwrap() = Some(turn);
            return Ok(());
        }
        self.end(turn)
    }

    async fn interrupt(&self) -> loa_adapter::Result<()> {
        Ok(())
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> loa_adapter::Result<()> {
        let values = Value::List(answer.values.into_iter().map(Value::from).collect());
        self.sink.commit(Draft::set(decision, "answer", values))?;
        self.sink.commit(Draft::set(decision, "by", answer.by))?;
        if let Some(reason) = answer.reason {
            self.sink.commit(Draft::set(decision, "reason", reason))?;
        }
        self.sink.commit(Draft::set(decision, "status", DecisionStatus::Answered))?;
        let turn = self.waiting.lock().unwrap().take().ok_or(AdapterError::UnknownDecision(decision))?;
        self.sink.commit(Draft::set(turn, "status", TurnStatus::Running))?;
        self.end(turn)
    }

    fn harness(&self) -> &'static str {
        self.id
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

/// The fields of the fake harness's "ask form" decision.
pub(crate) const FAKE_FORM: &str = r#"{"type":"object","properties":{
    "name":{"type":"string","description":"Release name"},
    "channel":{"enum":["beta","stable"]}
},"required":["name","channel"]}"#;

pub(crate) fn services(dir: &Path, adapters: Vec<FakeAdapter>) -> Services {
    let mut harnesses = Harnesses { registry: Registry::new(), list: Vec::new() };
    for adapter in adapters {
        let id = adapter.id;
        harnesses.add(Arc::new(adapter), id, true, None, None);
    }
    let node = Node::new(NodeIdentity::from_secret_bytes(&[7; 32]), "testhost".into());
    Services::new(node, Dirs::new(dir.join("loa")), harnesses, dir.to_owned()).unwrap()
}

pub(crate) fn fake_app(dir: &Path, ids: &[&'static str]) -> App {
    App::build(services(dir, ids.iter().map(|id| FakeAdapter::new(id)).collect())).0
}

/// Runs `task` the way the iced runtime would, feeding every message it
/// produces back through `App::update`, until no task is left.
pub(crate) fn run(app: &mut App, task: Task<Message>) {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut pending = vec![task];
    while let Some(task) = pending.pop() {
        let Some(stream) = iced_runtime::task::into_stream(task) else { continue };
        let outputs = runtime.block_on(async {
            let outputs = stream.filter_map(|action| async move {
                match action {
                    RuntimeAction::Output(message) => Some(message),
                    _ => None,
                }
            });
            tokio::time::timeout(Duration::from_secs(10), outputs.collect::<Vec<_>>()).await.expect("task finished")
        });
        for message in outputs {
            pending.push(app.update(message));
        }
    }
}

pub(crate) fn update(app: &mut App, message: Message) {
    let task = app.update(message);
    run(app, task);
}

/// Delivers every patch `subscription` holds as the session's feed would.
pub(crate) fn deliver(app: &mut App, id: &SessionId, subscription: &mut loa_bus::Subscription) {
    while let Ok(event) = subscription.try_recv() {
        update(app, Message::Session(id.clone(), Feed::Event(event)));
    }
}

/// Catches every open session's UI copy up with its writer, as the feed
/// does when it (re)subscribes.
fn sync(app: &mut App) {
    let open: Vec<_> = app.ctx.sessions.iter().map(|session| (session.id().clone(), Arc::clone(session.writer()))).collect();
    for (id, writer) in open {
        let (dom, _) = writer.subscribe(Policy::every_patch(1));
        update(app, Message::Session(id, Feed::Synced(Arc::new(dom))));
    }
}

/// Types `text` into the composer and submits it.
pub(crate) fn prompt(app: &mut App, text: &str) {
    for c in text.chars() {
        update(app, Message::Composer(composer::Message::Edit(Action::Edit(Edit::Insert(c)))));
    }
    update(app, Message::Composer(composer::Message::Submit));
}

pub(crate) fn active(app: &App) -> Option<&str> {
    app.ctx.sessions.active_id().map(SessionId::as_str)
}

fn pick(app: &mut App, harness: &str) -> Task<Message> {
    app.update(Message::Composer(composer::Message::HarnessPicked(harness.to_owned())))
}

pub(crate) fn escape() -> keyboard::Event {
    keyboard::Event::KeyPressed {
        key: keyboard::Key::Named(key::Named::Escape),
        modified_key: keyboard::Key::Named(key::Named::Escape),
        physical_key: key::Physical::Code(key::Code::Escape),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::default(),
        text: None,
        repeat: false,
    }
}

#[test]
fn panel_command_toggles_without_starting_a_harness() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let initial = app.ctx.shell.panel_open();
    let _ = app.command(&palette::ids::PANEL_TOGGLE);
    assert_ne!(app.ctx.shell.panel_open(), initial);
    let _ = app.command(&palette::ids::PANEL_TOGGLE);
    assert_eq!(app.ctx.shell.panel_open(), initial);
    assert!(app.ctx.sessions.starting().is_none());
}

#[test]
fn escape_closes_palette_before_attempting_interrupt() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let _ = app.update(Message::Palette(palette::Message::Toggle));
    assert!(app.palette.is_open());
    let _ = app.update(Message::Keyboard(escape()));
    assert!(!app.palette.is_open());
}

#[test]
fn switching_harness_restarts_an_empty_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a", "fake-b"]);
    update(&mut app, Message::NewSession);
    let s1 = active(&app).expect("started").to_owned();
    assert_eq!(app.ctx.composer.harness().map(|h| h.id.as_str()), Some("fake-a"));

    let task = pick(&mut app, "fake-b");
    assert!(active(&app).is_none(), "the empty session is closed");
    assert!(app.ctx.sessions.get(&s1).is_none());
    assert!(app.ctx.shell.sidebar().session(&s1).is_none());
    assert!(!dir.path().join("loa/sessions").join(&s1).exists(), "its journal is deleted");
    assert!(app.ctx.sessions.starting().is_some());
    assert_eq!(app.ctx.sessions.harness.as_deref(), Some("fake-b"));

    run(&mut app, task);
    let s2 = active(&app).expect("restarted");
    assert_ne!(s2, s1);
    assert_eq!(app.ctx.sessions.active().and_then(|s| s.harness_id()), Some("fake-b"));
    assert_eq!(app.ctx.composer.harness().map(|h| h.id.as_str()), Some("fake-b"));
}

#[test]
fn switching_harness_keeps_a_session_with_turns_in_recents() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a", "fake-b"]);
    update(&mut app, Message::NewSession);
    prompt(&mut app, "hello");
    sync(&mut app);
    let s1 = active(&app).unwrap().to_owned();

    let task = pick(&mut app, "fake-b");
    assert_eq!(active(&app), Some(s1.as_str()), "kept until the new one starts");
    assert!(app.ctx.sessions.starting().is_some());

    run(&mut app, task);
    let s2 = active(&app).unwrap().to_owned();
    assert_ne!(s2, s1);
    assert!(app.ctx.sessions.get(&s1).is_some());
    assert!(app.ctx.shell.sidebar().session(&s1).is_some());

    update(&mut app, Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::OpenSession(s1.clone()))));
    assert_eq!(active(&app), Some(s1.as_str()));
    assert_eq!(app.ctx.composer.harness().map(|h| h.id.as_str()), Some("fake-a"));
    assert_eq!(app.ctx.sessions.harness.as_deref(), Some("fake-a"));
}

#[test]
fn palette_lists_available_harnesses_tabs_and_themes() {
    let dir = tempfile::tempdir().unwrap();
    let app = fake_app(dir.path(), &["fake-a", "fake-b"]);
    let titles: Vec<&str> = app.palette.commands().iter().map(|c| c.title.as_str()).collect();
    for title in [
        "Switch harness: fake-a",
        "Switch harness: fake-b",
        "Open Terminal tab",
        "Open Tools tab",
        "Open Agents tab",
        "Theme: Dark",
        "Theme: Light",
        "Theme: System",
    ] {
        assert!(titles.contains(&title), "{title} missing from {titles:?}");
    }
}

#[test]
fn palette_tab_command_adds_and_shows_the_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let tabs: Vec<&str> = app.ctx.shell.tabs().iter().map(|tab| tab.id.as_str()).collect();
    assert_eq!(tabs, ["diff", "files", "terminal", "tools"]);
    let _ = app.command(&palette::CommandId::new("tab.agents"));
    assert_eq!(app.ctx.shell.active_tab(), Some("agents"));
    assert!(app.ctx.shell.panel_open());
}

#[test]
fn sidebar_tools_button_opens_the_tools_area_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let _ = app.command(&palette::CommandId::new("tab.diff"));
    update(&mut app, Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::OpenTools)));
    assert_eq!(app.ctx.shell.active_tab(), Some("tools"));
}

#[test]
fn sessions_persist_across_an_app_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (id, dom) = {
        let mut app = fake_app(dir.path(), &["fake-a"]);
        update(&mut app, Message::NewSession);
        prompt(&mut app, "hello journal");
        let session = app.ctx.sessions.active().unwrap();
        (session.id().to_string(), session.writer().with_dom(Dom::clone))
    };
    assert_eq!(dom_turns(&dom).count(), 1);

    let adapter = FakeAdapter::new("fake-a");
    let resumed = Arc::clone(&adapter.resumed);
    let mut app = App::build(services(dir.path(), vec![adapter])).0;
    let summary = app.ctx.shell.sidebar().session(&id).expect("listed in Recents");
    assert_eq!(summary.title, "hello journal");
    assert!(active(&app).is_none());

    update(&mut app, Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::OpenSession(id.clone()))));
    assert_eq!(active(&app), Some(id.as_str()));
    assert_eq!(app.ctx.sessions.active().unwrap().dom(), &dom, "the journal replays the same DOM");

    prompt(&mut app, "and again");
    assert_eq!(resumed.load(Ordering::SeqCst), 1, "resumed on its harness");
    let session = app.ctx.sessions.active().unwrap();
    assert_eq!(session.id().as_str(), id);
    assert_eq!(session.writer().with_dom(|dom| dom_turns(dom).count()), 2);
}

#[test]
fn a_harness_without_resume_continues_in_a_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut app = fake_app(dir.path(), &["fake-a"]);
        update(&mut app, Message::NewSession);
        prompt(&mut app, "first");
        active(&app).unwrap().to_owned()
    };
    let mut adapter = FakeAdapter::new("fake-a");
    adapter.resumes = false;
    let mut app = App::build(services(dir.path(), vec![adapter])).0;
    update(&mut app, Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::OpenSession(id.clone()))));
    prompt(&mut app, "second");
    let session = app.ctx.sessions.active().unwrap();
    assert_ne!(session.id().as_str(), id);
    assert_eq!(session.harness_id(), Some("fake-a"));
    assert_eq!(session.writer().with_dom(|dom| dom_turns(dom).count()), 1);
}

#[test]
fn session_events_reach_the_areas() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    update(&mut app, Message::NewSession);
    let session = app.ctx.sessions.active().unwrap();
    let id = session.id().clone();
    let (_, mut feed) = session.writer().subscribe(Policy::every_patch(1024));

    prompt(&mut app, "ask first");
    deliver(&mut app, &id, &mut feed);
    let decision = app.areas.decisions.focused().expect("the decisions area shows the waiting card");
    assert!(app.ctx.composer.is_busy(), "turn started");
    assert_eq!(app.ctx.shell.sidebar().session(id.as_str()).map(|s| s.waiting), Some(true));
    assert_eq!(app.ctx.shell.sidebar().session(id.as_str()).map(|s| s.title.as_str()), Some("ask first"));

    let choose = decisions::desk::Message::Card(id.to_string(), decisions::Message::Choose(decision, 0));
    update(&mut app, Message::Decisions(areas::decisions::Message::Desk(choose)));
    deliver(&mut app, &id, &mut feed);
    assert!(app.areas.decisions.focused().is_none(), "answered");
    assert!(!app.ctx.composer.is_busy(), "turn ended");
    assert_eq!(app.ctx.shell.sidebar().session(id.as_str()).map(|s| s.waiting), Some(false));
}

#[test]
fn feeds_name_turn_decision_and_compaction_events() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    update(&mut app, Message::NewSession);
    let session = app.ctx.sessions.active().unwrap();
    let id = session.id().clone();
    let writer = Arc::clone(session.writer());
    let root = session.dom().root().unwrap().id;
    let (_, mut feed) = writer.subscribe(Policy::every_patch(64));

    let turn = writer.commit(Draft::insert(root, ElementKind::Turn, [("status", Value::from(TurnStatus::Running))], "")).unwrap().node;
    let attrs: [(&str, Value); 2] = [("kind", DecisionKind::Question.into()), ("status", DecisionStatus::Waiting.into())];
    let decision = writer.commit(Draft::insert(turn, ElementKind::Decision, attrs, "?")).unwrap().node;
    writer.commit(Draft::set(decision, "status", DecisionStatus::Answered)).unwrap();
    writer.commit(Draft::set(turn, "status", TurnStatus::Cancelled)).unwrap();
    let compaction = writer.commit(Draft::insert(root, ElementKind::Compaction, [("covers", Value::List(vec![Value::Int(turn.0 as i64)]))], "summary")).unwrap().node;

    let mut named = Vec::new();
    while let Ok(event) = feed.try_recv() {
        match app.ctx.sessions.feed(id.as_str(), Feed::Event(event)) {
            Change::Patch(_, derived) => named.extend(derived.map(|event| format!("{event:?}"))),
            _ => panic!("every patch is new"),
        }
    }
    let session = id.clone();
    assert_eq!(
        named,
        [
            format!("{:?}", SessionEvent::TurnStarted { session: session.clone(), turn }),
            format!("{:?}", SessionEvent::DecisionWaiting { session: session.clone(), decision }),
            format!("{:?}", SessionEvent::DecisionAnswered { session: session.clone(), decision }),
            format!("{:?}", SessionEvent::TurnEnded { session: session.clone(), turn, status: TurnStatus::Cancelled }),
            format!("{:?}", SessionEvent::Compaction { session, node: compaction }),
        ]
    );
    assert_eq!(app.ctx.sessions.active().unwrap().dom(), &writer.with_dom(Dom::clone));
}

#[test]
fn pins_and_the_theme_survive_new_turns_and_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut app = fake_app(dir.path(), &["fake-a"]);
        update(&mut app, Message::NewSession);
        let id = app.ctx.sessions.active().unwrap().id().clone();
        let (_, mut feed) = app.ctx.sessions.active().unwrap().writer().subscribe(Policy::every_patch(1024));
        prompt(&mut app, "pin me");
        deliver(&mut app, &id, &mut feed);
        update(&mut app, Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::TogglePin(id.to_string()))));
        // The next turn replaces the sidebar row with a fresh summary.
        prompt(&mut app, "still pinned");
        deliver(&mut app, &id, &mut feed);
        let pinned: Vec<_> = app.ctx.shell.sidebar().pinned().into_iter().map(|summary| summary.id.clone()).collect();
        assert_eq!(pinned, [id.to_string()]);
        let task = app.command(&palette::CommandId::new("theme.light"));
        run(&mut app, task);
        id
    };
    let app = fake_app(dir.path(), &["fake-a"]);
    assert_eq!(app.ctx.shell.theme_preference(), shell::Preference::Light);
    let summary = app.ctx.shell.sidebar().session(id.as_str()).expect("listed after the restart");
    assert!(summary.pinned, "the pin was saved");
}

#[test]
fn tool_calls_collapse_into_one_row_that_expands_on_click() {
    use iced_test::Simulator;
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    update(&mut app, Message::NewSession);
    let session = app.ctx.sessions.active().unwrap();
    let id = session.id().clone();
    let writer = Arc::clone(session.writer());
    let (_, mut feed) = writer.subscribe(Policy::every_patch(1024));
    prompt(&mut app, "read the sources");
    let turn = writer.with_dom(|dom| dom_turns(dom).last().map(|turn| turn.id)).unwrap();
    for (index, file) in ["a.rs", "b.rs", "c.rs"].into_iter().enumerate() {
        let started = 1_000 * (index as i64 + 1);
        let attrs: [(&str, Value); 4] =
            [("tool", "read".into()), ("status", "done".into()), ("started", Value::Int(started)), ("ended", Value::Int(started + 11_000))];
        let call = writer.commit(Draft::insert(turn, ElementKind::Call, attrs, "")).unwrap().node;
        writer.commit(Draft::insert(call, ElementKind::Input, [] as [(&str, Value); 0], format!(r#"{{"path":"{file}"}}"#))).unwrap();
        writer.commit(Draft::insert(call, ElementKind::Result, [] as [(&str, Value); 0], format!("contents of {file}"))).unwrap();
    }
    deliver(&mut app, &id, &mut feed);

    let size = iced::Size::new(1600.0, 1000.0);
    let messages: Vec<Message> = {
        let mut ui = Simulator::with_size(iced::Settings::default(), size, app.view());
        assert!(ui.find("Read 3 files").is_ok(), "one collapsed row for the three reads");
        // From the first start (1s) to the last end (3s + 11s).
        assert!(ui.find("13s").is_ok(), "with the time the reads took");
        assert!(ui.find("contents of b.rs").is_err(), "collapsed");
        ui.click("Read 3 files").expect("the row is clickable");
        ui.into_messages().collect()
    };
    assert!(!messages.is_empty());
    for message in messages {
        update(&mut app, message);
    }
    let mut ui = Simulator::with_size(iced::Settings::default(), size, app.view());
    assert!(ui.find("contents of b.rs").is_ok(), "expanded on click");
}
