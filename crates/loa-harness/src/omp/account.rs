//! The real [`OmpRpc`]: a dedicated `omp --mode rpc --no-session` process for
//! sign in and model listing, plus where omp keeps its `config.yml` and
//! `models.yml`.
//!
//! The process is separate from any chat session: listing providers and models,
//! running an OAuth login and probing `set_model` never touch a session's
//! history. A live session's model is switched through that session's own
//! harness connection instead.
//!
//! [`OmpService`] starts the process on first use and starts a new one when it
//! has exited, so the app can hold one for its whole lifetime.

use loa_util::LockExt;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crate::omp::rpc as wire;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc::UnboundedSender;

use loa_settings::models::files::{OmpFiles, OmpFilesResolver};
use loa_settings::models::rpc::{LoginProvider, LoginUiRequest, ModelInfo, OmpRpc, RpcError, UiResponse};
use loa_settings::models::error::{Error, Result};

/// How to start omp: the executable, its working directory and extra
/// environment (for example `PI_CODING_AGENT_DIR` to point it at another agent
/// directory).
#[derive(Debug, Clone)]
pub struct OmpLaunch {
    pub executable: PathBuf,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
}

impl Default for OmpLaunch {
    fn default() -> Self {
        Self { executable: PathBuf::from("omp"), cwd: None, env: Vec::new() }
    }
}

impl OmpLaunch {
    /// Adds an environment variable for the omp child.
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command
    }
}

#[async_trait]
impl OmpFilesResolver for OmpLaunch {
    /// Asks omp for its active agent directory (`omp config path`), which
    /// honours `PI_CODING_AGENT_DIR` and profiles the way omp itself does.
    async fn resolve(&self) -> Result<OmpFiles> {
        let program = self.executable.display().to_string();
        let out = self
            .command(&["config", "path"])
            .output()
            .await
            .map_err(|e| Error::Process { program: program.clone(), message: e.to_string() })?;
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !out.status.success() || dir.is_empty() {
            return Err(Error::Process {
                program,
                message: format!(
                    "`config path` failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            });
        }
        Ok(OmpFiles::new(dir))
    }
}

fn rpc_error(err: wire::RpcError) -> RpcError {
    match err {
        wire::RpcError::Command { command, message, code } => RpcError::Command { command, message, code },
        other => RpcError::Transport(other.to_string()),
    }
}

/// Converts an omp UI frame to the login frame it is, when it is one.
fn login_frame(request: wire::UiRequest) -> Option<LoginUiRequest> {
    Some(match request {
        wire::UiRequest::OpenUrl { id, url, launch_url, instructions } => {
            LoginUiRequest::OpenUrl { id, url, launch_url, instructions }
        }
        wire::UiRequest::Notify { id, message, notify_type } => LoginUiRequest::Notify { id, message, notify_type },
        wire::UiRequest::Input { id, title, placeholder, timeout, secret: false } => {
            LoginUiRequest::Input { id, title, placeholder, timeout }
        }
        wire::UiRequest::Cancel { id, target_id } => LoginUiRequest::Cancel { id, target_id },
        _ => return None,
    })
}

/// The sender of the login that currently receives omp's UI frames.
type LoginSlot = Arc<Mutex<Option<UnboundedSender<LoginUiRequest>>>>;


/// One running `omp --mode rpc --no-session` process. The process is killed
/// when the client is dropped.
pub struct OmpClient {
    client: wire::RpcClient,
    login: LoginSlot,
}

impl std::fmt::Debug for OmpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OmpClient").field("client", &self.client).finish_non_exhaustive()
    }
}

impl Drop for OmpClient {
    fn drop(&mut self) {
        // The frame reader holds a clone of the client, so dropping this one
        // alone would not end the process.
        self.client.kill();
    }
}

impl OmpClient {
    /// Starts omp and waits for it to be ready.
    pub async fn spawn(launch: &OmpLaunch) -> std::result::Result<Self, RpcError> {
        let mut options = wire::SpawnOptions::new(&launch.executable).arg("--no-session");
        options.cwd = launch.cwd.clone();
        options.env = launch.env.clone();
        let (client, mut events) = wire::RpcClient::spawn(options).await.map_err(rpc_error)?;
        let login: LoginSlot = Arc::default();
        let reader = client.clone();
        let slot = login.clone();
        tokio::spawn(async move {
            while let Some(incoming) = events.recv().await {
                let wire::Incoming::Ui(request) = incoming else { continue };
                let dialog = request.is_dialog().then(|| request.id().to_owned());
                let sender = slot.locked().clone();
                let forwarded = match (sender, login_frame(request)) {
                    (Some(sender), Some(frame)) => sender.send(frame).is_ok(),
                    _ => false,
                };
                // A dialog nobody shows (no login running, a secret prompt omp
                // should never send over RPC) is dismissed so omp does not wait.
                if let (false, Some(id)) = (forwarded, dialog) {
                    let _ = reader.respond_ui(&wire::UiResponse::Cancelled { id, timed_out: false }).await;
                }
            }
        });
        Ok(Self { client, login })
    }

    /// Whether the process has exited.
    pub fn has_exited(&self) -> bool {
        self.client.has_exited()
    }

    /// The model the process would use for a session (`get_state`).
    pub async fn current_model(&self) -> std::result::Result<Option<ModelInfo>, RpcError> {
        #[derive(Deserialize)]
        struct State {
            #[serde(default)]
            model: Option<ModelInfo>,
        }
        Ok(self.request::<State>(wire::Command::GetState).await?.model)
    }

    /// Closes omp's stdin and waits up to `grace` for it to exit.
    pub async fn shutdown(&self, grace: std::time::Duration) {
        let _ = self.client.shutdown(grace).await;
    }

    async fn request<T: DeserializeOwned>(&self, command: wire::Command) -> std::result::Result<T, RpcError> {
        self.client.request(command).await.map_err(rpc_error)
    }
}

#[async_trait]
impl OmpRpc for OmpClient {
    async fn get_login_providers(&self) -> std::result::Result<Vec<LoginProvider>, RpcError> {
        #[derive(Deserialize)]
        struct Providers {
            providers: Vec<LoginProvider>,
        }
        Ok(self.request::<Providers>(wire::Command::GetLoginProviders).await?.providers)
    }

    async fn login(
        &self,
        provider_id: &str,
        ui: UnboundedSender<LoginUiRequest>,
    ) -> std::result::Result<String, RpcError> {
        {
            let mut slot = self.login.locked();
            if slot.as_ref().is_some_and(|running| !running.is_closed()) {
                return Err(RpcError::Command {
                    command: "login".into(),
                    message: "another login is already running".into(),
                    code: None,
                });
            }
            *slot = Some(ui);
        }
        let result = self
            .client
            .request::<serde_json::Value>(wire::Command::Login { provider_id: provider_id.to_owned() })
            .await
            .map_err(rpc_error);
        self.login.locked().take();
        result?;
        Ok(provider_id.to_owned())
    }

    async fn respond_ui(&self, response: UiResponse) -> std::result::Result<(), RpcError> {
        let response = match response {
            UiResponse::Value { id, value } => wire::UiResponse::Value { id, value },
            UiResponse::Cancelled { id, .. } => wire::UiResponse::Cancelled { id, timed_out: false },
        };
        self.client.respond_ui(&response).await.map_err(rpc_error)
    }

    async fn get_available_models(&self) -> std::result::Result<Vec<ModelInfo>, RpcError> {
        #[derive(Deserialize)]
        struct Models {
            models: Vec<ModelInfo>,
        }
        Ok(self.request::<Models>(wire::Command::GetAvailableModels).await?.models)
    }

    async fn set_model(&self, provider: &str, model_id: &str) -> std::result::Result<ModelInfo, RpcError> {
        self.request(wire::Command::SetModel { provider: provider.to_owned(), model_id: model_id.to_owned() }).await
    }
}

/// An [`OmpRpc`] that starts its omp process on first use and replaces it
/// when it has exited. Cheap to share behind an `Arc` for the app's lifetime.
#[derive(Debug)]
pub struct OmpService {
    launch: OmpLaunch,
    client: tokio::sync::Mutex<Option<Arc<OmpClient>>>,
}

impl OmpService {
    pub fn new(launch: OmpLaunch) -> Self {
        Self { launch, client: tokio::sync::Mutex::new(None) }
    }

    /// How the service starts omp.
    pub fn launch(&self) -> &OmpLaunch {
        &self.launch
    }

    /// The running client, starting omp when none runs.
    pub async fn client(&self) -> std::result::Result<Arc<OmpClient>, RpcError> {
        let mut slot = self.client.lock().await;
        if let Some(client) = slot.as_ref().filter(|client| !client.has_exited()) {
            return Ok(client.clone());
        }
        let client = Arc::new(OmpClient::spawn(&self.launch).await?);
        *slot = Some(client.clone());
        Ok(client)
    }

    /// Stops the process, if one runs.
    pub async fn shutdown(&self, grace: std::time::Duration) {
        let client = self.client.lock().await.take();
        if let Some(client) = client {
            client.shutdown(grace).await;
        }
    }
}

macro_rules! delegate_lazy_client {
    ($(fn $name:ident($($arg:ident: $ty:ty),*) -> $result:ty;)+) => {
        #[async_trait]
        impl OmpRpc for OmpService {
            $(
                async fn $name(&self, $($arg: $ty),*) -> std::result::Result<$result, RpcError> {
                    self.client().await?.$name($($arg),*).await
                }
            )+
        }
    };
}

delegate_lazy_client! {
    fn get_login_providers() -> Vec<LoginProvider>;
    fn login(provider_id: &str, ui: UnboundedSender<LoginUiRequest>) -> String;
    fn respond_ui(response: UiResponse) -> ();
    fn get_available_models() -> Vec<ModelInfo>;
    fn set_model(provider: &str, model_id: &str) -> ModelInfo;
}

/// Account access over the same RPC client used for provider discovery.
pub struct OmpAccount {
    pub rpc: Arc<dyn OmpRpc>,
    pub opener: Arc<dyn loa_contracts::harness::UrlOpener>,
    pub launch: OmpLaunch,
}

impl OmpAccount {
    fn config_path_result(&self) -> std::result::Result<PathBuf, loa_contracts::harness::AdapterError> {
        let mut command = std::process::Command::new(&self.launch.executable);
        command.args(["config", "path"]).envs(self.launch.env.iter().cloned());
        if let Some(cwd) = &self.launch.cwd { command.current_dir(cwd); }
        let output = command.output().map_err(loa_contracts::harness::AdapterError::Io)?;
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || path.is_empty() {
            return Err(loa_contracts::harness::AdapterError::Protocol("omp config path failed".into()));
        }
        Ok(PathBuf::from(path).join("config.yml"))
    }
}

#[async_trait]
impl loa_contracts::harness::HarnessAccount for OmpAccount {
    fn id(&self) -> &'static str { "omp" }
    fn name(&self) -> &'static str { "omp" }
    async fn sign_in_state(&self) -> loa_contracts::harness::SignInState {
        use loa_contracts::harness::{SignInMethod, SignInState};
        match self.rpc.get_login_providers().await {
            Ok(providers) if providers.iter().any(|provider| provider.authenticated) =>
                SignInState::SignedIn { method: SignInMethod::OAuthToken },
            Ok(_) => SignInState::SignedOut,
            Err(error) => SignInState::Unknown { reason: error.to_string() },
        }
    }
    fn login(&self, _: loa_contracts::harness::LoginKind) -> std::result::Result<Box<dyn loa_contracts::harness::LoginStrategy>, loa_contracts::harness::AdapterError> {
        Err(loa_contracts::harness::AdapterError::Unsupported { harness: "omp", what: "login without a provider" })
    }
    fn logout_command(&self) -> Option<loa_contracts::harness::CommandSpec> { None }
    fn efforts(&self) -> &'static [&'static str] {
        &loa_settings::models::selector::THINKING_LEVELS
    }
    fn config_path(&self) -> Option<PathBuf> { self.config_path_result().ok() }
    fn read_model(&self) -> std::result::Result<loa_contracts::harness::ModelSettings, loa_contracts::harness::AdapterError> {
        use loa_contracts::harness::{AdapterError, ModelSettings};
        use loa_settings::models::selector::Selector;
        let config = loa_settings::models::config::OmpConfig::load(&self.config_path_result()?)
            .map_err(|error| AdapterError::Protocol(error.to_string()))?;
        let Some(selector) = config.model_role("default").map_err(|error| AdapterError::Protocol(error.to_string()))?
            .and_then(|role| role.candidates.into_iter().next()) else {
            return Ok(ModelSettings::default());
        };
        let effort = selector.thinking().map(str::to_owned);
        let model = match selector {
            Selector::Model { model, .. } => model,
            Selector::Role { role, .. } => format!("@{role}"),
        };
        Ok(ModelSettings { model: Some(model), effort })
    }
    fn write_model(&self, settings: &loa_contracts::harness::ModelSettings) -> std::result::Result<(), loa_contracts::harness::AdapterError> {
        use loa_contracts::harness::AdapterError;
        use loa_settings::models::selector::Selector;
        let mut config = loa_settings::models::config::OmpConfig::load(&self.config_path_result()?)
            .map_err(|error| AdapterError::Protocol(error.to_string()))?;
        if let Some(model) = &settings.model {
            let mut selector = Selector::parse(model).map_err(|error| AdapterError::Protocol(error.to_string()))?;
            match &mut selector {
                Selector::Model { thinking, .. } | Selector::Role { thinking, .. } => *thinking = settings.effort.clone(),
            }
            let mut candidates = config.model_role("default").map_err(|error| AdapterError::Protocol(error.to_string()))?
                .map(|role| role.candidates).unwrap_or_default();
            if candidates.is_empty() { candidates.push(selector); } else { candidates[0] = selector; }
            config.set_model_role("default", &candidates).map_err(|error| AdapterError::Protocol(error.to_string()))?;
        } else {
            config.clear_model_role("default").map_err(|error| AdapterError::Protocol(error.to_string()))?;
        }
        config.save().map_err(|error| AdapterError::Protocol(error.to_string()))
    }
}

impl loa_settings::models::login::OmpProviderAccount for OmpAccount {
    fn login_provider(&self, provider: LoginProvider) -> std::result::Result<Box<dyn loa_contracts::harness::LoginStrategy>, loa_contracts::harness::AdapterError> {
        Ok(Box::new(super::oauth::OmpOAuthLogin { rpc: self.rpc.clone(), opener: self.opener.clone(), provider }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_contracts::harness::{LoginControl, LoginStrategy, LoginUpdate, UrlOpener};
    use crate::omp::oauth::OmpOAuthLogin;
    use crate::testing::{FakeProcess as FakeOmp, handshake_v2};
    use tokio::sync::mpsc::channel;

    #[derive(Default)]
    struct Opener(Mutex<Vec<String>>);
    impl UrlOpener for Opener {
        fn open_url(&self, url: &str) -> std::result::Result<(), String> {
            self.0.locked().push(url.to_owned());
            Ok(())
        }
    }

    fn launch(fake: &FakeOmp) -> OmpLaunch {
        OmpLaunch { executable: fake.executable().to_owned(), ..OmpLaunch::default() }
    }

    #[tokio::test]
    async fn login_frames_reach_the_flow_and_input_answers_reach_omp() {
        let dir = tempfile::tempdir().unwrap();
        let script = format!(
            "{}< login lg\n\
             > {{\"type\":\"extension_ui_request\",\"id\":\"u1\",\"method\":\"open_url\",\"url\":\"https://auth.example/authorize\",\"launchUrl\":\"http://127.0.0.1:9/l\"}}\n\
             > {{\"type\":\"extension_ui_request\",\"id\":\"u2\",\"method\":\"input\",\"title\":\"Paste the callback URL\"}}\n\
             < extension_ui_response\n\
             > {{\"type\":\"response\",\"id\":\"{{{{lg}}}}\",\"command\":\"login\",\"success\":true}}\n",
            handshake_v2()
        );
        let fake = FakeOmp::install(dir.path(), &script).unwrap();
        let client = OmpClient::spawn(&launch(&fake)).await.unwrap();
        assert_eq!(fake.args(), ["--mode", "rpc", "--no-session"]);
        let opener = Arc::new(Opener::default());
        let provider = LoginProvider { id: "anthropic".into(), name: "Anthropic".into(), available: true, authenticated: false };
        let (control_tx, control_rx) = channel(16);
        let (state_tx, mut state_rx) = channel(16);
        let answer = async {
            let mut succeeded = false;
            while let Some(state) = state_rx.recv().await {
                if matches!(state, LoginUpdate::Prompt { .. }) {
                    control_tx.send(LoginControl::SubmitInput("http://127.0.0.1:9/cb?code=1".into())).await.unwrap();
                } else if matches!(state, LoginUpdate::Succeeded { .. }) {
                    succeeded = true;
                }
            }
            succeeded
        };
        let strategy = Box::new(OmpOAuthLogin { rpc: Arc::new(client), opener: opener.clone(), provider });
        let (_, succeeded) = tokio::join!(strategy.run(control_rx, state_tx), answer);
        assert!(succeeded);
        assert_eq!(*opener.0.locked(), ["https://auth.example/authorize"]);
        let received = fake.received();
        assert!(received.iter().any(|f| f["type"] == "login" && f["providerId"] == "anthropic"), "{received:?}");
        assert!(
            received.iter().any(|f| f["type"] == "extension_ui_response" && f["id"] == "u2" && f["value"] == "http://127.0.0.1:9/cb?code=1"),
            "{received:?}"
        );
    }

    #[tokio::test]
    async fn dialogs_outside_a_login_are_dismissed_and_models_decode() {
        let dir = tempfile::tempdir().unwrap();
        let script = format!(
            "{}< get_available_models gm\n\
             > {{\"type\":\"extension_ui_request\",\"id\":\"d1\",\"method\":\"confirm\",\"title\":\"Trust?\"}}\n\
             < extension_ui_response\n\
             > {{\"type\":\"response\",\"id\":\"{{{{gm}}}}\",\"command\":\"get_available_models\",\"success\":true,\"data\":{{\"models\":[{{\"id\":\"m1\",\"name\":\"M One\",\"provider\":\"gw\",\"contextWindow\":null}}]}}}}\n\
             < set_model sm\n\
             > {{\"type\":\"response\",\"id\":\"{{{{sm}}}}\",\"command\":\"set_model\",\"success\":false,\"error\":\"Model not found: gw/nope\"}}\n",
            handshake_v2()
        );
        let fake = FakeOmp::install(dir.path(), &script).unwrap();
        let service = OmpService::new(launch(&fake));
        let models = service.get_available_models().await.unwrap();
        assert_eq!(models.iter().map(ModelInfo::selector).collect::<Vec<_>>(), ["gw/m1"]);
        let err = service.set_model("gw", "nope").await.unwrap_err();
        assert_eq!(
            err,
            RpcError::Command { command: "set_model".into(), message: "Model not found: gw/nope".into(), code: None }
        );
        let received = fake.received();
        assert_eq!(received[2]["type"], "extension_ui_response");
        assert_eq!(received[2]["id"], "d1");
        assert_eq!(received[2]["cancelled"], true);
    }

    #[test]
    fn secret_input_is_never_forwarded_to_login() {
        let input = |secret| wire::UiRequest::Input {
            id: "1".into(), title: "Code".into(), placeholder: None, timeout: None, secret,
        };
        assert!(login_frame(input(true)).is_none());
        assert!(matches!(login_frame(input(false)), Some(LoginUiRequest::Input { .. })));
    }

    #[test]
    fn account_edits_default_role_without_losing_other_candidates() {
        use loa_contracts::harness::{HarnessAccount, ModelSettings};
        let dir = tempfile::tempdir().unwrap();
        let program = crate::account_support::process::testutil::fake_program(
            dir.path(), "omp", "if [ \"$1 $2\" = 'config path' ]; then printf '%s\\n' \"$TEST_AGENT_DIR\"; exit 0; fi\nexit 64"
        );
        let config = dir.path().join("config.yml");
        std::fs::write(&config, "# keep\nmodelRoles:\n  default: old/model:low,other/model\n").unwrap();
        let launch = OmpLaunch { executable: program, cwd: None, env: vec![("TEST_AGENT_DIR".into(), dir.path().as_os_str().into())] };
        let account = OmpAccount { rpc: Arc::new(OmpService::new(launch.clone())), opener: Arc::new(Opener::default()), launch };
        let before = account.read_model().unwrap();
        assert_eq!(before, ModelSettings { model: Some("old/model".into()), effort: Some("low".into()) });
        let after = ModelSettings { model: Some("new/model".into()), effort: Some("high".into()) };
        account.write_model(&after).unwrap();
        assert_eq!(account.read_model().unwrap(), after);
        let text = std::fs::read_to_string(config).unwrap();
        assert!(text.contains("# keep"));
        assert!(text.contains("new/model:high,other/model"));
    }

    #[tokio::test]
    async fn the_service_starts_a_new_omp_after_the_old_one_exited() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeOmp::install(dir.path(), &format!("{}exit 0\n", handshake_v2())).unwrap();
        let service = OmpService::new(launch(&fake));
        let first = service.client().await.unwrap();
        for _ in 0..200 {
            if first.has_exited() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(first.has_exited());
        let second = service.client().await.unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
    }
}
