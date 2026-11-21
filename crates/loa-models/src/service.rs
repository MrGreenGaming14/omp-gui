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
use std::sync::{Arc, Mutex};

use loa_harness::omp::config::OmpConfig;
use crate::keys::{self, EnvironmentPresence, KeySource, ProcessEnvironment};
use loa_contracts::harness::{HarnessAccount, LoginKind, SignInState};
use crate::login::{Auth, LoginControl, LoginStrategy, ProviderEntry, ProviderKey, UrlOpener};
use loa_harness::omp::models::ModelsYml;
use loa_harness::omp::account::{OmpAccount, OmpFiles, OmpLaunch, OmpService};
use loa_harness::account_support::opener::SystemUrlOpener;
use loa_harness::account_support::process::TokioProcessRunner;
use loa_contracts::harness::model_rpc::{self as rpc, LoginProvider, OmpRpc};
use loa_contracts::harness::selector::{Catalog, ModelInfo, ModelRef, RoleAssignment, Selector};
use crate::settings::{self, HarnessModel, ModelSettings, ValidationError};
use crate::validate::OmpSettingsValidator;
use loa_contracts::harness::model_error::SettingsValidator;
use loa_contracts::harness::LoginUpdate;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// The harness id of omp's rows on the sign in screen.
pub const OMP: &str = "omp";

/// A harness account exposed on the sign-in and settings screens.
#[derive(Clone)]
struct Harness(Arc<dyn HarnessAccount>);

impl Harness {
    fn key(&self) -> &'static str { self.0.id() }
    fn name(&self) -> &'static str { self.0.name() }

    fn model_settings(&self) -> Result<HarnessModel, String> {
        let settings = self.0.read_model().map_err(|e| e.to_string())?;
        Ok(HarnessModel {
            harness: self.key().to_owned(),
            name: self.name().to_owned(),
            config_path: self.0.config_path().map(|path| path.display().to_string()).unwrap_or_default(),
            model: settings.model.unwrap_or_default(),
            effort: settings.effort.unwrap_or_default(),
            efforts: self.0.efforts().iter().map(|value| (*value).to_owned()).collect(),
        })
    }

    fn check(&self, after: &HarnessModel) -> Result<(), String> {
        if after.model.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!("model `{}` must have no spaces", after.model));
        }
        if !after.effort.is_empty() && !self.0.efforts().contains(&after.effort.as_str()) {
            return Err(format!("effort `{}` is not one of {}", after.effort, self.0.efforts().join(", ")));
        }
        Ok(())
    }

    fn write(&self, before: &HarnessModel, after: &HarnessModel) -> Result<(), String> {
        let mut settings = self.0.read_model().map_err(|error| error.to_string())?;
        if before.model != after.model {
            settings.model = (!after.model.is_empty()).then(|| after.model.clone());
        }
        if before.effort != after.effort {
            settings.effort = (!after.effort.is_empty()).then(|| after.effort.clone());
        }
        self.0.write_model(&settings).map_err(|error| error.to_string())
    }
}

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
    pub harnesses: Vec<Arc<dyn HarnessAccount>>,
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
    omp_account: Arc<dyn HarnessAccount>,
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


impl Backend {
    /// The real backend: omp started from `launch`, its active agent
    /// directory, validation by omp, the platform browser and the `claude` and
    /// `codex` CLIs on `PATH`.
    pub fn system(launch: OmpLaunch) -> Self {
        let runner = Arc::new(TokioProcessRunner);
        let harnesses = loa_harness::factories(&loa_harness::HarnessEnv::default())
            .into_iter().filter(|factory| factory.id() != OMP)
            .filter_map(|factory| factory.account(None)).map(Harness).collect();
        Self::build(
            Arc::new(OmpService::new(launch.clone())),
            FilesSource::Resolve { launch: launch.clone(), cached: tokio::sync::OnceCell::new() },
            Arc::new(OmpSettingsValidator::new(launch.executable.clone(), runner)),
            Arc::new(SystemUrlOpener),
            harnesses,
            Arc::new(ProcessEnvironment),
        )
    }

    /// A backend over the given parts.
    pub fn new(parts: Parts) -> Self {
        let harnesses = parts.harnesses.into_iter().map(Harness).collect();
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
        let launch = match &files {
            FilesSource::Resolve { launch, .. } => launch.clone(),
            FilesSource::Fixed(_) => OmpLaunch::default(),
        };
        let omp_account = Arc::new(OmpAccount { rpc: rpc.clone(), opener: opener.clone(), launch });
        Self {
            inner: Arc::new(Inner {
                rpc,
                files,
                validator,
                opener,
                omp_account,
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
            Ok(mut providers) => {
                providers.sort_by(|a, b| {
                    let rank = |p: &LoginProvider| if p.authenticated { 0 } else if p.available { 1 } else { 2 };
                    rank(a).cmp(&rank(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                entries.extend(providers.into_iter().map(|provider| ProviderEntry {
                    key: ProviderKey::new(OMP, &provider.id),
                    name: provider.name,
                    harness_name: OMP.to_owned(),
                    auth: if provider.authenticated {
                        Auth::Login { signed_in: true, account: None }
                    } else if provider.available {
                        Auth::Login { signed_in: false, account: None }
                    } else {
                        Auth::Unavailable { reason: "omp offers no sign in for this provider now".into() }
                    },
                }));
            }
            Err(reason) => entries.push(ProviderEntry {
                key: ProviderKey::new(OMP, OMP),
                name: "omp".into(),
                harness_name: OMP.to_owned(),
                auth: Auth::Unavailable { reason },
            }),
        }
        entries.extend(self.api_key_rows().await);
        for harness in &self.inner.harnesses {
            let auth = match harness.0.sign_in_state().await {
                SignInState::SignedIn { .. } => Auth::Login { signed_in: true, account: None },
                SignInState::SignedOut => Auth::Login { signed_in: false, account: None },
                SignInState::Unknown { reason } => Auth::Unavailable { reason },
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
    pub fn login(&self, key: ProviderKey) -> UnboundedReceiver<LoginUpdate> {
        let backend = self.clone();
        let (control_tx, control_rx) = unbounded_channel();
        backend.inner.logins.locked().insert(key.clone(), control_tx);
        let (update_tx, update_rx) = unbounded_channel();
        tokio::spawn(async move {
            backend.run_login(key.clone(), control_rx, update_tx).await;
            backend.inner.logins.locked().remove(&key);
        });
        update_rx
    }

    async fn run_login(
        &self,
        key: ProviderKey,
        controls: tokio::sync::mpsc::UnboundedReceiver<LoginControl>,
        updates: UnboundedSender<LoginUpdate>,
    ) {
        let (strategy, harness): (Box<dyn LoginStrategy>, Option<Harness>) = if key.harness == OMP {
            let provider = self.inner.login_providers.locked().iter().find(|p| p.id == key.provider).cloned()
                .unwrap_or_else(|| LoginProvider {
                    id: key.provider.clone(), name: key.provider.clone(), available: true, authenticated: false,
                });
            match self.inner.omp_account.login_provider(provider) {
                Ok(strategy) => (strategy, None),
                Err(error) => {
                    let _ = updates.send(LoginUpdate::Failed(error.to_string()));
                    return;
                }
            }
        } else {
            let Some(harness) = self.inner.harnesses.iter().find(|h| h.key() == key.harness).cloned() else {
                let _ = updates.send(LoginUpdate::Failed(format!("unknown harness {}", key.harness)));
                return;
            };
            let strategy = match harness.0.login(LoginKind::Browser) {
                Ok(strategy) => strategy,
                Err(e) => {
                    let _ = updates.send(LoginUpdate::Failed(e.to_string()));
                    return;
                }
            };
            (strategy, Some(harness))
        };
        let (control_tx, control_rx) = tokio::sync::mpsc::channel(16);
        let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(16);
        let run = strategy.run(control_rx, update_tx);
        let forward_controls = async move {
            let mut controls = controls;
            while let Some(control) = controls.recv().await {
                if control_tx.send(control).await.is_err() { break; }
            }
        };
        let run = async { tokio::select! { _ = run => (), _ = forward_controls => () } };
        let forward_updates = async {
            while let Some(mut update) = update_rx.recv().await {
                if matches!(update, LoginUpdate::Succeeded { .. })
                    && let Some(harness) = &harness
                    && !matches!(harness.0.sign_in_state().await, SignInState::SignedIn { .. })
                {
                    update = LoginUpdate::Failed(format!("{} still reports no sign in", harness.name()));
                }
                let _ = updates.send(update);
            }
        };
        tokio::join!(run, forward_updates);
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
        catalog(&available, &config, &self.provider_names())
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
        let mut settings = settings::settings(&config, &models, &available, &self.provider_names())?;
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
        let edited = settings::apply_settings(&mut config, &mut models, &loaded.settings, &settings)?;
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
        let mut saved = settings::settings(&config, &models, &loaded.available, &self.provider_names())
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

fn resolve_role(role: &str, roles: &HashMap<&str, &[Selector]>, available: &[rpc::ModelInfo], depth: usize) -> Option<ModelRef> {
    for candidate in roles.get(role)?.iter() {
        let found = match candidate {
            Selector::Model { model, .. } => available.iter()
                .find(|m| m.selector() == *model)
                .or_else(|| available.iter().find(|m| m.id == *model))
                .map(|m| ModelRef::new(&m.provider, &m.id)),
            Selector::Role { role, .. } if depth < 4 => resolve_role(role, roles, available, depth + 1),
            Selector::Role { .. } => None,
        };
        if found.is_some() { return found; }
    }
    None
}

fn catalog(available: &[rpc::ModelInfo], config: &OmpConfig, names: &HashMap<String, String>) -> Result<Catalog, String> {
    let assignments = config.model_roles().map_err(|e| e.to_string())?;
    let roles: HashMap<&str, &[Selector]> = assignments.iter().map(|a| (a.role.as_str(), a.candidates.as_slice())).collect();
    let models = available.iter().map(|m| ModelInfo {
        model: ModelRef::new(&m.provider, &m.id),
        name: m.name.clone(),
        provider_name: names.get(&m.provider).cloned().unwrap_or_default(),
    }).collect();
    let roles = assignments.iter().filter_map(|a| {
        resolve_role(&a.role, &roles, available, 0).map(|model| RoleAssignment { role: a.role.clone(), model })
    }).collect();
    Ok(Catalog { models, roles })
}


