//! Login state and strategies shared by the service and sign-in screen.
pub use loa_contracts::harness::{LoginControl, LoginStrategy, LoginUpdate, UrlOpener};

/// Identifies one row: the harness that owns the login and its provider id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProviderKey {
    pub harness: String,
    pub provider: String,
}

impl ProviderKey {
    pub fn new(harness: impl Into<String>, provider: impl Into<String>) -> Self {
        Self { harness: harness.into(), provider: provider.into() }
    }
}

/// How a provider authenticates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    /// Subscription login (OAuth) run by the harness.
    Login { signed_in: bool, account: Option<String> },
    /// Provider authenticated by an API key the harness reads itself.
    ApiKey {
        /// Where the key was found, e.g. an environment variable name or a
        /// config key. `None` when no key is configured.
        configured_via: Option<String>,
        /// Tells the user where to put the key.
        instructions: String,
    },
    /// No sign in can be offered now: the harness is not installed, its
    /// status could not be read, or it offers no login for the provider.
    Unavailable { reason: String },
}

/// One provider row as the app reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderEntry {
    pub key: ProviderKey,
    /// Display name of the provider, e.g. "Anthropic".
    pub name: String,
    /// Display name of the owning harness, e.g. "omp" or "Claude Code". Rows
    /// are grouped under it.
    pub harness_name: String,
    pub auth: Auth,
}

/// What a row shows about its login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginState {
    Idle,
    /// Sign in requested, waiting for the harness to send the URL. Progress
    /// messages arriving before the URL are kept in `progress` and carried
    /// into [`LoginState::Browser`]; prompts arriving before the URL are
    /// ignored, since the user cannot have a code before visiting the URL.
    Starting { progress: Option<String> },
    /// Browser step: URL shown and copyable, waiting for the user to finish.
    Browser {
        url: String,
        /// The URL the Copy button writes: the launch URL when the harness
        /// sent one, the full URL otherwise.
        copy_url: String,
        instructions: Option<String>,
        /// The latest progress message.
        progress: Option<String>,
        prompt: Option<LoginPrompt>,
    },
    Succeeded { account: Option<String> },
    Failed { message: String },
    Cancelled,
}

impl LoginState {
    /// Whether a login is running for the row.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Starting { .. } | Self::Browser { .. })
    }
}

/// A pending prompt of a running login and what the user typed so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginPrompt {
    pub message: String,
    pub placeholder: Option<String>,
    pub input: String,
}



#[cfg(test)]
mod oauth_tests {
    use loa_harness::omp::oauth::OmpOAuthLogin;
    use loa_contracts::harness::model_rpc::{LoginProvider, LoginUiRequest, UiResponse};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use super::*;
    use crate::testing::{FakeOmp, LoginScript};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Opener(Mutex<Vec<String>>);
    impl UrlOpener for Opener {
        fn open_url(&self, url: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(url.to_owned());
            Ok(())
        }
    }
    fn provider() -> LoginProvider {
        LoginProvider { id: "anthropic".into(), name: "Anthropic".into(), available: true, authenticated: false }
    }
    fn frames() -> Vec<LoginUiRequest> {
        vec![
            LoginUiRequest::OpenUrl { id: "1".into(), url: "https://auth.example".into(), launch_url: Some("http://localhost/login".into()), instructions: None },
            LoginUiRequest::Input { id: "2".into(), title: "Paste code".into(), placeholder: None, timeout: None },
        ]
    }
    #[tokio::test]
    async fn relays_oauth_input_and_success() {
        let mut fake = FakeOmp::default();
        fake.login_frames = frames();
        fake.script = LoginScript::AwaitInput("code".into());
        let rpc = Arc::new(fake);
        let opener = Arc::new(Opener::default());
        let (ctl_tx, ctl_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);
        let strategy = Box::new(OmpOAuthLogin { rpc: rpc.clone(), opener: opener.clone(), provider: provider() });
        let drive = async {
            assert_eq!(out_rx.recv().await, Some(LoginUpdate::Url { url: "https://auth.example".into(), launch_url: Some("http://localhost/login".into()), instructions: None }));
            assert!(matches!(out_rx.recv().await, Some(LoginUpdate::Prompt { .. })));
            ctl_tx.send(LoginControl::SubmitInput("code".into())).await.unwrap();
            assert_eq!(out_rx.recv().await, Some(LoginUpdate::Succeeded { account: None }));
        };
        tokio::join!(strategy.run(ctl_rx, out_tx), drive);
        assert_eq!(*opener.0.lock().unwrap(), ["https://auth.example"]);
        assert_eq!(*rpc.responses.lock().unwrap(), [UiResponse::Value { id: "2".into(), value: "code".into() }]);
    }
    #[tokio::test]
    async fn cancellation_withdraws_pending_input() {
        let mut fake = FakeOmp::default();
        fake.login_frames = frames();
        fake.script = LoginScript::Hang;
        let rpc = Arc::new(fake);
        let (ctl_tx, ctl_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);
        let strategy = Box::new(OmpOAuthLogin { rpc: rpc.clone(), opener: Arc::new(Opener::default()), provider: provider() });
        let drive = async {
            assert!(matches!(out_rx.recv().await, Some(LoginUpdate::Url { .. })));
            assert!(matches!(out_rx.recv().await, Some(LoginUpdate::Prompt { .. })));
            ctl_tx.send(LoginControl::Cancel).await.unwrap();
            assert_eq!(out_rx.recv().await, Some(LoginUpdate::Cancelled));
        };
        tokio::join!(strategy.run(ctl_rx, out_tx), drive);
        assert_eq!(*rpc.responses.lock().unwrap(), [UiResponse::Cancelled { id: "2".into(), cancelled: true }]);
    }
}


