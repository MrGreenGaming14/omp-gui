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

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use loa_omp_rpc as wire;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc::UnboundedSender;

use crate::rpc::{LoginProvider, LoginUiRequest, ModelInfo, OmpRpc, RpcError, UiResponse};
use crate::{Error, Result};

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
        let mut command = tokio::process::Command::new(loa_exec::resolve(&self.executable));
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

/// omp's settings files in its active agent directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmpFiles {
    pub agent_dir: PathBuf,
}

impl OmpFiles {
    pub fn new(agent_dir: impl Into<PathBuf>) -> Self {
        Self { agent_dir: agent_dir.into() }
    }

    /// Asks omp for its active agent directory (`omp config path`), which
    /// honours `PI_CODING_AGENT_DIR` and profiles the way omp itself does.
    pub async fn resolve(launch: &OmpLaunch) -> Result<Self> {
        let program = launch.executable.display().to_string();
        let out = launch
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
        Ok(Self::new(dir))
    }

    /// `config.yml`: roles, fallback chains, enabled models and providers.
    pub fn config(&self) -> PathBuf {
        self.agent_dir.join("config.yml")
    }

    /// `models.yml`: custom providers and model overrides.
    pub fn models(&self) -> PathBuf {
        self.agent_dir.join("models.yml")
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
                let sender = lock(&slot).clone();
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
            let mut slot = lock(&self.login);
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
            .request_raw(wire::Command::Login { provider_id: provider_id.to_owned() })
            .await
            .map_err(rpc_error);
        lock(&self.login).take();
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

#[async_trait]
impl OmpRpc for OmpService {
    async fn get_login_providers(&self) -> std::result::Result<Vec<LoginProvider>, RpcError> {
        self.client().await?.get_login_providers().await
    }

    async fn login(
        &self,
        provider_id: &str,
        ui: UnboundedSender<LoginUiRequest>,
    ) -> std::result::Result<String, RpcError> {
        self.client().await?.login(provider_id, ui).await
    }

    async fn respond_ui(&self, response: UiResponse) -> std::result::Result<(), RpcError> {
        self.client().await?.respond_ui(response).await
    }

    async fn get_available_models(&self) -> std::result::Result<Vec<ModelInfo>, RpcError> {
        self.client().await?.get_available_models().await
    }

    async fn set_model(&self, provider: &str, model_id: &str) -> std::result::Result<ModelInfo, RpcError> {
        self.client().await?.set_model(provider, model_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::login::{LoginControl, LoginState, UrlOpener, run_login};
    use loa_omp_rpc::fake::{FakeOmp, handshake_v2};
    use tokio::sync::mpsc::unbounded_channel;

    #[derive(Default)]
    struct Opener(Mutex<Vec<String>>);
    impl UrlOpener for Opener {
        fn open_url(&self, url: &str) -> std::result::Result<(), String> {
            lock(&self.0).push(url.to_owned());
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
        let opener = Opener::default();
        let provider = LoginProvider { id: "anthropic".into(), name: "Anthropic".into(), available: true, authenticated: false };
        let (control_tx, control_rx) = unbounded_channel();
        let (state_tx, mut state_rx) = unbounded_channel();
        let answer = async {
            while let Some(state) = state_rx.recv().await {
                if matches!(state, LoginState::WaitingForInput { .. }) {
                    control_tx.send(LoginControl::SubmitInput("http://127.0.0.1:9/cb?code=1".into())).unwrap();
                }
            }
        };
        let (end, ()) = tokio::join!(run_login(&client, &opener, &provider, control_rx, state_tx), answer);
        assert_eq!(end, LoginState::Succeeded { provider: "anthropic".into() });
        assert_eq!(*lock(&opener.0), ["https://auth.example/authorize"]);
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
