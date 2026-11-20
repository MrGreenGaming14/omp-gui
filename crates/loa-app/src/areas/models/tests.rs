//! The models area driven through `App::update`: a fake harness for the
//! session side and a backend over a fake omp RPC and a temp agent dir.

use std::sync::Arc;
use std::time::Duration;

use iced::futures::StreamExt;
use iced_runtime::Action as RuntimeAction;
use loa_adapter::{Adapter, AdapterError, Answer, Capabilities, HarnessSession, Prompt, Registry, SessionState};
use loa_bus::Policy;
use loa_core::{ElementKind, NodeId, Value, now_ms};
use loa_identity::NodeIdentity;
use loa_models::harness::codex::Codex;
use loa_models::keys::EnvironmentPresence;
use loa_models::login::UrlOpener;
use loa_models::omp::OmpFiles;
use loa_models::process::TokioProcessRunner;
use loa_models::rpc::{self, LoginProvider, LoginUiRequest, OmpRpc, RpcError, UiResponse};
use loa_models::validate::SettingsValidator;
use loa_ui_models::backend::Parts;
use loa_ui_models::signin::{Auth, LoginState};
use tokio::sync::mpsc::UnboundedSender;

use super::*;
use crate::app::{App, Message as AppMessage};
use crate::services::{Dirs, Harnesses, Node, Services};
use crate::session::Feed;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A harness listing `models`, recording the config of each start and the
/// model and effort changes on `<session>` the way the real adapters do.
struct FakeAdapter {
    id: &'static str,
    models: Vec<HarnessModel>,
    efforts: bool,
    configs: Arc<Mutex<Vec<SessionConfig>>>,
}

impl FakeAdapter {
    fn new(id: &'static str, models: Vec<HarnessModel>, efforts: bool) -> Self {
        Self { id, models, efforts, configs: Arc::default() }
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
        let mut attrs: Vec<(&str, Value)> = vec![
            ("id", config.session_id.clone().into()),
            ("node", config.node.clone().into()),
            ("harness", self.id.into()),
            ("created", Value::Int(now_ms())),
            (attr::MODEL, config.model.clone().unwrap_or_else(|| self.models[0].id.clone()).into()),
        ];
        if let Some(role) = &config.model_role {
            attrs.push((attr::MODEL_ROLE, role.as_str().into()));
        }
        if let Some(effort) = &config.effort {
            attrs.push((attr::EFFORT, effort.as_str().into()));
        }
        lock(&self.configs).push(config);
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?.node;
        Ok(Box::new(FakeSession { id: self.id, sink, root, models: self.models.clone(), efforts: self.efforts }))
    }

    async fn resume(&self, _: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        let root = dom.root().map(|root| root.id).ok_or_else(|| AdapterError::Format("empty DOM".into()))?;
        Ok(Box::new(FakeSession { id: self.id, sink, root, models: self.models.clone(), efforts: self.efforts }))
    }
}

struct FakeSession {
    id: &'static str,
    sink: Arc<dyn PatchSink>,
    root: NodeId,
    models: Vec<HarnessModel>,
    efforts: bool,
}

#[async_trait::async_trait]
impl HarnessSession for FakeSession {
    async fn send(&self, _: Prompt) -> loa_adapter::Result<()> {
        Ok(())
    }

    async fn interrupt(&self) -> loa_adapter::Result<()> {
        Ok(())
    }

    async fn answer(&self, decision: NodeId, _: Answer) -> loa_adapter::Result<()> {
        Err(AdapterError::UnknownDecision(decision))
    }

    async fn set_model(&self, selection: ModelSelection) -> loa_adapter::Result<()> {
        if selection.role.is_some() {
            return Err(AdapterError::Unsupported { harness: self.id, what: "model roles" });
        }
        if !self.models.iter().any(|model| model.id == selection.model) {
            return Err(AdapterError::Protocol(format!("unknown model {}", selection.model)));
        }
        self.sink.commit(Draft::set(self.root, attr::MODEL, selection.model))?;
        Ok(())
    }

    async fn models(&self) -> loa_adapter::Result<Vec<HarnessModel>> {
        Ok(self.models.clone())
    }

    async fn set_effort(&self, effort: String) -> loa_adapter::Result<()> {
        if !self.efforts {
            return Err(AdapterError::Unsupported { harness: self.id, what: "set_effort" });
        }
        self.sink.commit(Draft::set(self.root, attr::EFFORT, effort))?;
        Ok(())
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

/// omp's RPC: two models, an Anthropic login that is signed in and an
/// OpenAI one that signs in after sending the browser URL.
#[derive(Default)]
struct FakeRpc {
    signed_in: Mutex<bool>,
}

#[async_trait::async_trait]
impl OmpRpc for FakeRpc {
    async fn get_login_providers(&self) -> Result<Vec<LoginProvider>, RpcError> {
        let openai = *lock(&self.signed_in);
        Ok(vec![
            LoginProvider { id: "anthropic".into(), name: "Anthropic".into(), available: true, authenticated: true },
            LoginProvider { id: "openai-codex".into(), name: "ChatGPT".into(), available: true, authenticated: openai },
        ])
    }

    async fn login(&self, provider_id: &str, ui: UnboundedSender<LoginUiRequest>) -> Result<String, RpcError> {
        let url = LoginUiRequest::OpenUrl { id: "u1".into(), url: "https://auth.example/authorize".into(), launch_url: None, instructions: None };
        ui.send(url).map_err(|error| RpcError::Transport(error.to_string()))?;
        // The user finishes in the browser.
        tokio::time::sleep(Duration::from_millis(50)).await;
        *lock(&self.signed_in) = true;
        Ok(provider_id.to_owned())
    }

    async fn respond_ui(&self, _: UiResponse) -> Result<(), RpcError> {
        Ok(())
    }

    async fn get_available_models(&self) -> Result<Vec<rpc::ModelInfo>, RpcError> {
        let model = |provider: &str, id: &str, name: &str| rpc::ModelInfo {
            id: id.into(),
            name: name.into(),
            provider: provider.into(),
            api: String::new(),
            reasoning: true,
            input: Vec::new(),
            cost: rpc::ModelCost::default(),
            context_window: None,
            max_tokens: None,
            thinking: None,
            extra: Default::default(),
        };
        Ok(vec![model("anthropic", "claude-x", "Claude X"), model("openai-codex", "gpt-y", "GPT Y")])
    }

    async fn set_model(&self, _: &str, _: &str) -> Result<rpc::ModelInfo, RpcError> {
        Err(RpcError::Transport("the backend never switches a session model".into()))
    }
}

/// Accepts every file unless it contains `reject-me`, as omp rejects a bad
/// role.
struct Validator;

#[async_trait::async_trait]
impl SettingsValidator for Validator {
    async fn check_config(&self, text: &str, _: &[(String, serde_json::Value)]) -> loa_models::Result<()> {
        if text.contains("reject-me") {
            return Err(loa_models::Error::Rejected { what: "config.yml".into(), message: "unknown role reject-me".into() });
        }
        Ok(())
    }

    async fn check_models(&self, _: &str) -> loa_models::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Opener(Mutex<Vec<String>>);

impl UrlOpener for Opener {
    fn open_url(&self, url: &str) -> Result<(), String> {
        lock(&self.0).push(url.to_owned());
        Ok(())
    }
}

struct NoKeys;

impl EnvironmentPresence for NoKeys {
    fn contains(&self, _: &str) -> bool {
        false
    }
}

fn omp_models() -> Vec<HarnessModel> {
    let model = |id: &str, name: &str, provider: &str, efforts: &[&str]| HarnessModel {
        id: id.into(),
        name: name.into(),
        provider: provider.into(),
        efforts: efforts.iter().map(|effort| (*effort).to_owned()).collect(),
    };
    vec![model("anthropic/claude-x", "Claude X", "anthropic", &["low", "high"]), model("openai-codex/gpt-y", "GPT Y", "openai-codex", &[])]
}

struct Fixture {
    dir: tempfile::TempDir,
    app: App,
    configs: Arc<Mutex<Vec<SessionConfig>>>,
    opener: Arc<Opener>,
}

/// The app with one fake harness and the models backend over a fake omp and
/// the temp agent dir `<dir>/agent` (config.yml assigns `default`).
fn fixture(adapter: FakeAdapter) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("config.yml"), "# mine\nmodelRoles:\n  default: anthropic/claude-x\n").unwrap();
    std::fs::write(agent.join("models.yml"), "").unwrap();
    let configs = Arc::clone(&adapter.configs);
    let id = adapter.id;
    let mut harnesses = Harnesses { registry: Registry::new(), list: Vec::new() };
    harnesses.add(Arc::new(adapter), id, true, None, None);
    let node = Node::new(NodeIdentity::from_secret_bytes(&[7; 32]), "testhost".into());
    let services = Services::new(node, Dirs::new(dir.path().join("loa")), harnesses, dir.path().to_owned()).unwrap();
    let (mut app, _) = App::build(services);
    let opener = Arc::new(Opener::default());
    app.areas.models.backend = Backend::new(Parts {
        rpc: Arc::new(FakeRpc::default()),
        files: OmpFiles::new(&agent),
        validator: Arc::new(Validator),
        opener: opener.clone(),
        claude: None,
        codex: Some(Codex::new(dir.path().join("no-codex-here"), dir.path().join("codex.toml"), Arc::new(TokioProcessRunner))),
        environment: Arc::new(NoKeys),
    });
    Fixture { dir, app, configs, opener }
}

/// Runs `task` the way the iced runtime would, feeding every message it
/// produces back through `App::update`, until no task is left.
fn run(app: &mut App, task: Task<AppMessage>) {
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

fn update(app: &mut App, message: AppMessage) {
    let task = app.update(message);
    run(app, task);
}

fn models(app: &mut App, message: Message) {
    update(app, AppMessage::Models(message));
}

/// Delivers the writers' changes to the UI copies, as the session feeds do.
fn sync(app: &mut App) {
    let open: Vec<_> = app.ctx.sessions.iter().map(|session| (session.id().clone(), Arc::clone(session.writer()))).collect();
    for (id, writer) in open {
        let (dom, _) = writer.subscribe(Policy::every_patch(1));
        update(app, AppMessage::Session(id, Feed::Synced(Arc::new(dom))));
    }
}

/// Runs the palette command titled `title` the way the user does.
fn palette(app: &mut App, title: &str) {
    update(app, AppMessage::Palette(palette::Message::Open));
    update(app, AppMessage::Palette(palette::Message::QueryChanged(title.into())));
    update(app, AppMessage::Palette(palette::Message::Submit));
}

fn session_attr(app: &App, name: &str) -> Option<String> {
    app.ctx.sessions.active()?.dom().root()?.str_attr(name).map(str::to_owned)
}

fn composer_models(app: &App) -> (Vec<(String, Option<String>)>, Option<String>) {
    let (options, current) = app.ctx.composer.models();
    (options.iter().map(|option| (option.id.clone(), option.role.clone())).collect(), current.map(|option| option.id.clone()))
}

fn picker(app: &App) -> &picker::State {
    match &app.areas.models.picker.1 {
        Load::Ready(picker) => &picker.state,
        Load::Loading => panic!("the picker is still loading"),
        Load::Failed(error) => panic!("the picker failed: {error}"),
    }
}

#[test]
fn the_composer_lists_the_harness_models_and_switches_model_and_effort() {
    let mut f = fixture(FakeAdapter::new("omp", omp_models(), true));
    update(&mut f.app, AppMessage::NewSession);
    // The listing came from the harness, the `default` role from omp's config.
    assert_eq!(
        composer_models(&f.app),
        (
            vec![("anthropic/claude-x".into(), Some("default".into())), ("openai-codex/gpt-y".into(), None)],
            Some("anthropic/claude-x".into())
        )
    );
    assert_eq!(f.app.ctx.composer.efforts(), (&["low".to_owned(), "high".to_owned()][..], None));

    let gpt = f.app.ctx.composer.models().0[1].clone();
    update(&mut f.app, AppMessage::Composer(composer::Message::ModelPicked(gpt)));
    sync(&mut f.app);
    assert_eq!(session_attr(&f.app, attr::MODEL).as_deref(), Some("openai-codex/gpt-y"));
    assert_eq!(composer_models(&f.app).1.as_deref(), Some("openai-codex/gpt-y"));
    // GPT Y has no effort setting.
    assert_eq!(f.app.ctx.composer.efforts().0, &[] as &[String]);

    // Picking the role's model records that the session follows the role.
    let claude = f.app.ctx.composer.models().0[0].clone();
    update(&mut f.app, AppMessage::Composer(composer::Message::ModelPicked(claude)));
    sync(&mut f.app);
    assert_eq!(session_attr(&f.app, attr::MODEL).as_deref(), Some("anthropic/claude-x"));
    assert_eq!(session_attr(&f.app, attr::MODEL_ROLE).as_deref(), Some("default"));

    update(&mut f.app, AppMessage::Composer(composer::Message::EffortPicked("high".into())));
    sync(&mut f.app);
    assert_eq!(session_attr(&f.app, attr::EFFORT).as_deref(), Some("high"));
    assert_eq!(f.app.ctx.composer.efforts().1, Some("high"));
}

#[test]
fn an_effort_the_harness_cannot_change_mid_session_starts_the_next_session() {
    let mut f = fixture(FakeAdapter::new("codex", omp_models(), false));
    update(&mut f.app, AppMessage::NewSession);
    update(&mut f.app, AppMessage::Composer(composer::Message::EffortPicked("high".into())));
    assert!(f.app.ctx.notice().is_some_and(|notice| notice.contains("new sessions start with high effort")), "{:?}", f.app.ctx.notice());
    update(&mut f.app, AppMessage::NewSession);
    assert_eq!(lock(&f.configs).last().and_then(|config| config.effort.clone()).as_deref(), Some("high"));
    sync(&mut f.app);
    assert_eq!(session_attr(&f.app, attr::EFFORT).as_deref(), Some("high"));
    // Only the next session.
    update(&mut f.app, AppMessage::NewSession);
    assert_eq!(lock(&f.configs).last().and_then(|config| config.effort.clone()), None);
}

#[test]
fn the_model_screen_opens_from_the_palette_and_the_composer_and_switches_the_session() {
    let mut f = fixture(FakeAdapter::new("omp", omp_models(), true));
    update(&mut f.app, AppMessage::NewSession);

    palette(&mut f.app, "Switch model");
    assert_eq!(f.app.ctx.screen(), Some(AreaId::Models));
    assert_eq!(f.app.areas.models.page, Page::Model);
    let state = picker(&f.app);
    assert_eq!(state.rows(), [picker::Row::Role(0), picker::Row::Model(0), picker::Row::Model(1)]);

    // Search and pick GPT Y.
    models(&mut f.app, Message::Picker(picker::Message::Query("gpt".into())));
    models(&mut f.app, Message::Picker(picker::Message::Submit));
    assert_eq!(f.app.ctx.screen(), None);
    sync(&mut f.app);
    assert_eq!(session_attr(&f.app, attr::MODEL).as_deref(), Some("openai-codex/gpt-y"));
    assert_eq!(session_attr(&f.app, attr::MODEL_ROLE), None);

    // The composer's "All models…" opens the same page; picking the role row
    // switches to its model and records the role.
    update(&mut f.app, AppMessage::Composer(composer::Message::ModelActionPicked("all".into())));
    assert_eq!(f.app.ctx.screen(), Some(AreaId::Models));
    assert_eq!(picker(&f.app).selected(), Some(2), "the current model is selected first");
    models(&mut f.app, Message::Picker(picker::Message::Pick(0)));
    sync(&mut f.app);
    assert_eq!(session_attr(&f.app, attr::MODEL).as_deref(), Some("anthropic/claude-x"));
    assert_eq!(session_attr(&f.app, attr::MODEL_ROLE).as_deref(), Some("default"));
}

#[test]
fn a_model_picked_without_a_session_starts_the_next_one() {
    let mut f = fixture(FakeAdapter::new("omp", omp_models(), true));
    palette(&mut f.app, "Switch model");
    models(&mut f.app, Message::Picker(picker::Message::Query("gpt".into())));
    models(&mut f.app, Message::Picker(picker::Message::Submit));
    update(&mut f.app, AppMessage::NewSession);
    assert_eq!(lock(&f.configs)[0].model.as_deref(), Some("openai-codex/gpt-y"));
    sync(&mut f.app);
    assert_eq!(session_attr(&f.app, attr::MODEL).as_deref(), Some("openai-codex/gpt-y"));
}

#[test]
fn the_sign_in_screen_lists_providers_and_runs_an_omp_login() {
    let mut f = fixture(FakeAdapter::new("omp", omp_models(), true));
    palette(&mut f.app, "Sign in");
    assert_eq!(f.app.ctx.screen(), Some(AreaId::Models));
    let Load::Ready(state) = &f.app.areas.models.signin else { panic!("sign in not loaded") };
    let rows: Vec<(&str, &str, bool)> = state
        .providers()
        .iter()
        .map(|row| (row.harness_name.as_str(), row.key.provider.as_str(), matches!(row.auth, Auth::Login { signed_in: true, .. })))
        .collect();
    assert_eq!(rows, [("omp", "anthropic", true), ("omp", "openai-codex", false), ("Codex", "codex", false)]);
    assert!(matches!(state.providers()[2].auth, Auth::Unavailable { .. }), "{:?}", state.providers()[2]);

    let key = ProviderKey::new("omp", "openai-codex");
    models(&mut f.app, Message::SignIn(signin::Message::SignIn(key.clone())));
    assert_eq!(*lock(&f.opener.0), ["https://auth.example/authorize"], "the browser opened on omp's URL");
    let Load::Ready(state) = &f.app.areas.models.signin else { unreachable!() };
    assert!(matches!(state.login_state(&key), LoginState::Succeeded { .. }), "{:?}", state.login_state(&key));
    assert!(matches!(state.providers()[1].auth, Auth::Login { signed_in: true, .. }), "rows were read again");
}

#[test]
fn the_settings_screen_saves_omp_config_through_validation() {
    let mut f = fixture(FakeAdapter::new("omp", omp_models(), true));
    update(&mut f.app, AppMessage::NewSession);
    update(&mut f.app, AppMessage::Composer(composer::Message::ModelActionPicked("settings".into())));
    assert_eq!(f.app.areas.models.page, Page::Settings);
    assert!(matches!(f.app.areas.models.settings, Load::Ready(_)));

    models(&mut f.app, Message::Settings(settings::Message::ChainInput(0, "openai-codex/gpt-y".into())));
    models(&mut f.app, Message::Settings(settings::Message::AddTypedChainEntry(0)));
    models(&mut f.app, Message::Settings(settings::Message::Save));
    let config = f.dir.path().join("agent/config.yml");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.starts_with("# mine\n"), "{text}");
    assert!(text.contains("fallbackChains:\n    default:\n      - openai-codex/gpt-y\n"), "{text}");
    assert_eq!(f.app.ctx.notice(), Some("Model settings saved"));
    let Load::Ready(state) = &f.app.areas.models.settings else { unreachable!() };
    assert!(!state.is_dirty());

    // omp rejects the new role: the errors show and nothing is written.
    models(&mut f.app, Message::Settings(settings::Message::NewRoleInput("reject-me".into())));
    models(&mut f.app, Message::Settings(settings::Message::AddRole));
    models(&mut f.app, Message::Settings(settings::Message::AddChainEntry(1, "anthropic/claude-x".into())));
    models(&mut f.app, Message::Settings(settings::Message::Save));
    let Load::Ready(state) = &f.app.areas.models.settings else { unreachable!() };
    assert_eq!(state.errors()[0].location, "config.yml");
    assert!(state.is_dirty());
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
}

/// Drives an app over the machine's real harnesses the way the iced runtime
/// does: task outputs and session feeds are fed back through `App::update`.
struct Live {
    runtime: tokio::runtime::Runtime,
    sender: UnboundedSender<AppMessage>,
    outputs: tokio::sync::mpsc::UnboundedReceiver<AppMessage>,
    feeds: Vec<(SessionId, loa_bus::Subscription)>,
}

impl Live {
    fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let (sender, outputs) = tokio::sync::mpsc::unbounded_channel();
        Self { runtime, sender, outputs, feeds: Vec::new() }
    }

    fn update(&mut self, app: &mut App, message: AppMessage) {
        let _runtime = self.runtime.enter();
        let Some(mut stream) = iced_runtime::task::into_stream(app.update(message)) else { return };
        let sender = self.sender.clone();
        self.runtime.spawn(async move {
            while let Some(action) = stream.next().await {
                if let RuntimeAction::Output(message) = action {
                    let _ = sender.send(message);
                }
            }
        });
    }

    /// Delivers task outputs and session feeds until `done` holds.
    fn until(&mut self, app: &mut App, what: &str, timeout: Duration, mut done: impl FnMut(&App) -> bool) {
        let deadline = std::time::Instant::now() + timeout;
        loop {
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
                self.update(app, AppMessage::Session(id, Feed::Synced(Arc::new(dom))));
            }
            let mut idle = true;
            while let Ok(message) = self.outputs.try_recv() {
                idle = false;
                self.update(app, message);
            }
            for index in 0..self.feeds.len() {
                while let Ok(event) = self.feeds[index].1.try_recv() {
                    idle = false;
                    let id = self.feeds[index].0.clone();
                    self.update(app, AppMessage::Session(id, Feed::Events(vec![event])));
                }
            }
            if done(app) {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}; notice: {:?}", app.ctx.notice());
            if idle {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn palette(&mut self, app: &mut App, title: &str) {
        self.update(app, AppMessage::Palette(palette::Message::Open));
        self.update(app, AppMessage::Palette(palette::Message::QueryChanged(title.into())));
        self.update(app, AppMessage::Palette(palette::Message::Submit));
    }
}

fn live_model() -> String {
    std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".to_owned())
}

/// An app over a temp data dir and the machine's real harnesses (omp probed
/// ready); the models area talks to the installed omp.
fn live_app(dir: &std::path::Path, live: &mut Live) -> App {
    let node = Node::new(NodeIdentity::from_secret_bytes(&[13; 32]), "live".into());
    let dirs = Dirs::new(dir.join("loa"));
    let harnesses = Harnesses::detect(&node.key, &dirs.omp);
    let workspace = dir.join("work");
    std::fs::create_dir_all(&workspace).unwrap();
    let (mut app, _) = App::build(Services::new(node, dirs, harnesses, workspace).unwrap());
    live.update(&mut app, AppMessage::Harness(crate::areas::harness::Message::Recheck));
    live.until(&mut app, "the omp probe", Duration::from_secs(30), |app| {
        app.ctx.services.harnesses.get(OMP).is_some_and(|omp| omp.availability.is_some())
    });
    assert!(app.ctx.services.harnesses.get(OMP).unwrap().option.available, "omp must be installed");
    app
}

fn loaded<T>(load: &Load<T>) -> bool {
    match load {
        Load::Loading => false,
        Load::Failed(error) => panic!("{error}"),
        Load::Ready(_) => true,
    }
}

/// Reads the sign in rows and the model settings of the user's omp (read
/// only), switches a live omp session's model from the composer and from the
/// model screen, then runs a turn on the picked model.
#[test]
#[ignore = "runs the installed omp with a real model call (LOA_LIVE_MODEL, default anthropic/claude-haiku-5-5)"]
fn live_omp_feeds_the_screens_and_switches_the_session_model() {
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::new();
    let mut app = live_app(dir.path(), &mut live);

    live.palette(&mut app, "Sign in");
    live.until(&mut app, "the sign in rows", Duration::from_secs(60), |app| loaded(&app.areas.models.signin));
    let Load::Ready(signin) = &app.areas.models.signin else { unreachable!() };
    let rows: Vec<_> = signin.providers().iter().map(|row| (row.harness_name.clone(), row.key.provider.clone())).collect();
    assert!(rows.contains(&("omp".to_owned(), "anthropic".to_owned())), "{rows:?}");
    assert!(rows.iter().any(|(harness, _)| harness == "Claude Code") && rows.iter().any(|(harness, _)| harness == "Codex"), "{rows:?}");

    live.palette(&mut app, "Model settings");
    live.until(&mut app, "the model settings", Duration::from_secs(60), |app| loaded(&app.areas.models.settings));
    assert_eq!(app.areas.models.page, Page::Settings);

    live.update(&mut app, AppMessage::NewSession);
    live.until(&mut app, "omp to start", Duration::from_secs(90), |app| {
        app.ctx.sessions.active().is_some_and(|session| session.harness().is_some())
    });
    let wanted = live_model();
    live.until(&mut app, "the composer to list omp's models", Duration::from_secs(60), |app| {
        app.ctx.composer.models().0.iter().any(|option| option.id == wanted)
    });

    // Away from the wanted model through the composer…
    let other = app.ctx.composer.models().0.iter().find(|option| option.id != wanted).cloned().expect("a second model");
    live.update(&mut app, AppMessage::Composer(composer::Message::ModelPicked(other.clone())));
    live.until(&mut app, "the composer switch", Duration::from_secs(30), |app| session_attr(app, attr::MODEL) == Some(other.id.clone()));
    assert_eq!(composer_models(&app).1, Some(other.id.clone()));

    // …and back through the model screen's search.
    live.palette(&mut app, "Switch model");
    live.until(&mut app, "omp's model catalog", Duration::from_secs(60), |app| loaded(&app.areas.models.picker.1));
    let (_, id) = wanted.split_once('/').unwrap();
    live.update(&mut app, AppMessage::Models(Message::Picker(picker::Message::Query(id.into()))));
    live.update(&mut app, AppMessage::Models(Message::Picker(picker::Message::Submit)));
    live.until(&mut app, "the screen switch", Duration::from_secs(30), |app| session_attr(app, attr::MODEL) == Some(wanted.clone()));
    assert_eq!(app.ctx.screen(), None);
    assert_eq!(composer_models(&app).1, Some(wanted.clone()));

    for c in "Reply with just the word KIWI. Use no tools.".chars() {
        let insert = iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Insert(c));
        live.update(&mut app, AppMessage::Composer(composer::Message::Edit(insert)));
    }
    live.update(&mut app, AppMessage::Composer(composer::Message::Submit));
    let turn_status = |app: &App| {
        let dom = app.ctx.sessions.active()?.dom();
        crate::session::dom_turns(dom).next()?.str_attr(attr::STATUS).and_then(loa_core::TurnStatus::parse)
    };
    live.until(&mut app, "the turn to end", Duration::from_secs(120), |app| {
        matches!(turn_status(app), Some(loa_core::TurnStatus::Done | loa_core::TurnStatus::Error | loa_core::TurnStatus::Cancelled))
    });
    assert_eq!(turn_status(&app), Some(loa_core::TurnStatus::Done), "notice: {:?}", app.ctx.notice());
    let dom = app.ctx.sessions.active().unwrap().dom();
    let reply: String = dom
        .root()
        .map(|root| {
            dom.descendants(root.id)
                .filter(|node| node.kind == ElementKind::Message && node.str_attr(attr::ROLE) == Some("assistant"))
                .map(|node| node.text.clone())
                .collect()
        })
        .unwrap_or_default();
    assert!(reply.contains("KIWI"), "{reply}");
    assert_eq!(session_attr(&app, attr::MODEL), Some(wanted), "the turn ran on the picked model");

    let harness = app.ctx.sessions.active().and_then(|session| session.harness().cloned()).unwrap();
    live.runtime.block_on(harness.shutdown()).unwrap();
}

/// The settings screen saves through the real omp's validation, in a temp
/// agent dir (`PI_CODING_AGENT_DIR`); the saved custom provider's model then
/// shows in the model screen.
#[test]
#[ignore = "runs the installed omp (no model call)"]
fn live_settings_saved_in_the_app_are_validated_by_real_omp() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("config.yml"), "# hand written\n").unwrap();
    let mut live = Live::new();
    let mut app = live_app(dir.path(), &mut live);
    let executable = app.ctx.services.harnesses.get(OMP).and_then(|omp| omp.executable.clone()).unwrap_or_else(|| "omp".into());
    let launch = OmpLaunch { executable, cwd: Some(dir.path().to_owned()), env: Vec::new() }.env("PI_CODING_AGENT_DIR", &agent);
    app.areas.models.backend = Backend::system(launch);

    live.palette(&mut app, "Model settings");
    live.until(&mut app, "the model settings", Duration::from_secs(60), |app| loaded(&app.areas.models.settings));
    let edits = [
        settings::Message::NewRoleInput("smol".into()),
        settings::Message::AddRole,
        settings::Message::ChainInput(0, format!("{}:low", live_model())),
        settings::Message::AddTypedChainEntry(0),
        settings::Message::AddCustomProvider,
        settings::Message::CustomProviderEdited(0, settings::ProviderEdit::Id("local-gw".into())),
        settings::Message::CustomProviderEdited(0, settings::ProviderEdit::BaseUrl("http://127.0.0.1:9/v1".into())),
        settings::Message::CustomProviderEdited(0, settings::ProviderEdit::Api("openai-completions".into())),
        settings::Message::CustomProviderEdited(0, settings::ProviderEdit::ApiKeyEnv("LOCAL_GW_KEY".into())),
        settings::Message::CustomProviderEdited(0, settings::ProviderEdit::Models("qwen-coder".into())),
        settings::Message::Save,
    ];
    for edit in edits {
        live.update(&mut app, AppMessage::Models(Message::Settings(edit)));
    }
    live.until(&mut app, "the save", Duration::from_secs(60), |app| {
        let Load::Ready(state) = &app.areas.models.settings else { return false };
        assert!(state.local_errors().is_empty() && state.errors().is_empty(), "{:?} {:?}", state.local_errors(), state.errors());
        !state.is_dirty()
    });
    assert_eq!(app.ctx.notice(), Some("Model settings saved"));
    let config = std::fs::read_to_string(agent.join("config.yml")).unwrap();
    assert!(config.starts_with("# hand written\n"), "{config}");
    assert!(config.contains(&format!("smol: {}:low", live_model())), "{config}");
    assert!(std::fs::read_to_string(agent.join("models.yml")).unwrap().contains("local-gw"));

    live.palette(&mut app, "Switch model");
    live.until(&mut app, "omp's model catalog", Duration::from_secs(60), |app| loaded(&app.areas.models.picker.1));
    let Load::Ready(picker) = &app.areas.models.picker.1 else { unreachable!() };
    assert!(picker.ids.contains_key(&ModelRef::new("local-gw", "qwen-coder")), "{:?}", picker.ids.keys());
}
