//! In-memory omp RPC for exercising model and login flows without a process.
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::{Notify, mpsc};
use loa_settings::models::rpc::{LoginProvider, LoginUiRequest, ModelInfo, OmpRpc, RpcError, UiResponse};
use loa_contracts::harness::{
    AdapterError, CommandSpec, HarnessAccount, LoginKind, LoginStrategy, ModelSettings, SignInState,
};
use std::path::PathBuf;

/// The behavior of a fake login after its UI frames have been sent.
#[derive(Clone, Default)]
pub enum LoginScript {
    /// Wait indefinitely until the caller cancels the login.
    #[default]
    Hang,
    /// Return the given result immediately.
    Result(Result<String, RpcError>),
    /// Succeed after any UI response.
    AwaitResponse,
    /// Succeed only when the response has the expected value.
    AwaitInput(String),
    /// Complete after a delay, marking the provider authenticated.
    CompleteAfter(Duration),
}

/// Configurable fake implementing the same RPC port used by the application.
#[derive(Default)]
pub struct FakeOmp {
    pub providers: Vec<LoginProvider>,
    pub models: Vec<ModelInfo>,
    pub login_frames: Vec<LoginUiRequest>,
    pub script: LoginScript,
    pub responses: Mutex<Vec<UiResponse>>,
    pub selected: Mutex<Vec<(String, String)>>,
    answered: Notify,
    authenticated: Mutex<HashSet<String>>,
}

#[async_trait::async_trait]
impl OmpRpc for FakeOmp {
    async fn get_login_providers(&self) -> Result<Vec<LoginProvider>, RpcError> {
        let authenticated = self.authenticated.lock().unwrap();
        Ok(self.providers.iter().cloned().map(|mut provider| {
            provider.authenticated |= authenticated.contains(&provider.id);
            provider
        }).collect())
    }

    async fn login(&self, provider_id: &str, ui: mpsc::UnboundedSender<LoginUiRequest>) -> Result<String, RpcError> {
        for frame in &self.login_frames {
            ui.send(frame.clone()).map_err(|error| RpcError::Transport(error.to_string()))?;
        }
        match &self.script {
            LoginScript::Hang => std::future::pending().await,
            LoginScript::Result(result) => result.clone(),
            LoginScript::AwaitResponse | LoginScript::AwaitInput(_) => {
                self.answered.notified().await;
                if let LoginScript::AwaitInput(expected) = &self.script
                    && !matches!(self.responses.lock().unwrap().last(), Some(UiResponse::Value { value, .. }) if value == expected)
                {
                    return Err(RpcError::Command { command: "login".into(), message: "bad callback".into(), code: None });
                }
                Ok(provider_id.to_owned())
            }
            LoginScript::CompleteAfter(delay) => {
                tokio::time::sleep(*delay).await;
                self.authenticated.lock().unwrap().insert(provider_id.to_owned());
                Ok(provider_id.to_owned())
            }
        }
    }

    async fn respond_ui(&self, response: UiResponse) -> Result<(), RpcError> {
        self.responses.lock().unwrap().push(response);
        self.answered.notify_one();
        Ok(())
    }

    async fn get_available_models(&self) -> Result<Vec<ModelInfo>, RpcError> {
        Ok(self.models.clone())
    }

    async fn set_model(&self, provider: &str, model_id: &str) -> Result<ModelInfo, RpcError> {
        self.selected.lock().unwrap().push((provider.to_owned(), model_id.to_owned()));
        self.models.iter().find(|model| model.provider == provider && model.id == model_id).cloned().ok_or_else(|| RpcError::Command {
            command: "set_model".into(), message: format!("Model not found: {provider}/{model_id}"), code: None,
        })
    }
}

/// An omp account that runs nothing: provider logins are refused and the
/// default model reads as unset.
pub struct OfflineOmpAccount;

#[async_trait::async_trait]
impl HarnessAccount for OfflineOmpAccount {
    fn id(&self) -> &'static str { "omp" }
    fn name(&self) -> &'static str { "omp" }
    async fn sign_in_state(&self) -> SignInState { SignInState::SignedOut }
    fn login(&self, _: LoginKind) -> Result<Box<dyn LoginStrategy>, AdapterError> {
        Err(AdapterError::Unsupported { harness: "omp", what: "login without a provider" })
    }
    fn logout_command(&self) -> Option<CommandSpec> { None }
    fn efforts(&self) -> &'static [&'static str] { &loa_settings::models::selector::THINKING_LEVELS }
    fn config_path(&self) -> Option<PathBuf> { None }
    fn read_model(&self) -> Result<ModelSettings, AdapterError> { Ok(ModelSettings::default()) }
    fn write_model(&self, _: &ModelSettings) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported { harness: "omp", what: "model settings offline" })
    }
}

impl loa_settings::models::login::OmpProviderAccount for OfflineOmpAccount {
    fn login_provider(&self, _: LoginProvider) -> Result<Box<dyn LoginStrategy>, AdapterError> {
        Err(AdapterError::Unsupported { harness: "omp", what: "provider login offline" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn login_records_and_checks_input() {
        let mut fake = FakeOmp { script: LoginScript::AwaitInput("code".into()), ..Default::default() };
        fake.login_frames.push(LoginUiRequest::Input {
            id: "prompt".into(), title: "Code".into(), placeholder: None, timeout: None,
        });
        let (tx, mut rx) = mpsc::unbounded_channel();
        let login = fake.login("provider", tx);
        let answer = async {
            assert!(matches!(rx.recv().await, Some(LoginUiRequest::Input { .. })));
            fake.respond_ui(UiResponse::Value { id: "prompt".into(), value: "code".into() }).await.unwrap();
        };
        let (result, ()) = tokio::join!(login, answer);
        assert_eq!(result.unwrap(), "provider");
        assert_eq!(fake.responses.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn completed_login_updates_provider_state() {
        let mut fake = FakeOmp::default();
        fake.providers.push(LoginProvider { id: "provider".into(), name: "Provider".into(), available: true, authenticated: false });
        fake.script = LoginScript::CompleteAfter(Duration::ZERO);
        let (tx, _rx) = mpsc::unbounded_channel();
        assert_eq!(fake.login("provider", tx).await.unwrap(), "provider");
        assert!(fake.get_login_providers().await.unwrap()[0].authenticated);
    }
}
