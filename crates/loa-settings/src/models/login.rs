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

/// A stored sign in that signing out of a row removes. Only its description;
/// the credential itself stays in the harness's store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credential {
    /// omp's credential id; `None` for a harness that keeps one sign in and
    /// removes it with its logout command.
    pub id: Option<i64>,
    pub label: String,
    pub detail: String,
    /// Whether the harness signs in with it now.
    pub active: bool,
}

/// omp's sign-in account: a [`HarnessAccount`] that also logs in to one of
/// the OAuth providers omp lists.
pub trait OmpProviderAccount: HarnessAccount {
    fn login_provider(&self, provider: crate::models::rpc::LoginProvider) -> Result<Box<dyn LoginStrategy>, AdapterError>;
}
