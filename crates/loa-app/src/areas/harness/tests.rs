//! The harness area driven through `App::update`: availability probes, the
//! saved default harness and executable overrides. The `live_` tests run the
//! installed omp with real model calls through the app's own session code.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::Task;
use iced::futures::StreamExt;
use iced::widget::text_editor::{Action, Edit};
use iced_runtime::Action as RuntimeAction;
use loa_adapter::{
    Adapter, Availability, AvailabilityStatus, Capabilities, HarnessSession, Registry, Result as AdapterResult,
    SessionConfig,
};
use loa_bus::{Policy, SessionId};
use loa_core::{Dom, ElementKind, PatchSink, TurnStatus, attr};
use loa_identity::NodeIdentity;
use loa_ui_composer as composer;
use loa_ui_palette as palette;

use super::{Message as HarnessMessage, TAB};
use crate::app::{App, Message};
use crate::area::Area;
use crate::services::{CHECKING, Dirs, Harnesses, Node, Services};
use crate::session::{Feed, dom_turns};

/// A harness that only reports `availability`.
struct Probed {
    id: &'static str,
    availability: Availability,
}

#[async_trait::async_trait]
impl Adapter for Probed {
    fn harness(&self) -> &'static str {
        self.id
    }

    async fn availability(&self) -> Availability {
        self.availability.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    async fn start(&self, _: SessionConfig, _: Arc<dyn PatchSink>) -> AdapterResult<Box<dyn HarnessSession>> {
        Err(loa_adapter::AdapterError::Unsupported { harness: self.id, what: "sessions" })
    }

    async fn resume(&self, _: SessionConfig, _: &Dom, _: Arc<dyn PatchSink>) -> AdapterResult<Box<dyn HarnessSession>> {
        Err(loa_adapter::AdapterError::Unsupported { harness: self.id, what: "sessions" })
    }
}

fn node() -> Node {
    Node::new(NodeIdentity::from_secret_bytes(&[5; 32]), "testhost".into())
}

fn app(dir: &Path, harnesses: Harnesses) -> App {
    App::build(Services::new(node(), Dirs::new(dir.join("loa")), harnesses, dir.to_owned()).unwrap()).0
}

/// `ready` reports version 1.2.3 and `gone` isn't installed; both start out
/// listed as available, as a CLI on `PATH` would.
fn probed_harnesses() -> Harnesses {
    let mut harnesses = Harnesses { registry: Registry::new(), list: Vec::new() };
    let ready = Availability::ready(Some("/opt/ready/bin/ready".into()), Some("1.2.3".into()));
    let gone = Availability::unavailable(AvailabilityStatus::NotInstalled, "`gone` not found");
    for (id, availability) in [("ready", ready), ("gone", gone)] {
        harnesses.add(Arc::new(Probed { id, availability }), id, true, Some(CHECKING.into()), None);
    }
    harnesses
}

/// Runs `task` the way the iced runtime would, feeding every message it
/// produces back through `App::update`, until no task is left.
fn run(app: &mut App, task: Task<Message>) {
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
            tokio::time::timeout(Duration::from_secs(20), outputs.collect::<Vec<_>>()).await.expect("task finished")
        });
        pending.extend(outputs.into_iter().map(|message| app.update(message)));
    }
}

fn update(app: &mut App, message: Message) {
    let task = app.update(message);
    run(app, task);
}

/// Runs the palette command titled `title` as a user would: open, type,
/// submit.
fn palette_command(app: &mut App, title: &str) {
    update(app, Message::Palette(palette::Message::Open));
    update(app, Message::Palette(palette::Message::QueryChanged(title.to_owned())));
    update(app, Message::Palette(palette::Message::Submit));
}

/// The harness `id` as the composer's picker lists it.
fn picker_option(app: &App, id: &str) -> composer::HarnessOption {
    app.ctx.services.harnesses.options().into_iter().find(|option| option.id == id).unwrap()
}

#[test]
fn probes_set_availability_in_the_picker_palette_and_harnesses_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path(), probed_harnesses());
    palette_command(&mut app, "Recheck harness availability");

    assert_eq!(app.ctx.shell.active_tab(), Some(TAB), "the command shows the Harnesses tab");
    let ready = picker_option(&app, "ready");
    assert!(ready.available);
    assert_eq!(ready.detail.as_deref(), Some("1.2.3"));
    let gone = picker_option(&app, "gone");
    assert!(!gone.available, "the probe overrides the PATH guess");
    assert_eq!(gone.detail.as_deref(), Some("`gone` not found"));
    assert_eq!(app.ctx.composer.harness().map(|option| option.id.as_str()), Some("ready"));
    let harness = app.ctx.services.harnesses.get("ready").unwrap();
    assert_eq!(harness.executable.as_deref(), Some(Path::new("/opt/ready/bin/ready")), "sessions run the probed executable");

    let titles: Vec<String> = app.areas.harness.palette_commands(&app.ctx).into_iter().map(|command| command.title).collect();
    assert!(titles.contains(&"Switch harness: ready".to_owned()), "{titles:?}");
    assert!(!titles.iter().any(|title| title.contains("gone")), "{titles:?}");

    let tab = app.areas.harness.view_tab(TAB, &app.ctx).expect("Harnesses tab");
    let mut ui = iced_test::simulator(tab);
    for label in ["Ready", "Not installed", "`gone` not found", "Version 1.2.3", "/opt/ready/bin/ready", "New sessions"] {
        assert!(ui.find(label).is_ok(), "the tab shows {label:?}");
    }
}

#[test]
fn an_unavailable_harness_cannot_be_picked() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path(), probed_harnesses());
    update(&mut app, Message::Harness(HarnessMessage::Recheck));
    // The composer's picker lists it greyed out and ignores the pick.
    update(&mut app, Message::Composer(composer::Message::HarnessPicked("gone".into())));
    assert_eq!(app.ctx.sessions.harness.as_deref(), Some("ready"));
    // A switch command for it is refused with a notice.
    update(&mut app, Message::Harness(HarnessMessage::Pick("gone".into())));
    assert_eq!(app.ctx.sessions.harness.as_deref(), Some("ready"));
    assert_eq!(app.ctx.notice(), Some("gone is unavailable"));
    assert!(app.ctx.sessions.active().is_none(), "nothing started");
}

#[test]
fn the_picked_harness_is_saved_as_the_default_for_the_next_launch() {
    let dir = tempfile::tempdir().unwrap();
    let mut harnesses = probed_harnesses();
    let other = Availability::ready(None, Some("2.0".into()));
    harnesses.add(Arc::new(Probed { id: "other", availability: other }), "other", true, None, None);
    let mut first = app(dir.path(), harnesses);
    update(&mut first, Message::Harness(HarnessMessage::Recheck));
    assert_eq!(first.ctx.sessions.harness.as_deref(), Some("ready"));
    // Picking starts a session on it; this fake can't run one.
    update(&mut first, Message::Composer(composer::Message::HarnessPicked("other".into())));
    let saved = first.ctx.services.config.load().unwrap().config.harness;
    assert_eq!(saved.default, "other");

    let mut harnesses = probed_harnesses();
    let other = Availability::ready(None, Some("2.0".into()));
    harnesses.add(Arc::new(Probed { id: "other", availability: other }), "other", true, None, None);
    let second = app(dir.path(), harnesses);
    assert_eq!(second.ctx.sessions.harness.as_deref(), Some("other"));
    assert_eq!(second.ctx.composer.harness().map(|option| option.id.as_str()), Some("other"));
}

#[cfg(unix)]
#[test]
fn an_executable_set_in_the_tab_is_saved_registered_and_probed() {
    let dir = tempfile::tempdir().unwrap();
    let codex = dir.path().join("my-codex");
    loa_exec::write_executable(&codex, "#!/bin/sh\necho 'codex-cli 0.99.1'\n").unwrap();
    let this = node();
    let key = this.key.clone();
    let dirs = Dirs::new(dir.path().join("loa"));
    let omp = dirs.omp.clone();
    let services = Services::new(this, dirs, Harnesses::detect(&key, &omp), dir.path().to_owned()).unwrap();
    let mut app = App::build(services).0;
    let id = loa_adapter_codex::HARNESS;

    update(&mut app, Message::Harness(HarnessMessage::EditExecutable(id.into(), codex.display().to_string())));
    update(&mut app, Message::Harness(HarnessMessage::SaveExecutable(id.into())));

    let saved = app.ctx.services.config.load().unwrap().config.harness;
    assert_eq!(saved.executable(id), Some(codex.as_path()));
    let harness = app.ctx.services.harnesses.get(id).unwrap();
    assert_eq!(harness.configured.as_deref(), Some(codex.as_path()));
    assert!(harness.option.available, "{:?}", harness.availability);
    assert_eq!(harness.availability.as_ref().and_then(|availability| availability.version.as_deref()), Some("0.99.1"));
    assert_eq!(harness.executable.as_deref(), Some(codex.as_path()));

    // A new launch registers Codex on the saved executable.
    let services = Services::new(node(), Dirs::new(dir.path().join("loa")), Harnesses::detect(&key, &omp), dir.path().to_owned()).unwrap();
    let relaunched = App::build(services).0;
    assert_eq!(relaunched.ctx.services.harnesses.get(id).unwrap().configured.as_deref(), Some(codex.as_path()));

    // Clearing the field restores the default and forgets the override.
    update(&mut app, Message::Harness(HarnessMessage::EditExecutable(id.into(), String::new())));
    update(&mut app, Message::Harness(HarnessMessage::SaveExecutable(id.into())));
    assert_eq!(app.ctx.services.config.load().unwrap().config.harness.executable(id), None);
    let harness = app.ctx.services.harnesses.get(id).unwrap();
    assert_eq!(harness.configured, None);
    assert_ne!(harness.executable.as_deref(), Some(codex.as_path()));
}

/// Drives an app as the iced runtime does, on a multi-thread runtime that
/// outlives each task (harness readers keep running on it), and delivers
/// every open session's writer feed.
pub(crate) struct Live {
    pub(crate) runtime: tokio::runtime::Runtime,
    sender: tokio::sync::mpsc::UnboundedSender<Message>,
    outputs: tokio::sync::mpsc::UnboundedReceiver<Message>,
    feeds: Vec<(SessionId, loa_bus::Subscription)>,
}

impl Live {
    pub(crate) fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let (sender, outputs) = tokio::sync::mpsc::unbounded_channel();
        Self { runtime, sender, outputs, feeds: Vec::new() }
    }

    pub(crate) fn spawn(&self, task: Task<Message>) {
        let Some(stream) = iced_runtime::task::into_stream(task) else { return };
        let sender = self.sender.clone();
        self.runtime.spawn(async move {
            let mut stream = stream;
            while let Some(action) = stream.next().await {
                if let RuntimeAction::Output(message) = action {
                    let _ = sender.send(message);
                }
            }
        });
    }

    pub(crate) fn update(&mut self, app: &mut App, message: Message) {
        // The app may start harness work inside `update`, as under iced's
        // runtime.
        let task = {
            let _guard = self.runtime.enter();
            app.update(message)
        };
        self.spawn(task);
    }

    /// Subscribes to sessions that opened since the last call, as the
    /// app's `Sessions::watch` does.
    pub(crate) fn subscribe(&mut self, app: &mut App) {
        let new: Vec<_> = app
            .ctx
            .sessions
            .iter()
            .filter(|session| !self.feeds.iter().any(|(id, _)| id == session.id()))
            .map(|session| (session.id().clone(), Arc::clone(session.writer())))
            .collect();
        for (id, writer) in new {
            let (dom, subscription) = writer.subscribe(Policy::coalescing(4096));
            self.feeds.push((id.clone(), subscription));
            self.update(app, Message::Session(id, Feed::Synced(Arc::new(dom))));
        }
    }

    /// Delivers task outputs and session feeds until `done` holds.
    pub(crate) fn until(&mut self, app: &mut App, what: &str, timeout: Duration, mut done: impl FnMut(&App) -> bool) {
        let deadline = Instant::now() + timeout;
        loop {
            self.subscribe(app);
            let mut idle = true;
            while let Ok(message) = self.outputs.try_recv() {
                idle = false;
                self.update(app, message);
            }
            for index in 0..self.feeds.len() {
                while let Ok(event) = self.feeds[index].1.try_recv() {
                    idle = false;
                    let id = self.feeds[index].0.clone();
                    self.update(app, Message::Session(id, Feed::Event(event)));
                }
            }
            if done(app) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}; notice: {:?}", app.ctx.notice());
            if idle {
                self.runtime.block_on(async { tokio::time::sleep(Duration::from_millis(10)).await });
            }
        }
    }

    /// Types `text` into the composer and submits it.
    pub(crate) fn prompt(&mut self, app: &mut App, text: &str) {
        for c in text.chars() {
            self.update(app, Message::Composer(composer::Message::Edit(Action::Edit(Edit::Insert(c)))));
        }
        self.update(app, Message::Composer(composer::Message::Submit));
    }
}

pub(crate) fn live_model() -> String {
    std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".to_owned())
}

/// The UI's copy of the active session.
pub(crate) fn active_dom(app: &App) -> &Dom {
    app.ctx.sessions.active().expect("an active session").dom()
}

/// Status of the session's turn number `index` (0 based).
pub(crate) fn turn_status(dom: &Dom, index: usize) -> Option<TurnStatus> {
    dom_turns(dom).nth(index).and_then(|turn| turn.str_attr(attr::STATUS)).and_then(TurnStatus::parse)
}

/// The text of turn `index`'s messages with role `role`.
pub(crate) fn turn_text(dom: &Dom, index: usize, role: &str) -> String {
    let Some(turn) = dom_turns(dom).nth(index) else { return String::new() };
    dom.descendants(turn.id)
        .filter(|node| node.kind == ElementKind::Message && node.str_attr(attr::ROLE) == Some(role))
        .map(|node| node.text.as_str())
        .collect()
}

pub(crate) fn ended(status: Option<TurnStatus>) -> bool {
    matches!(status, Some(TurnStatus::Done | TurnStatus::Error | TurnStatus::Cancelled))
}

/// An app over a temp data dir and the machine's real harnesses, with omp
/// probed ready and a session started on it, switched to [`live_model`].
pub(crate) fn live_session(dir: &Path, live: &mut Live) -> App {
    let node = Node::new(NodeIdentity::from_secret_bytes(&[11; 32]), "live".into());
    let dirs = Dirs::new(dir.join("loa"));
    let harnesses = Harnesses::detect(&node.key, &dirs.omp);
    let workspace = dir.join("work");
    std::fs::create_dir_all(&workspace).unwrap();
    // Other areas' startup work is not part of these tests.
    let (mut app, _) = App::build(Services::new(node, dirs, harnesses, workspace).unwrap());
    live.update(&mut app, Message::Harness(HarnessMessage::Recheck));
    live.until(&mut app, "the omp probe", Duration::from_secs(30), |app| {
        app.ctx.services.harnesses.get("omp").is_some_and(|omp| omp.availability.is_some())
    });
    let omp = app.ctx.services.harnesses.get("omp").unwrap();
    assert!(omp.option.available, "omp must be installed: {:?}", omp.availability);
    assert_eq!(app.ctx.sessions.harness.as_deref(), Some("omp"));

    live.update(&mut app, Message::NewSession);
    live.until(&mut app, "omp to start", Duration::from_secs(90), |app| {
        app.ctx.sessions.active().is_some_and(|session| session.harness().is_some())
    });
    let model = live_model();
    let (provider, name) = model.split_once('/').unwrap_or(("", &model));
    let option = composer::ModelOption { id: model.clone(), name: name.to_owned(), provider: provider.to_owned(), role: None };
    live.update(&mut app, Message::Composer(composer::Message::ModelPicked(option)));
    live.until(&mut app, "the model switch", Duration::from_secs(30), |app| {
        active_dom(app).root().and_then(|root| root.str_attr(attr::MODEL)) == Some(model.as_str())
    });
    app
}

#[test]
#[ignore = "runs the installed omp with real model calls (LOA_LIVE_MODEL, default anthropic/claude-haiku-5-5)"]
fn live_omp_streams_into_the_journal_and_the_chat() {
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::new();
    let mut app = live_session(dir.path(), &mut live);
    let id = app.ctx.sessions.active_id().unwrap().clone();

    live.prompt(&mut app, "Write the numbers 1 to 40 as English words, one per line. Then write PINEAPPLE on its own line. Use no tools.");
    let mut seen = Vec::new();
    live.until(&mut app, "the turn to end", Duration::from_secs(180), |app| {
        let dom = active_dom(app);
        let text = turn_text(dom, 0, "assistant");
        if !text.is_empty() && seen.last() != Some(&text.len()) {
            seen.push(text.len());
        }
        ended(turn_status(dom, 0))
    });
    let dom = active_dom(&app);
    assert_eq!(turn_status(dom, 0), Some(TurnStatus::Done), "notice: {:?}", app.ctx.notice());
    let reply = turn_text(dom, 0, "assistant");
    assert!(reply.contains("PINEAPPLE"), "{reply}");
    assert!(seen.len() >= 3, "the reply streamed into the app in pieces: {seen:?}");

    // The chat view shows the reply.
    let mut ui = iced_test::Simulator::with_size(iced::Settings::default(), iced::Size::new(1600.0, 1000.0), app.view());
    let shown = ui.find(|candidate: iced_test::selector::Candidate<'_>| match candidate {
        iced_test::selector::Candidate::Text { content, .. } if content.contains("PINEAPPLE") => Some(()),
        _ => None,
    });
    assert!(shown.is_ok(), "the chat shows the reply");
    drop(ui);

    // The journal holds it once the session is closed.
    let harness = app.ctx.close_session(id.as_str()).expect("a running harness");
    live.runtime.block_on(harness.shutdown()).unwrap();
    let journal = app.ctx.services.library.load(id.as_str()).unwrap();
    assert_eq!(turn_text(&journal, 0, "assistant"), reply);
    assert_eq!(turn_status(&journal, 0), Some(TurnStatus::Done));
    // omp's TUI bookkeeping (status widgets, cost pings) leaves no rows; its
    // command list is session data on the root, hidden from the chat.
    let rows: Vec<_> = journal.iter().filter(|node| node.kind == ElementKind::Ext && node.str_attr(attr::TYPE) != Some(loa_ui_chat::COMMANDS_EXT)).map(|node| node.str_attr(attr::TYPE)).collect();
    assert!(rows.is_empty(), "{rows:?}");
}

#[test]
#[ignore = "runs the installed omp with real model calls (LOA_LIVE_MODEL, default anthropic/claude-haiku-5-5)"]
fn live_omp_runs_a_message_queued_mid_turn_and_escape_interrupts() {
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::new();
    let mut app = live_session(dir.path(), &mut live);

    live.prompt(&mut app, "Write the numbers 1 to 30 as English words, one per line. Use no tools.");
    live.until(&mut app, "the first reply to stream", Duration::from_secs(120), |app| {
        !turn_text(active_dom(app), 0, "assistant").is_empty() || ended(turn_status(active_dom(app), 0))
    });
    assert!(app.ctx.composer.is_busy());
    live.prompt(&mut app, "Now reply with just the word MANGO.");
    assert_eq!(app.ctx.composer.queue().map(|queued| queued.text.as_str()).collect::<Vec<_>>(), ["Now reply with just the word MANGO."]);
    assert_eq!(dom_turns(active_dom(&app)).count(), 1, "the queued message waits for the turn");

    live.until(&mut app, "the queued message's turn to end", Duration::from_secs(180), |app| ended(turn_status(active_dom(app), 1)));
    let dom = active_dom(&app);
    assert_eq!(turn_status(dom, 0), Some(TurnStatus::Done));
    assert_eq!(turn_text(dom, 1, "user"), "Now reply with just the word MANGO.");
    assert!(turn_text(dom, 1, "assistant").contains("MANGO"), "{}", turn_text(dom, 1, "assistant"));
    assert_eq!(app.ctx.composer.queue().count(), 0);

    live.prompt(&mut app, "Write a numbered list of 200 different fruits, one per line. Use no tools.");
    live.until(&mut app, "the long reply to stream", Duration::from_secs(120), |app| {
        turn_text(active_dom(app), 2, "assistant").len() > 20
    });
    let escape = iced::keyboard::Event::KeyPressed {
        key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
        modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
        physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Escape),
        location: iced::keyboard::Location::Standard,
        modifiers: iced::keyboard::Modifiers::default(),
        text: None,
        repeat: false,
    };
    live.update(&mut app, Message::Keyboard(escape));
    live.until(&mut app, "Esc to end the turn", Duration::from_secs(60), |app| ended(turn_status(active_dom(app), 2)));
    let dom = active_dom(&app);
    assert_eq!(turn_status(dom, 2), Some(TurnStatus::Cancelled));
    assert!(!turn_text(dom, 2, "assistant").contains("200."), "interrupted before the list finished");
    assert!(!app.ctx.composer.is_busy());
    assert!(app.ctx.sessions.active().is_some_and(|session| !session.is_busy()));

    let harness = app.ctx.sessions.active().and_then(|session| session.harness().cloned()).unwrap();
    live.runtime.block_on(harness.shutdown()).unwrap();
}
