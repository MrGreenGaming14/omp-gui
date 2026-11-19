//! The decisions area against the installed omp, approval mode always-ask,
//! driven through `App::update` like the running app: the app starts the
//! session, the feed brings the cards in, and the cards' answers go back
//! through the area. Run with
//! `cargo test -p loa-app areas::decisions::live -- --ignored`;
//! `LOA_LIVE_MODEL` picks the model (default `anthropic/claude-haiku-5-5`).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::Task;
use iced::futures::StreamExt;
use iced::widget::text_editor::{Action, Edit};
use iced_runtime::Action as RuntimeAction;
use loa_adapter::{Adapter, Capabilities, HarnessSession, Registry, SessionConfig};
use loa_adapter_omp::{OmpAdapter, OmpAdapterConfig};
use loa_bus::{Policy, SessionId};
use loa_core::{DecisionKind, DecisionStatus, Dom, ElementKind, PatchSink, Role, TurnStatus, answer, attr};
use loa_identity::NodeIdentity;
use loa_ui_composer as composer;
use loa_ui_decisions::{Decision, Shortcut, desk};
use tokio::sync::mpsc;

use super::Message;
use crate::app::{App, Message as AppMessage};
use crate::services::{Dirs, Harnesses, Node, Services};
use crate::session::{Feed, dom_turns};

const TURN_TIMEOUT: Duration = Duration::from_secs(240);

fn model() -> String {
    std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".into())
}

/// omp as the app registers it (approval mode always-ask), pinned to the
/// live test model.
struct LiveOmp(OmpAdapter);

#[async_trait::async_trait]
impl Adapter for LiveOmp {
    fn harness(&self) -> &'static str {
        self.0.harness()
    }

    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }

    async fn start(&self, mut config: SessionConfig, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        config.model = Some(model());
        self.0.start(config, sink).await
    }

    async fn resume(&self, mut config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        config.model = Some(model());
        self.0.resume(config, dom, sink).await
    }
}

/// The app on a live omp, its tasks running on a multi-thread runtime the
/// way iced runs them.
struct Live {
    app: App,
    runtime: tokio::runtime::Runtime,
    sender: mpsc::UnboundedSender<AppMessage>,
    receiver: mpsc::UnboundedReceiver<AppMessage>,
    id: SessionId,
    /// The session's feed, once it started.
    feed: Option<loa_bus::Subscription>,
    work: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl Live {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let node = Node::new(NodeIdentity::from_secret_bytes(&[9; 32]), "livehost".into());
        let dirs = Dirs::new(dir.path().join("loa"));
        let mut omp = OmpAdapterConfig::new(node.key.clone(), dirs.omp.clone());
        omp.approval_mode = Some("always-ask".into());
        let mut harnesses = Harnesses { registry: Registry::new(), list: Vec::new() };
        harnesses.add(Arc::new(LiveOmp(OmpAdapter::new(omp))), "omp", true, None, None);
        let services = Services::new(node, dirs, harnesses, work.clone()).unwrap();
        let (sender, receiver) = mpsc::unbounded_channel();
        let (app, task) = {
            let _entered = runtime.enter();
            App::build(services)
        };
        let mut live = Self { app, runtime, sender, receiver, id: SessionId::from("none"), feed: None, work, _dir: dir };
        live.spawn(task);
        live.update(AppMessage::NewSession);
        live.pump_until("the session starts", |app| app.ctx.sessions.active().is_some());
        let session = live.app.ctx.sessions.active().unwrap();
        live.id = session.id().clone();
        // Subscribed after the start: catch the UI copy up as the app's
        // feed does when it (re)subscribes.
        let (dom, feed) = session.writer().subscribe(Policy::every_patch(4096));
        live.feed = Some(feed);
        live.update(AppMessage::Session(live.id.clone(), Feed::Synced(Arc::new(dom))));
        live
    }

    fn spawn(&self, task: Task<AppMessage>) {
        let Some(stream) = iced_runtime::task::into_stream(task) else { return };
        let sender = self.sender.clone();
        self.runtime.spawn(stream.for_each(move |action| {
            if let RuntimeAction::Output(message) = action {
                let _ = sender.send(message);
            }
            std::future::ready(())
        }));
    }

    fn update(&mut self, message: AppMessage) {
        let task = {
            let _entered = self.runtime.enter();
            self.app.update(message)
        };
        self.spawn(task);
    }

    /// Feeds what tasks and the session feed delivered back into the app.
    fn drain(&mut self) {
        loop {
            let mut idle = true;
            while let Ok(message) = self.receiver.try_recv() {
                idle = false;
                self.update(message);
            }
            while let Some(Ok(event)) = self.feed.as_mut().map(loa_bus::Subscription::try_recv) {
                idle = false;
                self.update(AppMessage::Session(self.id.clone(), Feed::Event(event)));
            }
            if idle {
                return;
            }
        }
    }

    fn pump_until(&mut self, what: &str, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + TURN_TIMEOUT;
        loop {
            self.drain();
            if done(&self.app) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting until {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Types `text` into the composer and submits it.
    fn prompt(&mut self, text: &str) {
        for c in text.chars() {
            self.update(AppMessage::Composer(composer::Message::Edit(Action::Edit(Edit::Insert(c)))));
        }
        self.update(AppMessage::Composer(composer::Message::Submit));
    }

    fn dom(&self) -> Dom {
        self.app.ctx.sessions.get(self.id.as_str()).unwrap().writer().with_dom(Dom::clone)
    }

    /// Sends `text` and runs the turn to its end. Each card that comes to
    /// take the keys is handed once to `answer`, which works it like a user.
    fn turn(&mut self, text: &str, mut answer: impl FnMut(&mut Live, &Decision)) -> Vec<Decision> {
        let before = dom_turns(&self.dom()).count();
        self.prompt(text);
        let mut shown = HashSet::new();
        let mut cards = Vec::new();
        let deadline = Instant::now() + TURN_TIMEOUT;
        loop {
            self.drain();
            if let Some(focused) = self.app.areas.decisions.focused()
                && shown.insert(focused)
            {
                let session = self.app.ctx.sessions.get(self.id.as_str()).unwrap();
                let decision = Decision::from_dom(session.dom(), focused).expect("the card's decision");
                assert!(self.app.areas.decisions.card(self.id.as_str(), focused).is_some(), "drawn inline");
                assert_eq!(session.summary().map(|s| s.waiting), Some(true), "the session shows as waiting");
                cards.push(decision.clone());
                answer(self, &decision);
            }
            let dom = self.dom();
            let ended = dom_turns(&dom).count() > before
                && dom_turns(&dom).last().and_then(|turn| turn.str_attr(attr::STATUS)).and_then(TurnStatus::parse).is_some_and(|status| {
                    !matches!(status, TurnStatus::Running | TurnStatus::Waiting)
                });
            if ended {
                self.drain();
                return cards;
            }
            assert!(Instant::now() < deadline, "the turn didn't end in time");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn card(&mut self, message: loa_ui_decisions::Message) {
        self.update(AppMessage::Decisions(Message::Desk(desk::Message::Card(self.id.to_string(), message))));
    }

    fn key(&mut self, shortcut: Shortcut) {
        self.update(AppMessage::Decisions(Message::Desk(desk::Message::Shortcut(shortcut))));
    }

    fn me(&self) -> String {
        self.app.ctx.services.node.key.clone()
    }

    fn shutdown(self) {
        if let Some(harness) = self.app.ctx.sessions.get(self.id.as_str()).and_then(|s| s.harness().cloned()) {
            self.runtime.block_on(harness.shutdown()).unwrap();
        }
    }
}

fn assistant_text(dom: &Dom) -> String {
    dom.iter()
        .filter(|n| n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == Some(Role::Assistant.as_str()) && n.str_attr(attr::KIND) != Some("thinking"))
        .map(|n| n.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Answers an approval that isn't the point of the test.
fn allow(live: &mut Live, decision: &Decision) {
    live.card(loa_ui_decisions::Message::Choose(decision.id, 0));
}

#[test]
#[ignore = "runs the installed omp with real model calls"]
fn approving_a_bash_call_on_its_card_runs_it() {
    let mut live = Live::start();
    let cards = live.turn(
        "Run the shell command `echo loa-approved-7731 > approved.txt` with your bash tool, then say done.",
        |live, decision| {
            assert_eq!(decision.kind, DecisionKind::Approval, "{decision:?}");
            assert_eq!(live.app.areas.decisions.desk.badge(live.id.as_str()), 1);
            // Number key 1 then Enter: allow once.
            live.key(Shortcut::Pick(0));
            live.key(Shortcut::Confirm);
        },
    );
    assert!(!cards.is_empty(), "omp asked for approval");
    let written = std::fs::read_to_string(live.work.join("approved.txt")).expect("the approved command ran");
    assert_eq!(written.trim(), "loa-approved-7731");
    let dom = live.dom();
    for card in cards {
        let recorded = Decision::from_dom(&dom, card.id).unwrap();
        assert_eq!(recorded.status, DecisionStatus::Answered);
        assert_eq!(recorded.answer, [answer::ALLOW_ONCE]);
        assert_eq!(recorded.by, Some(live.me()));
    }
    assert_eq!(live.app.areas.decisions.desk.total(), 0, "no badge once answered");
    assert_eq!(live.app.ctx.shell.sidebar().session(live.id.as_str()).map(|s| s.waiting), Some(false));
    live.shutdown();
}

#[test]
#[ignore = "runs the installed omp with real model calls"]
fn denying_with_a_reason_on_the_card_stops_the_call() {
    let mut live = Live::start();
    let cards = live.turn(
        "Run the shell command `touch denied.txt` with your bash tool. If it is denied, do not retry; just tell me why it was denied.",
        |live, decision| {
            assert_eq!(decision.kind, DecisionKind::Approval, "{decision:?}");
            live.card(loa_ui_decisions::Message::Text(decision.id, "the codeword is pelican".into()));
            // Enter in the reason field denies with the reason.
            live.card(loa_ui_decisions::Message::SubmitText(decision.id));
        },
    );
    assert!(!live.work.join("denied.txt").exists(), "the denied command did not run");
    let first = cards.first().expect("omp asked for approval");
    let dom = live.dom();
    let recorded = Decision::from_dom(&dom, first.id).unwrap();
    assert_eq!(recorded.answer, [answer::DENY]);
    assert_eq!(recorded.reason.as_deref(), Some("the codeword is pelican"));
    assert_eq!(recorded.by, Some(live.me()));
    let said = assistant_text(&dom).to_lowercase();
    assert!(said.contains("pelican"), "the model got the reason: {said:?}");
    live.shutdown();
}

#[test]
#[ignore = "runs the installed omp with real model calls"]
fn an_ask_question_is_answered_on_its_card() {
    let mut live = Live::start();
    let mut asked = None;
    live.turn(
        "Use your ask tool to ask me exactly one question, \"Which color?\", with the two options red and green. \
         After I answer, reply with only the color I picked.",
        |live, decision| {
            if decision.kind == DecisionKind::Approval {
                return allow(live, decision);
            }
            assert_eq!(decision.kind, DecisionKind::Question, "{decision:?}");
            let green = decision.options.iter().position(|o| o.to_lowercase().contains("green")).expect("a green option");
            asked = Some(decision.id);
            live.card(loa_ui_decisions::Message::Choose(decision.id, green));
        },
    );
    let asked = asked.expect("omp asked through the card");
    let dom = live.dom();
    let recorded = Decision::from_dom(&dom, asked).unwrap();
    assert_eq!(recorded.status, DecisionStatus::Answered);
    assert!(recorded.answer[0].to_lowercase().contains("green"), "{recorded:?}");
    assert_eq!(recorded.by, Some(live.me()));
    let said = assistant_text(&dom).to_lowercase();
    assert!(said.contains("green") && !said.contains("red"), "assistant said {said:?}");
    live.shutdown();
}
