//! The service behind the model screens: everything they need from omp and
//! the other harnesses, as async calls returning this crate's screen data.
//!
//! [`Backend::system`] wires the real parts: a dedicated omp RPC process
//! ([`OmpService`]), omp's own settings files, validation by omp itself, the
//! platform browser opener and the Claude Code and Codex CLIs. The app keeps
//! one `Backend` (it is cheap to clone) and turns the screens' events into
//! these calls.
//!
//! The current session's model is not switched here: that goes through the
//! session's own harness connection (`HarnessSession::set_model`), which
//! records the new `model` on the `<session>`.

use loa_util::LockExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use iced::futures::{SinkExt, Stream};
use loa_models::config::{OmpConfig, Selector};
use loa_models::harness::cli_login::{CliLoginOutcome, CliLoginUpdate, run_cli_login};
use loa_models::harness::claude::{ClaudeCode, ClaudeEffort};
use loa_models::harness::codex::{Codex, ReasoningEffort};
use loa_models::harness::{HarnessAuth, LoginKind, SignInState};
use loa_models::keys::{self, EnvironmentPresence, KeySource, ProcessEnvironment};
use loa_models::login::{LoginControl, LoginState as FlowState, UrlOpener, run_login};
use loa_models::models_yml::ModelsYml;
use loa_models::omp::{OmpFiles, OmpLaunch, OmpService};
use loa_models::opener::SystemUrlOpener;
use loa_models::process::TokioProcessRunner;
use loa_models::providers::{SignIn, provider_rows};
use loa_models::rpc::{self, LoginProvider, OmpRpc};
use loa_models::validate::{OmpSettingsValidator, SettingsValidator};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::bridge;
use crate::catalog::ModelRef;
use crate::picker::Catalog;
use crate::settings::{HarnessModel, ModelSettings, ValidationError};
use crate::signin::{Auth, LoginUpdate, ProviderEntry, ProviderKey};

/// The harness id of omp's rows on the sign in screen.
pub const OMP: &str = "omp";

/// A harness other than omp, with its own sign in and model settings.
#[derive(Clone)]
enum Harness {
    Claude(Arc<ClaudeCode>),
    Codex(Arc<Codex>),
}

impl Harness {
    /// Row key on the sign in screen.
    fn key(&self) -> &'static str {
        match self {
            Self::Claude(_) => "claude",
            Self::Codex(_) => "codex",
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Claude(_) => "Claude Code",
            Self::Codex(_) => "Codex",
        }
    }

    fn auth(&self) -> &dyn HarnessAuth {
        match self {
            Self::Claude(claude) => claude.as_ref(),
            Self::Codex(codex) => codex.as_ref(),
        }
    }

    /// The default model and effort in the harness's own config.
    fn model_settings(&self) -> Result<HarnessModel, String> {
        let (config_path, model, effort, efforts) = match self {
            Self::Claude(claude) => {
                let s = claude.read_model_settings().map_err(|e| e.to_string())?;
                let efforts = ClaudeEffort::ALL.iter().map(|e| e.as_str().to_owned()).collect();
                (&claude.settings_path, s.model, s.effort.map(ClaudeEffort::as_str), efforts)
            }
            Self::Codex(codex) => {
                let s = codex.read_model_settings().map_err(|e| e.to_string())?;
                let efforts = CODEX_EFFORTS.iter().map(|e| e.as_str().to_owned()).collect();
                (&codex.config_path, s.model, s.reasoning_effort.map(ReasoningEffort::as_str), efforts)
            }
        };
        Ok(HarnessModel {
            harness: self.key().to_owned(),
            name: self.name().to_owned(),
            config_path: config_path.display().to_string(),
            model: model.unwrap_or_default(),
            effort: effort.unwrap_or_default().to_owned(),
            efforts,
        })
    }

    /// Checks an edit before anything is written.
    fn check(&self, after: &HarnessModel) -> Result<(), String> {
        if after.model.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!("model `{}` must have no spaces", after.model));
        }
        if !after.effort.is_empty() && !after.efforts.contains(&after.effort) {
            return Err(format!("effort `{}` is not one of {}", after.effort, after.efforts.join(", ")));
        }
        Ok(())
    }

    /// Writes the changed values to the harness's config file.
    fn write(&self, before: &HarnessModel, after: &HarnessModel) -> Result<(), String> {
        let model = (!after.model.is_empty()).then_some(after.model.as_str());
        let effort = (!after.effort.is_empty()).then_some(after.effort.as_str());
        let model_changed = before.model != after.model;
        let effort_changed = before.effort != after.effort;
        let result = match self {
            Self::Claude(claude) => (|| -> loa_models::Result<()> {
                if model_changed {
                    claude.set_model(model)?;
                }
                if effort_changed {
                    claude.set_effort(effort.map(ClaudeEffort::parse).transpose()?)?;
                }
                Ok(())
            })(),
            Self::Codex(codex) => (|| -> loa_models::Result<()> {
                if model_changed {
                    codex.set_model(model)?;
                }
                if effort_changed {
                    codex.set_reasoning_effort(effort.and_then(ReasoningEffort::parse))?;
                }
                Ok(())
            })(),
        };
        result.map_err(|e| e.to_string())
    }
}

const CODEX_EFFORTS: [ReasoningEffort; 6] = [
    ReasoningEffort::None,
    ReasoningEffort::Minimal,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
];

/// Where omp's settings files are: a fixed directory or the one omp reports.
enum FilesSource {
    Fixed(OmpFiles),
    Resolve { launch: OmpLaunch, cached: tokio::sync::OnceCell<OmpFiles> },
}

/// The parts a [`Backend`] is built from. Tests pass fakes.
pub struct Parts {
    pub rpc: Arc<dyn OmpRpc>,
    pub files: OmpFiles,
    pub validator: Arc<dyn SettingsValidator>,
    pub opener: Arc<dyn UrlOpener>,
    pub claude: Option<ClaudeCode>,
    pub codex: Option<Codex>,
    pub environment: Arc<dyn EnvironmentPresence + Send + Sync>,
}

/// The settings shown by the screen and the files they were read from, kept
/// so a save edits exactly what was shown and notices outside changes.
struct Loaded {
    config: OmpConfig,
    models: ModelsYml,
    settings: ModelSettings,
    available: Vec<rpc::ModelInfo>,
}

struct Inner {
    rpc: Arc<dyn OmpRpc>,
    files: FilesSource,
    validator: Arc<dyn SettingsValidator>,
    opener: Arc<dyn UrlOpener>,
    harnesses: Vec<Harness>,
    environment: Arc<dyn EnvironmentPresence + Send + Sync>,
    /// omp's login providers from the last listing, for display names and
    /// to start logins.
    login_providers: Mutex<Vec<LoginProvider>>,
    /// Controls of the running logins.
    logins: Mutex<HashMap<ProviderKey, UnboundedSender<LoginControl>>>,
    loaded: tokio::sync::Mutex<Option<Loaded>>,
}


/// What [`Backend::load_settings`] returns.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsLoad {
    pub settings: ModelSettings,
    /// Selectors of the available models, offered when adding chain entries
    /// and overrides.
    pub known_models: Vec<String>,
}

/// The model screens' service. Cheap to clone.
#[derive(Clone)]
pub struct Backend {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").finish_non_exhaustive()
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

fn dir_from_env(var: &str, home_dir: &str) -> PathBuf {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(home_dir)))
        .unwrap_or_else(|| PathBuf::from(home_dir))
}

impl Backend {
    /// The real backend: omp started from `launch`, its active agent
    /// directory, validation by omp, the platform browser and the `claude` and
    /// `codex` CLIs on `PATH`.
    pub fn system(launch: OmpLaunch) -> Self {
        let runner = Arc::new(TokioProcessRunner);
        let claude = ClaudeCode::new("claude", dir_from_env("CLAUDE_CONFIG_DIR", ".claude").join("settings.json"), runner.clone());
        let codex = Codex::new("codex", dir_from_env("CODEX_HOME", ".codex").join("config.toml"), runner.clone());
        Self::build(
            Arc::new(OmpService::new(launch.clone())),
            FilesSource::Resolve { launch: launch.clone(), cached: tokio::sync::OnceCell::new() },
            Arc::new(OmpSettingsValidator::new(launch.executable.clone(), runner)),
            Arc::new(SystemUrlOpener),
            vec![Harness::Claude(Arc::new(claude)), Harness::Codex(Arc::new(codex))],
            Arc::new(ProcessEnvironment),
        )
    }

    /// A backend over the given parts.
    pub fn new(parts: Parts) -> Self {
        let harnesses = parts
            .claude
            .map(|c| Harness::Claude(Arc::new(c)))
            .into_iter()
            .chain(parts.codex.map(|c| Harness::Codex(Arc::new(c))))
            .collect();
        Self::build(
            parts.rpc,
            FilesSource::Fixed(parts.files),
            parts.validator,
            parts.opener,
            harnesses,
            parts.environment,
        )
    }

    fn build(
        rpc: Arc<dyn OmpRpc>,
        files: FilesSource,
        validator: Arc<dyn SettingsValidator>,
        opener: Arc<dyn UrlOpener>,
        harnesses: Vec<Harness>,
        environment: Arc<dyn EnvironmentPresence + Send + Sync>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                rpc,
                files,
                validator,
                opener,
                harnesses,
                environment,
                login_providers: Mutex::default(),
                logins: Mutex::default(),
                loaded: tokio::sync::Mutex::new(None),
            }),
        }
    }

    /// The omp RPC the backend uses (the dedicated process for [`Backend::system`]).
    pub fn rpc(&self) -> Arc<dyn OmpRpc> {
        self.inner.rpc.clone()
    }

    /// omp's settings files.
    pub async fn files(&self) -> Result<OmpFiles, String> {
        match &self.inner.files {
            FilesSource::Fixed(files) => Ok(files.clone()),
            FilesSource::Resolve { launch, cached } => cached
                .get_or_try_init(|| OmpFiles::resolve(launch))
                .await
                .cloned()
                .map_err(|e| e.to_string()),
        }
    }

    fn provider_names(&self) -> HashMap<String, String> {
        self.inner.login_providers.locked().iter().map(|p| (p.id.clone(), p.name.clone())).collect()
    }

    async fn refresh_login_providers(&self) -> Result<Vec<LoginProvider>, String> {
        let providers = self.inner.rpc.get_login_providers().await.map_err(|e| e.to_string())?;
        *self.inner.login_providers.locked() = providers.clone();
        Ok(providers)
    }

    /// Every sign in row: omp's login providers, providers omp reads an API
    /// key for, then one row per other harness. A part that cannot be read
    /// shows as an unavailable row with the reason.
    pub async fn providers(&self) -> Vec<ProviderEntry> {
        let mut entries = Vec::new();
        match self.refresh_login_providers().await {
            Ok(providers) => entries.extend(provider_rows(providers).into_iter().map(|row| ProviderEntry {
                key: ProviderKey::new(OMP, &row.provider.id),
                name: row.provider.name.clone(),
                harness_name: OMP.to_owned(),
                auth: match row.sign_in {
                    SignIn::SignedIn => Auth::Login { signed_in: true, account: None },
                    SignIn::SignedOut => Auth::Login { signed_in: false, account: None },
                    SignIn::Unavailable => Auth::Unavailable {
                        reason: "omp offers no sign in for this provider now".into(),
                    },
                },
            })),
            Err(reason) => entries.push(ProviderEntry {
                key: ProviderKey::new(OMP, OMP),
                name: "omp".into(),
                harness_name: OMP.to_owned(),
                auth: Auth::Unavailable { reason },
            }),
        }
        entries.extend(self.api_key_rows().await);
        for harness in &self.inner.harnesses {
            let auth = match harness.auth().sign_in_state().await {
                Ok(SignInState::SignedIn { .. }) => Auth::Login { signed_in: true, account: None },
                Ok(SignInState::SignedOut) => Auth::Login { signed_in: false, account: None },
                Ok(SignInState::Unknown { reason }) => Auth::Unavailable { reason },
                Err(e) => Auth::Unavailable { reason: e.to_string() },
            };
            entries.push(ProviderEntry {
                key: ProviderKey::new(harness.key(), harness.key()),
                name: harness.name().into(),
                harness_name: harness.name().into(),
                auth,
            });
        }
        entries
    }

    /// API key providers: the custom providers of `models.yml` and the built
    /// in providers whose key variable is set. Names only, never key values.
    async fn api_key_rows(&self) -> Vec<ProviderEntry> {
        let Ok(files) = self.files().await else { return Vec::new() };
        let Ok(models) = ModelsYml::load(&files.models()) else { return Vec::new() };
        let env = self.inner.environment.as_ref();
        let oauth: Vec<String> = self.inner.login_providers.locked().iter().map(|p| p.id.clone()).collect();
        let mut rows = Vec::new();
        let mut add = |provider: &str, source: KeySource| {
            let names = keys::builtin_environment_names(provider);
            let configured_via = match &source {
                KeySource::ConfigEnvironment(name) => Some(format!("{name} (models.yml)")),
                KeySource::SecretCommand => Some("a secret command (models.yml)".to_owned()),
                KeySource::ConfigEntry => Some("models.yml".to_owned()),
                KeySource::Environment(name) => Some(name.clone()),
                KeySource::NotConfigured => None,
            };
            rows.push(ProviderEntry {
                key: ProviderKey::new(OMP, provider),
                name: provider.to_owned(),
                harness_name: "omp API keys".to_owned(),
                auth: Auth::ApiKey { configured_via, instructions: keys::key_instructions(provider, &source, names) },
            });
        };
        for entry in models.providers().into_iter().filter(|e| e.api_key.is_some() || e.base_url.is_some()) {
            if oauth.contains(&entry.id) {
                continue;
            }
            let names = keys::builtin_environment_names(&entry.id);
            add(&entry.id, keys::credential_source(&models, &entry.id, names, env));
        }
        const BUILTIN: [&str; 14] = [
            "anthropic", "openai", "google", "groq", "openrouter", "mistral", "xai", "deepseek", "together",
            "fireworks", "cerebras", "azure", "amazon-bedrock", "google-vertex",
        ];
        for provider in BUILTIN {
            if models.provider(provider).is_some() {
                continue;
            }
            let source = keys::credential_source(&models, provider, keys::builtin_environment_names(provider), env);
            if matches!(source, KeySource::Environment(_)) {
                add(provider, source);
            }
        }
        rows
    }

    /// Runs the login of a sign in row to its end, yielding its progress.
    /// omp providers run omp's OAuth flow (the browser opens on the URL omp
    /// sends); other harnesses run their own login command. Steer it with
    /// [`Backend::submit_input`] and [`Backend::cancel`].
    pub fn login(&self, key: ProviderKey) -> impl Stream<Item = LoginUpdate> + Send + 'static {
        let backend = self.clone();
        iced::stream::channel(16, async move |mut out: iced::futures::channel::mpsc::Sender<LoginUpdate>| {
            let (control_tx, control_rx) = unbounded_channel();
            backend.inner.logins.locked().insert(key.clone(), control_tx);
            let (update_tx, mut update_rx) = unbounded_channel();
            let run = backend.run_login(key.clone(), control_rx, update_tx);
            let forward = async {
                while let Some(update) = update_rx.recv().await {
                    if out.send(update).await.is_err() {
                        break;
                    }
                }
            };
            tokio::join!(run, forward);
            backend.inner.logins.locked().remove(&key);
        })
    }

    async fn run_login(
        &self,
        key: ProviderKey,
        controls: tokio::sync::mpsc::UnboundedReceiver<LoginControl>,
        updates: UnboundedSender<LoginUpdate>,
    ) {
        if key.harness == OMP {
            let provider = self.inner.login_providers.locked().iter().find(|p| p.id == key.provider).cloned();
            let provider = provider.unwrap_or_else(|| LoginProvider {
                id: key.provider.clone(),
                name: key.provider.clone(),
                available: true,
                authenticated: false,
            });
            let (state_tx, mut state_rx) = unbounded_channel();
            let opener = self.inner.opener.clone();
            let rpc = self.inner.rpc.clone();
            let run = async move { run_login(rpc.as_ref(), opener.as_ref(), &provider, controls, state_tx).await };
            let forward = async {
                let mut mapper = FlowUpdates::default();
                while let Some(state) = state_rx.recv().await {
                    for update in mapper.map(state) {
                        let _ = updates.send(update);
                    }
                }
            };
            tokio::join!(run, forward);
            return;
        }
        let Some(harness) = self.inner.harnesses.iter().find(|h| h.key() == key.harness).cloned() else {
            let _ = updates.send(LoginUpdate::Failed(format!("unknown harness {}", key.harness)));
            return;
        };
        let process = match harness.auth().start_login(LoginKind::Browser).await {
            Ok(process) => process,
            Err(e) => {
                let _ = updates.send(LoginUpdate::Failed(e.to_string()));
                return;
            }
        };
        let (cli_tx, mut cli_rx) = unbounded_channel();
        let run = run_cli_login(process, controls, cli_tx);
        let forward = async {
            let mut last = None;
            while let Some(update) = cli_rx.recv().await {
                let update = match update {
                    CliLoginUpdate::Url(url) => LoginUpdate::Url { url, launch_url: None, instructions: None },
                    CliLoginUpdate::Output(line) => {
                        last = Some(line.clone());
                        LoginUpdate::Progress(line)
                    }
                    CliLoginUpdate::Prompt(message) => LoginUpdate::Prompt { message, placeholder: None },
                };
                let _ = updates.send(update);
            }
            last
        };
        let (outcome, last) = tokio::join!(run, forward);
        let finished = match outcome {
            Ok(CliLoginOutcome::Cancelled) => return,
            Ok(CliLoginOutcome::Exited(Some(0))) => match harness.auth().sign_in_state().await {
                Ok(SignInState::SignedIn { .. }) => LoginUpdate::Succeeded { account: None },
                Ok(_) | Err(_) => LoginUpdate::Failed(format!("{} still reports no sign in", harness.name())),
            },
            Ok(CliLoginOutcome::Exited(code)) => LoginUpdate::Failed(match (code, last) {
                (Some(code), Some(line)) => format!("login exited with code {code}: {line}"),
                (Some(code), None) => format!("login exited with code {code}"),
                (None, _) => "login was stopped".to_owned(),
            }),
            Err(e) => LoginUpdate::Failed(e.to_string()),
        };
        let _ = updates.send(finished);
    }

    /// Answers the running login's prompt of `key`.
    pub fn submit_input(&self, key: &ProviderKey, text: String) {
        if let Some(control) = self.inner.logins.locked().get(key) {
            let _ = control.send(LoginControl::SubmitInput(text));
        }
    }

    /// Stops the running login of `key`.
    pub fn cancel(&self, key: &ProviderKey) {
        if let Some(control) = self.inner.logins.locked().get(key) {
            let _ = control.send(LoginControl::Cancel);
        }
    }

    /// Opens `url` in the browser.
    pub fn open_url(&self, url: &str) -> Result<(), String> {
        self.inner.opener.open_url(url)
    }

    /// The models the picker offers and the roles they serve.
    pub async fn catalog(&self) -> Result<Catalog, String> {
        let files = self.files().await?;
        let config = OmpConfig::load(&files.config()).map_err(|e| e.to_string())?;
        let available = self.inner.rpc.get_available_models().await.map_err(|e| e.to_string())?;
        if self.inner.login_providers.locked().is_empty() {
            let _ = self.refresh_login_providers().await;
        }
        bridge::catalog(&available, &config, &self.provider_names())
    }

    /// Makes `role` resolve to `model` in omp's `config.yml`, keeping the
    /// thinking level the role had. omp validates the file before it is saved.
    pub async fn assign_role(&self, role: String, model: ModelRef) -> Result<(), String> {
        let files = self.files().await?;
        let mut config = OmpConfig::load(&files.config()).map_err(|e| e.to_string())?;
        let thinking = config
            .model_role(&role)
            .map_err(|e| e.to_string())?
            .and_then(|r| r.candidates.first().and_then(|c| c.thinking().map(str::to_owned)));
        let selector = Selector::Model { model: model.selector(), thinking };
        config.set_model_role(&role, &[selector]).map_err(|e| e.to_string())?;
        config.save_validated(self.inner.validator.as_ref()).await.map_err(|e| e.to_string())?;
        if let Some(loaded) = self.inner.loaded.lock().await.as_mut() {
            // The screen's files are now stale; a save must reload first.
            loaded.config = OmpConfig::load(&files.config()).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Reads omp's settings for the settings screen. Available models that
    /// cannot be listed (omp not running) leave the model lists empty.
    pub async fn load_settings(&self) -> Result<SettingsLoad, String> {
        let files = self.files().await?;
        let config = OmpConfig::load(&files.config()).map_err(|e| e.to_string())?;
        let models = ModelsYml::load(&files.models()).map_err(|e| e.to_string())?;
        let available = self.inner.rpc.get_available_models().await.unwrap_or_default();
        if self.inner.login_providers.locked().is_empty() {
            let _ = self.refresh_login_providers().await;
        }
        let mut settings = bridge::settings(&config, &models, &available, &self.provider_names())?;
        settings.harnesses = self.harness_models();
        let known_models = available.iter().map(rpc::ModelInfo::selector).collect();
        *self.inner.loaded.lock().await = Some(Loaded { config, models, settings: settings.clone(), available });
        Ok(SettingsLoad { settings, known_models })
    }

    /// Writes `settings` over what [`Backend::load_settings`] returned: only
    /// changed keys are edited, omp validates both files before either is
    /// written, and the saved settings are read back.
    pub async fn save_settings(&self, settings: ModelSettings) -> Result<ModelSettings, Vec<ValidationError>> {
        let fail = |location: &str, message: String| vec![ValidationError { location: location.into(), message }];
        let mut slot = self.inner.loaded.lock().await;
        let Some(loaded) = slot.as_mut() else {
            return Err(fail("Model settings", "the settings were not loaded".into()));
        };
        let mut config = loaded.config.clone();
        let mut models = loaded.models.clone();
        let edited = bridge::apply_settings(&mut config, &mut models, &loaded.settings, &settings)?;
        let harness_edits: Vec<(&Harness, &HarnessModel, &HarnessModel)> = settings
            .harnesses
            .iter()
            .filter_map(|after| {
                let before = loaded.settings.harnesses.iter().find(|b| b.harness == after.harness)?;
                let harness = self.inner.harnesses.iter().find(|h| h.key() == after.harness)?;
                (before != after).then_some((harness, before, after))
            })
            .collect();
        let harness_errors: Vec<ValidationError> = harness_edits
            .iter()
            .filter_map(|(harness, _, after)| {
                harness.check(after).err().map(|message| ValidationError { location: harness.name().into(), message })
            })
            .collect();
        if !harness_errors.is_empty() {
            return Err(harness_errors);
        }
        let validator = self.inner.validator.as_ref();
        if edited.config {
            config.validate(validator).await.map_err(|e| fail("config.yml", e.to_string()))?;
        }
        if edited.models {
            models.validate(validator).await.map_err(|e| fail("models.yml", e.to_string()))?;
        }
        if edited.config {
            config.save().map_err(|e| fail("config.yml", e.to_string()))?;
        }
        if edited.models {
            models.save().map_err(|e| fail("models.yml", e.to_string()))?;
        }
        for (harness, before, after) in harness_edits {
            harness.write(before, after).map_err(|message| fail(harness.name(), message))?;
        }
        let mut saved = bridge::settings(&config, &models, &loaded.available, &self.provider_names())
            .map_err(|message| fail("Model settings", message))?;
        saved.harnesses = self.harness_models();
        loaded.config = config;
        loaded.models = models;
        loaded.settings = saved.clone();
        Ok(saved)
    }

    /// The other harnesses' model settings; a harness whose config cannot be
    /// read is left out.
    fn harness_models(&self) -> Vec<HarnessModel> {
        self.inner.harnesses.iter().filter_map(|h| h.model_settings().ok()).collect()
    }
}

/// Turns the login flow's states into the screen's updates, sending each
/// change once.
#[derive(Default)]
struct FlowUpdates {
    url: Option<String>,
    progress: Option<String>,
    open_error: Option<String>,
    prompt: Option<String>,
}

impl FlowUpdates {
    fn map(&mut self, state: FlowState) -> Vec<LoginUpdate> {
        let mut out = Vec::new();
        let progress = |current: &mut Option<String>, value: Option<String>, out: &mut Vec<LoginUpdate>| {
            if value.is_some() && *current != value {
                current.clone_from(&value);
                out.extend(value.map(LoginUpdate::Progress));
            }
        };
        match state {
            FlowState::Idle | FlowState::Cancelled { .. } => {}
            FlowState::Starting { progress: value, .. } => progress(&mut self.progress, value, &mut out),
            FlowState::WaitingForBrowser { url, copy_url, instructions, progress: value, open_error, .. } => {
                if self.url.as_ref() != Some(&url) {
                    self.url = Some(url.clone());
                    let launch_url = (copy_url != url).then_some(copy_url);
                    out.push(LoginUpdate::Url { url, launch_url, instructions });
                }
                self.prompt = None;
                progress(&mut self.progress, value, &mut out);
                let open_error = open_error.map(|e| format!("The browser did not open ({e}); open the URL yourself."));
                progress(&mut self.open_error, open_error, &mut out);
            }
            FlowState::WaitingForInput { request_id, title, placeholder, .. } => {
                if self.prompt.as_ref() != Some(&request_id) {
                    self.prompt = Some(request_id);
                    out.push(LoginUpdate::Prompt { message: title, placeholder });
                }
            }
            FlowState::Succeeded { .. } => out.push(LoginUpdate::Succeeded { account: None }),
            FlowState::Failed { message, .. } => out.push(LoginUpdate::Failed(message)),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use async_trait::async_trait;
    use iced::futures::StreamExt;
    use loa_models::rpc::{LoginUiRequest, RpcError, UiResponse};

    use super::*;
    use crate::bridge::tests::model;
    use crate::settings::RoleChain;
    use crate::signin;

    #[derive(Default)]
    struct FakeRpc {
        providers: Vec<LoginProvider>,
        models: Vec<rpc::ModelInfo>,
        login_frames: Vec<LoginUiRequest>,
        /// Whether `login` answers after the first `respond_ui`; else it never answers.
        finishes: bool,
        responses: Mutex<Vec<UiResponse>>,
        answered: tokio::sync::Notify,
    }

    #[async_trait]
    impl OmpRpc for FakeRpc {
        async fn get_login_providers(&self) -> Result<Vec<LoginProvider>, RpcError> {
            Ok(self.providers.clone())
        }
        async fn login(&self, provider_id: &str, ui: UnboundedSender<LoginUiRequest>) -> Result<String, RpcError> {
            for frame in &self.login_frames {
                ui.send(frame.clone()).unwrap();
            }
            if !self.finishes {
                std::future::pending::<()>().await;
            }
            self.answered.notified().await;
            Ok(provider_id.to_owned())
        }
        async fn respond_ui(&self, response: UiResponse) -> Result<(), RpcError> {
            self.responses.locked().push(response);
            self.answered.notify_one();
            Ok(())
        }
        async fn get_available_models(&self) -> Result<Vec<rpc::ModelInfo>, RpcError> {
            Ok(self.models.clone())
        }
        async fn set_model(&self, _: &str, _: &str) -> Result<rpc::ModelInfo, RpcError> {
            Err(RpcError::Transport("the backend never switches a session model".into()))
        }
    }

    /// Accepts every file unless it contains `reject-me`.
    struct Validator;
    #[async_trait]
    impl SettingsValidator for Validator {
        async fn check_config(&self, text: &str, _: &[(String, serde_json::Value)]) -> loa_models::Result<()> {
            if text.contains("reject-me") {
                return Err(loa_models::Error::Rejected { what: "config.yml".into(), message: "bad role".into() });
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
            self.0.locked().push(url.to_owned());
            Ok(())
        }
    }

    struct Env(HashSet<&'static str>);
    impl EnvironmentPresence for Env {
        fn contains(&self, name: &str) -> bool {
            self.0.contains(name)
        }
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        loa_util::exec::write_executable(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        path
    }

    /// A `claude` that is signed out until its login (URL, pasted code) ran.
    fn fake_claude(dir: &Path) -> ClaudeCode {
        let program = script(
            dir,
            "claude",
            r#"dir=$(dirname "$0")
if [ "$1 $2" = "auth status" ]; then
  if [ -f "$dir/signed-in" ]; then echo '{"loggedIn":true,"authMethod":"claude.ai"}'; exit 0; fi
  echo '{"loggedIn":false}'; exit 1
fi
if [ "$1 $2" = "auth login" ]; then
  echo "Opening https://claude.example/oauth?state=1 in your browser"
  printf 'Paste code here: '
  read code
  touch "$dir/signed-in"
  echo "Login successful for $code"
  exit 0
fi
exit 64"#,
        );
        ClaudeCode::new(program, dir.join("claude-settings.json"), Arc::new(TokioProcessRunner))
    }

    fn provider(id: &str, name: &str, available: bool, authenticated: bool) -> LoginProvider {
        LoginProvider { id: id.into(), name: name.into(), available, authenticated }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        backend: Backend,
        rpc: Arc<FakeRpc>,
        opener: Arc<Opener>,
    }

    fn fixture(rpc: FakeRpc, config: &str, models: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.yml"), config).unwrap();
        std::fs::write(dir.path().join("models.yml"), models).unwrap();
        let rpc = Arc::new(rpc);
        let opener = Arc::new(Opener::default());
        let codex = Codex::new(dir.path().join("no-codex-here"), dir.path().join("config.toml"), Arc::new(TokioProcessRunner));
        let backend = Backend::new(Parts {
            rpc: rpc.clone(),
            files: OmpFiles::new(dir.path()),
            validator: Arc::new(Validator),
            opener: opener.clone(),
            claude: Some(fake_claude(dir.path())),
            codex: Some(codex),
            environment: Arc::new(Env(["GW_KEY", "OPENAI_API_KEY"].into())),
        });
        Fixture { dir, backend, rpc, opener }
    }

    fn omp_rpc() -> FakeRpc {
        FakeRpc {
            providers: vec![
                provider("google-antigravity", "Antigravity", false, false),
                provider("anthropic", "Anthropic (Claude Pro/Max)", true, true),
                provider("openai-codex", "ChatGPT Plus/Pro (Codex Subscription)", true, false),
            ],
            models: vec![model("anthropic", "claude-x"), model("openai-codex", "gpt-y")],
            login_frames: vec![
                LoginUiRequest::OpenUrl {
                    id: "u1".into(),
                    url: "https://auth.openai.example/authorize".into(),
                    launch_url: Some("http://127.0.0.1:1455/l".into()),
                    instructions: Some("Sign in with ChatGPT".into()),
                },
                LoginUiRequest::Input {
                    id: "u2".into(),
                    title: "Paste the redirect URL".into(),
                    placeholder: None,
                    timeout: None,
                },
            ],
            finishes: true,
            ..FakeRpc::default()
        }
    }

    #[tokio::test]
    async fn providers_list_omp_logins_api_keys_and_other_harnesses() {
        let f = fixture(omp_rpc(), "", "providers:\n  gw:\n    baseUrl: https://gw.example/v1\n    apiKey: GW_KEY\n");
        let rows = f.backend.providers().await;
        let summary: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|r| {
                let state = match &r.auth {
                    Auth::Login { signed_in: true, .. } => "signed in",
                    Auth::Login { signed_in: false, .. } => "signed out",
                    Auth::ApiKey { configured_via: Some(_), .. } => "key set",
                    Auth::ApiKey { configured_via: None, .. } => "no key",
                    Auth::Unavailable { .. } => "unavailable",
                };
                (r.harness_name.as_str(), r.key.provider.as_str(), state)
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("omp", "anthropic", "signed in"),
                ("omp", "openai-codex", "signed out"),
                ("omp", "google-antigravity", "unavailable"),
                ("omp API keys", "gw", "key set"),
                ("omp API keys", "openai", "key set"),
                ("Claude Code", "claude", "signed out"),
                ("Codex", "codex", "unavailable"),
            ]
        );
        let Auth::ApiKey { configured_via, instructions } = &rows[3].auth else { unreachable!() };
        assert_eq!(configured_via.as_deref(), Some("GW_KEY (models.yml)"));
        assert!(instructions.contains("GW_KEY"), "{instructions}");
        let Auth::Unavailable { reason } = &rows[6].auth else { unreachable!() };
        assert!(reason.contains("codex"), "{reason}");
    }

    #[tokio::test]
    async fn the_sign_in_screen_runs_an_omp_oauth_login_to_success() {
        let f = fixture(omp_rpc(), "", "");
        let mut screen = signin::State::new(f.backend.providers().await);
        let key = ProviderKey::new(OMP, "openai-codex");
        let (_, event) = screen.update(signin::Message::SignIn(key.clone()));
        assert_eq!(event, Some(signin::Event::SignIn(key.clone())));

        let mut updates = Box::pin(f.backend.login(key.clone()));
        let mut seen = Vec::new();
        while let Some(update) = updates.next().await {
            screen.login_update(&key, update.clone());
            if let LoginUpdate::Prompt { .. } = update {
                let _ = screen.update(signin::Message::PromptInput(key.clone(), "http://127.0.0.1:1455/cb?code=7".into()));
                let (_, event) = screen.update(signin::Message::SubmitPrompt(key.clone()));
                let Some(signin::Event::SubmitInput { key, text }) = event else { panic!("{event:?}") };
                f.backend.submit_input(&key, text);
            }
            seen.push(update);
        }
        assert_eq!(
            seen,
            [
                LoginUpdate::Url {
                    url: "https://auth.openai.example/authorize".into(),
                    launch_url: Some("http://127.0.0.1:1455/l".into()),
                    instructions: Some("Sign in with ChatGPT".into()),
                },
                LoginUpdate::Prompt { message: "Paste the redirect URL".into(), placeholder: None },
                LoginUpdate::Succeeded { account: None },
            ]
        );
        assert_eq!(*f.opener.0.locked(), ["https://auth.openai.example/authorize"]);
        assert_eq!(
            *f.rpc.responses.locked(),
            [UiResponse::Value { id: "u2".into(), value: "http://127.0.0.1:1455/cb?code=7".into() }]
        );
        assert_eq!(screen.login_state(&key), &signin::LoginState::Succeeded { account: None });
        assert!(matches!(
            screen.providers().iter().find(|p| p.key == key).unwrap().auth,
            Auth::Login { signed_in: true, .. }
        ));
    }

    #[tokio::test]
    async fn cancelling_an_omp_login_ends_its_updates() {
        let f = fixture(FakeRpc { finishes: false, ..omp_rpc() }, "", "");
        f.backend.providers().await;
        let key = ProviderKey::new(OMP, "openai-codex");
        let mut updates = Box::pin(f.backend.login(key.clone()));
        let mut seen = Vec::new();
        while let Some(update) = updates.next().await {
            if matches!(update, LoginUpdate::Url { .. }) {
                f.backend.cancel(&key);
            }
            seen.push(update);
        }
        assert!(seen.iter().all(|u| !matches!(u, LoginUpdate::Succeeded { .. } | LoginUpdate::Failed(_))), "{seen:?}");
        assert!(f.backend.inner.logins.locked().is_empty());
    }

    #[tokio::test]
    async fn a_harness_login_runs_its_cli_and_checks_the_sign_in_again() {
        let f = fixture(omp_rpc(), "", "");
        let key = ProviderKey::new("claude", "claude");
        let mut updates = Box::pin(f.backend.login(key.clone()));
        let mut seen = Vec::new();
        while let Some(update) = updates.next().await {
            if matches!(update, LoginUpdate::Prompt { .. }) {
                f.backend.submit_input(&key, "abc".into());
            }
            seen.push(update);
        }
        assert_eq!(
            seen,
            [
                LoginUpdate::Url { url: "https://claude.example/oauth?state=1".into(), launch_url: None, instructions: None },
                LoginUpdate::Prompt { message: "Paste code here:".into(), placeholder: None },
                LoginUpdate::Progress("Login successful for abc".into()),
                LoginUpdate::Succeeded { account: None },
            ]
        );
        let rows = f.backend.providers().await;
        let claude = rows.iter().find(|r| r.key == key).unwrap();
        assert_eq!(claude.auth, Auth::Login { signed_in: true, account: None });
    }

    #[tokio::test]
    async fn the_catalog_and_role_assignment_use_omp_config() {
        let f = fixture(omp_rpc(), "modelRoles:\n  smol: anthropic/claude-x:low # fast\n", "");
        let catalog = f.backend.catalog().await.unwrap();
        assert_eq!(catalog.models.len(), 2);
        assert_eq!(catalog.models[0].provider_name, "Anthropic (Claude Pro/Max)");
        assert_eq!(catalog.roles[0].model, ModelRef::new("anthropic", "claude-x"));

        f.backend.assign_role("smol".into(), ModelRef::new("openai-codex", "gpt-y")).await.unwrap();
        let text = std::fs::read_to_string(f.dir.path().join("config.yml")).unwrap();
        assert_eq!(text, "modelRoles:\n  smol: openai-codex/gpt-y:low # fast\n");
        let catalog = f.backend.catalog().await.unwrap();
        assert_eq!(catalog.roles[0].model, ModelRef::new("openai-codex", "gpt-y"));
    }

    #[tokio::test]
    async fn the_settings_screen_saves_through_omp_validation() {
        let config = "# keep me\nmodelRoles:\n  default: anthropic/claude-x\n";
        let f = fixture(omp_rpc(), config, "");
        let load = f.backend.load_settings().await.unwrap();
        assert_eq!(load.known_models, ["anthropic/claude-x", "openai-codex/gpt-y"]);
        let mut screen = crate::settings::State::new(load.settings, load.known_models);
        let _ = screen.update(crate::settings::Message::AddChainEntry(0, "openai-codex/gpt-y".into()));
        let (_, event) = screen.update(crate::settings::Message::Save);
        let Some(crate::settings::Event::Save(settings)) = event else { panic!("{event:?}") };
        let saved = f.backend.save_settings(settings).await.unwrap();
        screen.saved(saved.clone());
        assert!(!screen.is_dirty());
        assert_eq!(
            saved.roles,
            [RoleChain { role: "default".into(), chain: vec!["anthropic/claude-x".into(), "openai-codex/gpt-y".into()] }]
        );
        let text = std::fs::read_to_string(f.dir.path().join("config.yml")).unwrap();
        assert!(text.starts_with("# keep me\n"), "{text}");
        assert!(text.contains("fallbackChains:\n    default:\n      - openai-codex/gpt-y\n"), "{text}");

        // omp's rejection is shown and nothing is written.
        let _ = screen.update(crate::settings::Message::NewRoleInput("reject-me".into()));
        let _ = screen.update(crate::settings::Message::AddRole);
        let _ = screen.update(crate::settings::Message::AddChainEntry(1, "anthropic/claude-x".into()));
        let (_, event) = screen.update(crate::settings::Message::Save);
        let Some(crate::settings::Event::Save(settings)) = event else { panic!("{event:?}") };
        let errors = f.backend.save_settings(settings).await.unwrap_err();
        assert_eq!(errors[0].location, "config.yml");
        screen.set_errors(errors);
        assert_eq!(std::fs::read_to_string(f.dir.path().join("config.yml")).unwrap(), text);

        // A change made outside the app since the load is not overwritten.
        std::fs::write(f.dir.path().join("config.yml"), "modelRoles: {}\n").unwrap();
        let mut settings = saved.clone();
        settings.roles[0].chain.pop();
        let errors = f.backend.save_settings(settings).await.unwrap_err();
        assert!(errors[0].message.contains("changed on disk"), "{errors:?}");
    }

    #[tokio::test]
    async fn the_settings_screen_edits_the_other_harnesses_own_configs() {
        let f = fixture(omp_rpc(), "", "");
        std::fs::write(f.dir.path().join("config.toml"), "# codex\nmodel = \"gpt-5\"\n").unwrap();
        let load = f.backend.load_settings().await.unwrap();
        let names: Vec<&str> = load.settings.harnesses.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["Claude Code", "Codex"]);
        assert_eq!(load.settings.harnesses[1].model, "gpt-5");
        let mut screen = crate::settings::State::new(load.settings, load.known_models);
        let _ = screen.update(crate::settings::Message::HarnessModelChanged(0, "opus".into()));
        let _ = screen.update(crate::settings::Message::HarnessEffortChanged(0, "high".into()));
        let _ = screen.update(crate::settings::Message::HarnessEffortChanged(1, "xhigh".into()));
        let (_, event) = screen.update(crate::settings::Message::Save);
        let Some(crate::settings::Event::Save(settings)) = event else { panic!("{event:?}") };
        let saved = f.backend.save_settings(settings).await.unwrap();
        assert_eq!(saved.harnesses[0].model, "opus");
        assert_eq!(saved.harnesses[0].effort, "high");
        assert_eq!(saved.harnesses[1].effort, "xhigh");
        let codex = std::fs::read_to_string(f.dir.path().join("config.toml")).unwrap();
        assert_eq!(codex, "# codex\nmodel = \"gpt-5\"\nmodel_reasoning_effort = \"xhigh\"\n");
        let claude = std::fs::read_to_string(f.dir.path().join("claude-settings.json")).unwrap();
        assert!(claude.contains("\"model\": \"opus\""), "{claude}");

        let mut bad = saved.clone();
        bad.harnesses[1].model = "gpt 6".into();
        let errors = f.backend.save_settings(bad).await.unwrap_err();
        assert_eq!(errors[0].location, "Codex");
    }
}
