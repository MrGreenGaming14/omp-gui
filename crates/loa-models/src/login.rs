//! Login state and strategies shared by the service and sign-in screen.
pub use loa_contracts::harness::{LoginControl, LoginStrategy, LoginUpdate, UrlOpener};
use loa_contracts::harness::{AdapterError, HarnessAccount};

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

/// omp's sign-in account: a [`HarnessAccount`] that also logs in to one of
/// the OAuth providers omp lists.
pub trait OmpProviderAccount: HarnessAccount {
    fn login_provider(&self, provider: crate::rpc::LoginProvider) -> Result<Box<dyn LoginStrategy>, AdapterError>;
}
