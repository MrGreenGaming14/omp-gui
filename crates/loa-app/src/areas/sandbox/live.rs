//! `sandbox_task` against the installed omp with real model calls, driven
//! through `App::update` like the running app: omp as the app registers it
//! (approval mode always-ask, the card allowed like a user would), the
//! session gets the tool from this area, calls it with the local provider in
//! an isolated git worktree, an omp worker does the work through the stub,
//! and the patch comes back to the Sandbox tab and is applied. Run with
//! `cargo test -p loa-app areas::sandbox::live -- --ignored`;
//! `LOA_LIVE_MODEL` picks the model (default `anthropic/claude-haiku-5-5`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::Task;
use iced::futures::StreamExt;
use iced::widget::text_editor::{Action, Edit};
use iced_runtime::Action as RuntimeAction;
use loa_adapter::{Adapter, Capabilities, HarnessSession, Registry, SessionConfig};
use loa_adapter_omp::{OmpAdapter, OmpAdapterConfig};
use loa_bus::{Policy, SessionId};
use loa_core::{Dom, ElementKind, PatchSink, TurnStatus, attr};
use loa_identity::NodeIdentity;
use loa_sandbox::{RunState, SandboxEvent, SandboxKind};
use loa_ui_composer as composer;
use loa_ui_decisions::desk;
use tokio::sync::{broadcast, mpsc};

use super::{Message, TAB};
use crate::app::{App, Message as AppMessage};
use crate::areas::decisions;
use crate::services::{Dirs, Harnesses, Node, Services};
use crate::session::{Feed, dom_turns};

const TURN_TIMEOUT: Duration = Duration::from_secs(600);

fn model() -> String {
    std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".into())
}

/// omp as the app registers it, pinned to the live test model; the sandbox
/// worker runs on it too.
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

/// The app with its tasks on a multi-thread runtime, the way iced runs them.
struct Live {
    app: App,
    runtime: tokio::runtime::Runtime,
    sender: mpsc::UnboundedSender<AppMessage>,
    receiver: mpsc::UnboundedReceiver<AppMessage>,
    id: SessionId,
    feed: Option<loa_bus::Subscription>,
    /// The area's run feed, as its subscription delivers it.
    runs: broadcast::Receiver<SandboxEvent>,
}

impl Live {
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

    fn drain(&mut self) {
        loop {
            let mut idle = true;
            while let Ok(message) = self.receiver.try_recv() {
                idle = false;
                self.update(message);
            }
            let events: Vec<_> = std::iter::from_fn(|| self.feed.as_mut().and_then(|feed| feed.try_recv().ok())).collect();
            if !events.is_empty() {
                idle = false;
                self.update(AppMessage::Session(self.id.clone(), Feed::Events(events)));
            }
            while let Ok(event) = self.runs.try_recv() {
                idle = false;
                self.update(AppMessage::Sandbox(Message::Event(event)));
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

    fn dom(&self) -> Dom {
        self.app.ctx.sessions.get(self.id.as_str()).unwrap().writer().with_dom(Dom::clone)
    }
}

#[test]
#[ignore = "runs the installed omp with real model calls"]
fn an_omp_session_in_the_app_runs_sandbox_task_in_an_isolated_worktree_and_the_patch_comes_back() {
    let dir = tempfile::tempdir().unwrap();
    let repo = super::tests::repository(dir.path());
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let node = Node::new(NodeIdentity::from_secret_bytes(&[11; 32]), "livehost".into());
    let dirs = Dirs::new(dir.path().join("loa"));
    let mut omp = OmpAdapterConfig::new(node.key.clone(), dirs.omp.clone());
    omp.approval_mode = Some("always-ask".into());
    let mut harnesses = Harnesses { registry: Registry::new(), list: Vec::new() };
    harnesses.add(Arc::new(LiveOmp(OmpAdapter::new(omp))), "omp", true, None, None);
    let services = Services::new(node, dirs, harnesses, repo.clone()).unwrap();
    let (sender, receiver) = mpsc::unbounded_channel();
    let (app, task) = {
        let _entered = runtime.enter();
        App::build(services)
    };
    let runs = app.areas.sandbox.subscribe();
    let mut live = Live { app, runtime, sender, receiver, id: SessionId::from("none"), feed: None, runs };
    live.spawn(task);
    live.pump_until("the providers are probed", |app| {
        app.ctx.app_tools.iter().any(|tool| tool.name == "sandbox_task" && !tool.details.is_empty())
    });

    live.update(AppMessage::NewSession);
    live.pump_until("the session starts", |app| app.ctx.sessions.active().is_some());
    let session = live.app.ctx.sessions.active().unwrap();
    live.id = session.id().clone();
    let (dom, feed) = session.writer().subscribe(Policy::every_patch(4096));
    live.feed = Some(feed);
    live.update(AppMessage::Session(live.id.clone(), Feed::Synced(Arc::new(dom))));

    let text = "Call the sandbox_task tool exactly once with kind \"local\", isolation true, and this task: \
                \"Create the file hello.txt in the workspace root containing exactly the line: hello from the sandbox\". \
                Do not create the file yourself. When the tool returns, tell me which files its patch changes.";
    for c in text.chars() {
        live.update(AppMessage::Composer(composer::Message::Edit(Action::Edit(Edit::Insert(c)))));
    }
    live.update(AppMessage::Composer(composer::Message::Submit));

    // Allow every card the turn raises, like the user would.
    let deadline = Instant::now() + TURN_TIMEOUT;
    let mut allowed = Vec::new();
    loop {
        live.drain();
        if let Some(focused) = live.app.areas.decisions.focused()
            && !allowed.contains(&focused)
        {
            allowed.push(focused);
            let card = desk::Message::Card(live.id.to_string(), loa_ui_decisions::Message::Choose(focused, 0));
            live.update(AppMessage::Decisions(decisions::Message::Desk(card)));
        }
        let dom = live.dom();
        let ended = dom_turns(&dom).last().and_then(|turn| turn.str_attr(attr::STATUS)).and_then(TurnStatus::parse).is_some_and(|status| {
            !matches!(status, TurnStatus::Running | TurnStatus::Waiting)
        });
        if ended && live.app.areas.sandbox.runs().running() == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "the turn didn't end in time");
        std::thread::sleep(Duration::from_millis(50));
    }
    live.drain();
    eprintln!("cards allowed: {}", allowed.len());

    let entries = live.app.areas.sandbox.runs().entries().to_vec();
    assert_eq!(entries.len(), 1, "{entries:#?}");
    let entry = &entries[0];
    assert_eq!((entry.run.kind, entry.run.isolated, entry.run.parent.as_str()), (SandboxKind::Local, true, live.id.as_str()));
    let patch = match &entry.state {
        RunState::Finished { patch: Some(patch), .. } => patch.clone(),
        other => panic!("the run returns a patch: {other:?}"),
    };
    eprintln!("patch:\n{patch}");
    assert!(patch.contains("hello.txt") && patch.contains("+hello from the sandbox"), "{patch}");
    assert!(!repo.join("hello.txt").exists(), "the workspace stays untouched");

    // The patch is up for review in the Sandbox tab.
    assert_eq!(live.app.ctx.shell.active_tab(), Some(TAB));
    assert_eq!(live.app.areas.sandbox.selected, Some(entry.run.run));
    assert!(live.app.areas.sandbox.diff.files().iter().any(|file| file.path() == "hello.txt"));

    // The worker was journaled in the app's library and used the sandbox tools.
    let worker = live.app.ctx.services.library.load(&entry.run.worker).expect("the worker session is journaled");
    let calls: Vec<_> = worker.iter().filter(|n| n.kind == ElementKind::Call).filter_map(|n| n.str_attr(attr::TOOL)).collect();
    eprintln!("worker calls: {calls:?}");
    assert!(calls.iter().any(|name| name.starts_with("sandbox_")), "{calls:?}");

    // The parent's DOM holds the call and its result.
    let dom = live.dom();
    let call = dom.iter().find(|n| n.kind == ElementKind::Call && n.str_attr(attr::TOOL) == Some("sandbox_task")).expect("the parent called sandbox_task");
    let result = dom.children_of_kind(call.id, ElementKind::Result).next().expect("the call has a result");
    assert!(result.text.contains("hello.txt"), "{}", result.text);

    live.update(AppMessage::Sandbox(Message::Apply(entry.run.run)));
    live.pump_until("the patch is applied", |app| app.areas.sandbox.runs().get(entry.run.run).is_some_and(|entry| entry.pending_patch().is_none()));
    assert_eq!(std::fs::read_to_string(repo.join("hello.txt")).unwrap().trim(), "hello from the sandbox");

    if let Some(harness) = live.app.ctx.sessions.get(live.id.as_str()).and_then(|s| s.harness().cloned()) {
        live.runtime.block_on(harness.shutdown()).unwrap();
    }
}
