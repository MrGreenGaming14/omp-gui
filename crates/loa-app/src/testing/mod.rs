//! The app driven headless for tests: [`TestApp`] runs tasks and the app's
//! real subscriptions on a tokio runtime, [`FakeHarness`] is the scripted
//! harness sessions run on, [`TwoNodes`] two apps whose peer nodes talk over
//! loopback.

use loa_util::LockExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::Task;
use iced::futures::channel::mpsc;
use iced::futures::{FutureExt, StreamExt};
use iced::keyboard::{Key, Modifiers};
use iced::widget::text_editor::{Action, Edit};
use iced_futures::subscription::{Tracker, into_recipes};
use iced_runtime::Action as RuntimeAction;
use loa_harness::testing::ScriptedAdapter;
use loa_trust::NodeIdentity;
use loa_ui_session::composer;
use loa_ui_shell::palette;
use loa_models::error::{Result as CheckResult, SettingsValidator};
use loa_models::files::OmpFiles;
use loa_models::keys::EnvironmentPresence;
use loa_models::login::UrlOpener;
use loa_models::service::{Backend, FilesSource, Parts};
use loa_models::testing::{FakeOmp, OfflineOmpAccount};
use loa_projects::{ServiceConfig, Summarizer, Summarizers};
use tempfile::TempDir;
use tokio::runtime::Runtime;

use crate::app::usage::{Usage, UsageSource, parse};
use crate::app::workspace::{FolderPicker, GitProbe};
use crate::app::{App, Message};
use crate::area::Area;
use crate::areas::{models, projects, sessions};
use crate::services::{Dirs, Harnesses, Host, NodeServices, Services};

/// The scripted harness of [`TestApp`] sessions.
pub(crate) type FakeHarness = ScriptedAdapter;

/// How long [`TestApp::until`] waits.
const PATIENCE: Duration = Duration::from_secs(10);

pub(crate) struct TestApp {
    pub app: App,
    pub harness: FakeHarness,
    pub usage: Arc<FakeUsage>,
    pub dir: TempDir,
    runtime: Arc<Runtime>,
    tracker: Tracker,
    feed: (mpsc::Sender<Message>, mpsc::Receiver<Message>),
}

impl TestApp {
    /// An app on harness `fake`, taking host tools and MCP servers, in a
    /// fresh home, on its own runtime.
    pub fn new() -> Self {
        let capabilities = loa_contracts::harness::Capabilities::builder().host_tools().mcp_servers().build();
        Self::with(FakeHarness::new("fake").capabilities(capabilities))
    }

    pub fn with(harness: FakeHarness) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
        Self::on(Arc::new(runtime), harness, "testhost", |_| {})
    }

    /// An app on `runtime` whose central config `setup` edits first.
    pub fn on(runtime: Arc<Runtime>, harness: FakeHarness, name: &str, setup: impl FnOnce(&mut loa_config::Config)) -> Self {
        Self::open(runtime, harness, name, tempfile::tempdir().expect("a temp dir"), setup)
    }

    /// The app started again over the same home, as after a restart.
    pub fn reopen(self) -> Self {
        let Self { app, harness, dir, runtime, tracker, .. } = self;
        let name = app.ctx.services.node.label.clone();
        // Cancelled subscriptions let go of their session writers once polled.
        {
            let _entered = runtime.enter();
            drop((app, tracker));
        }
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(5)).await });
        Self::open(runtime, harness, &name, dir, |_| {})
    }

    fn open(runtime: Arc<Runtime>, harness: FakeHarness, name: &str, dir: TempDir, setup: impl FnOnce(&mut loa_config::Config)) -> Self {
        let dirs = Dirs::new(dir.path().join("loa"));
        let config = loa_config::ConfigStore::in_dir(&dirs.data);
        let mut settings = config.load().expect("the default config");
        settings.network.discovery.mdns = false;
        settings.network.discovery.tailscale = false;
        setup(&mut settings);
        config.save(&settings).expect("config saved");
        let mut harnesses = Harnesses { registry: loa_contracts::harness::Registry::new(), list: Vec::new() };
        harnesses.add(Arc::new(harness.clone()), harness_id(&harness), true, None, None);
        let identity = NodeIdentity::from_secret_bytes(&[name.len() as u8; 32]);
        let key_source = loa_trust::KeySource::Memory { reason: "the key was handed in".into() };
        let node = NodeServices::open(identity, name.to_owned(), key_source, &dirs.data).expect("the node");
        // Nothing reaches git, a folder dialog or omp's usage.
        let usage = Arc::new(FakeUsage::default());
        let host = Host { git: Arc::new(Offline), picker: Arc::new(Offline), usage: usage.clone() };
        let services = Services::new(node, dirs, harnesses, host, dir.path().to_owned()).expect("services");
        let _entered = runtime.enter();
        // The startup tasks (harness probes, update checks) reach outside the test: dropped.
        let (mut app, _startup) = App::build(services);
        // Nothing reaches omp, a model or the developer's home.
        let parts = Parts {
            rpc: Arc::new(FakeOmp::default()),
            omp_account: Arc::new(OfflineOmpAccount),
            files: FilesSource::Fixed(OmpFiles::new(dir.path().join("omp"))),
            validator: Arc::new(Offline),
            opener: Arc::new(Offline),
            harnesses: Vec::new(),
            environment: Arc::new(Offline),
        };
        *app.areas.get_mut::<models::State>() = models::State::with_backend(Backend::new(parts));
        let (projects, _rebuilds) = projects::State::with(&mut app.ctx, Arc::new(Offline), ServiceConfig::default());
        *app.areas.get_mut::<projects::State>() = projects;
        let sessions = app.areas.get_mut::<sessions::State>();
        (sessions.home, sessions.opener) = (dir.path().join("home"), Arc::new(Offline));
        Self { app, harness, usage, dir, runtime, tracker: Tracker::new(), feed: mpsc::channel(256) }
    }

    pub fn area<A: Area>(&self) -> &A {
        self.app.areas.get::<A>()
    }

    pub fn area_mut<A: Area>(&mut self) -> &mut A {
        self.app.areas.get_mut::<A>()
    }

    /// Updates the app with `message` and runs the task it returns.
    pub fn send(&mut self, message: Message) {
        let task = {
            let _entered = self.runtime.enter();
            self.app.update(message)
        };
        self.run(task);
    }

    /// Runs `task` as the iced runtime would, feeding every message it
    /// produces back through `update`, until no task is left.
    pub fn run(&mut self, task: Task<Message>) {
        let mut pending = vec![task];
        while let Some(task) = pending.pop() {
            let Some(stream) = iced_runtime::task::into_stream(task) else { continue };
            let outputs = self.runtime.block_on(async {
                let outputs = stream.filter_map(|action| async move {
                    match action {
                        RuntimeAction::Output(message) => Some(message),
                        _ => None,
                    }
                });
                tokio::time::timeout(PATIENCE, outputs.collect::<Vec<_>>()).await.expect("the task finished")
            });
            let _entered = self.runtime.enter();
            pending.extend(outputs.into_iter().map(|message| self.app.update(message)));
        }
    }

    /// Runs palette command `id` as picking it in the palette does.
    pub fn command(&mut self, id: &str) {
        let task = self.app.command(&palette::CommandId::new(id));
        self.run(task);
    }

    /// Types `query` in the palette and runs the first match.
    pub fn palette(&mut self, query: &str) {
        for message in [palette::Message::Open, palette::Message::QueryChanged(query.to_owned()), palette::Message::Submit] {
            self.send(Message::Palette(message));
        }
    }

    /// Types `text` into the composer and submits it.
    pub fn prompt(&mut self, text: &str) {
        for c in text.chars() {
            self.send(Message::Composer(composer::Message::Edit(Action::Edit(Edit::Insert(c)))));
        }
        self.send(Message::Composer(composer::Message::Submit));
    }

    pub fn key(&mut self, key: Key, modifiers: Modifiers) {
        let iced::Event::Keyboard(event) = loa_ui_kit::testkit::key(key, modifiers, None) else { unreachable!("a key event") };
        self.send(Message::Keyboard(event));
    }

    /// Clicks the button labelled `label` in the app's view.
    pub fn click(&mut self, label: &str) {
        let messages = loa_ui_kit::testkit::click_label(self.app.view(), label);
        assert!(!messages.is_empty(), "nothing labelled {label} to click");
        messages.into_iter().for_each(|message| self.send(message));
    }

    /// Delivers what the app's subscriptions produced so far: new
    /// subscriptions start, dropped ones are cancelled, as in iced.
    pub fn pump_feeds(&mut self) {
        let _entered = self.runtime.enter();
        let futures = self.tracker.update(into_recipes(self.app.subscription()).into_iter(), self.feed.0.clone());
        for future in futures {
            self.runtime.spawn(future);
        }
        let messages = self.runtime.block_on(async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let mut messages = Vec::new();
            while let Some(Some(message)) = self.feed.1.next().now_or_never() {
                messages.push(message);
            }
            messages
        });
        messages.into_iter().for_each(|message| self.send(message));
    }

    /// Pumps feeds and tasks until `done` holds.
    pub fn until(&mut self, what: &str, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while !done(&self.app) {
            assert!(Instant::now() < deadline, "timed out waiting until {what}");
            self.pump_feeds();
        }
    }

    /// Starts a session with `text` and waits for its turn to end.
    pub fn session(&mut self, text: &str) -> String {
        self.prompt(text);
        self.until("the session ran", |app| app.ctx.sessions.active().is_some_and(|session| !session.is_busy()));
        self.active().expect("an active session").to_owned()
    }

    pub fn active(&self) -> Option<&str> {
        self.app.ctx.sessions.active_id().map(|id| id.as_str())
    }

    pub fn notice(&self) -> &str {
        self.app.ctx.notice().unwrap_or_default()
    }

    /// The titles of the palette's rows.
    pub fn palette_titles(&mut self) -> Vec<String> {
        self.send(Message::Palette(palette::Message::Open));
        let titles = self.app.palette.commands().iter().map(|command| command.title.clone()).collect();
        self.send(Message::Palette(palette::Message::Close));
        titles
    }
}

fn harness_id(harness: &FakeHarness) -> &'static str {
    use loa_contracts::harness::Adapter;
    harness.harness()
}

/// Two apps on one multi-threaded runtime whose nodes listen on loopback,
/// so both progress while either is driven.
pub(crate) struct TwoNodes {
    pub owner: TestApp,
    pub guest: TestApp,
}

impl TwoNodes {
    pub fn new() -> Self {
        let runtime = Arc::new(tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("a runtime"));
        let open = |name: &str| {
            let mut test = TestApp::on(Arc::clone(&runtime), FakeHarness::new("omp"), name, |_| {});
            // `[network] port` can't be 0: the node opens again on a free port.
            let mut network = test.app.ctx.services.storage.config.load().expect("the config").network;
            network.port = 0;
            let config = crate::areas::peers::node_config(&test.app.ctx, &network);
            let (peers, start) = crate::areas::peers::State::with_config(&mut test.app.ctx, config);
            *test.area_mut::<crate::areas::peers::State>() = peers;
            test.run(start.map(Message::area));
            test
        };
        Self { owner: open("owner"), guest: open("guest-node") }
    }
}

/// Refuses everything that would leave the test: model calls, browsers,
/// environment keys; accepts every settings file.
struct Offline;

#[async_trait::async_trait]
impl Summarizer for Offline {
    async fn summarize(&self, _: &str, _: &str) -> Result<String, String> {
        Err("no model calls in tests".to_owned())
    }
}

impl Summarizers for Offline {
    fn on(&self, _: Option<&str>) -> (Arc<dyn Summarizer>, Option<String>) {
        (Arc::new(Offline), None)
    }
}

#[async_trait::async_trait]
impl SettingsValidator for Offline {
    async fn check_config(&self, _: &str, _: &[(String, serde_json::Value)]) -> CheckResult<()> {
        Ok(())
    }

    async fn check_models(&self, _: &str) -> CheckResult<()> {
        Ok(())
    }
}

impl UrlOpener for Offline {
    fn open_url(&self, url: &str) -> Result<(), String> {
        Err(format!("no browser in tests: {url}"))
    }
}

impl EnvironmentPresence for Offline {
    fn contains(&self, _: &str) -> bool {
        false
    }
}

#[async_trait::async_trait]
impl GitProbe for Offline {
    fn branch(&self, _: &Path) -> Option<String> {
        None
    }

    async fn diff(&self, _: &Path) -> Option<String> {
        None
    }
}

#[async_trait::async_trait]
impl FolderPicker for Offline {
    async fn pick(&self, _: &Path) -> Result<Option<PathBuf>, String> {
        Err("no folder dialog in tests".to_owned())
    }
}

/// One account with a usable saved reset; records the resets spent.
#[derive(Default)]
pub(crate) struct FakeUsage {
    pub spent: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl UsageSource for FakeUsage {
    async fn fetch(&self) -> Result<Usage, String> {
        let report = r#"{"provider": "anthropic", "metadata": {"orgName": "Personal", "credentialId": "c1"}, "resetCredits": {"availableCount": 2, "redeemableCount": 1},
            "limits": [{"label": "Claude 7 Day", "window": {"id": "7d", "resetsAt": 9000}, "amount": {"usedFraction": 0.6}}]}"#;
        parse(&format!(r#"{{"generatedAt": 1, "reports": [{report}]}}"#))
    }

    async fn spend(&self, target: &str) -> Result<(), String> {
        self.spent.locked().push(target.to_owned());
        Ok(())
    }
}
