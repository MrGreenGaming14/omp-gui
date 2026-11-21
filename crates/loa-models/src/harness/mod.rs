//! Sign in and model settings for the harnesses other than omp: Claude Code
//! ([`claude`]) and Codex ([`codex`]).
//!
//! Sign in goes through each harness's own CLI: [`HarnessAuth`] reads the
//! signed in state from its status command and builds the login and logout
//! commands the app runs in its terminal view. The app never handles credentials
//! or API keys; it only learns which kind of sign in is active.
//!
//! Model settings retain unrelated keys and ordering; TOML comments are kept.

pub mod claude;
pub mod codex;
pub(crate) mod configdoc;

use async_trait::async_trait;

use crate::Result;
use crate::process::{CommandSpec, RunningProcess};

/// A harness other than omp that the app can sign in to and configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HarnessId {
    /// Anthropic's Claude Code (`claude`).
    ClaudeCode,
    /// OpenAI's Codex CLI (`codex`).
    Codex,
}

/// Whether a harness has usable credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInState {
    /// Credentials are present.
    SignedIn {
        /// The kind of credentials in use.
        method: SignInMethod,
    },
    /// No credentials are present.
    SignedOut,
    /// The state could not be determined: the CLI could not be run, exited
    /// abnormally or printed something this crate does not understand.
    Unknown {
        /// Why the state is unknown, for display. Never contains CLI output that
        /// could hold a credential.
        reason: String,
    },
}

/// The kind of credentials a harness is signed in with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInMethod {
    /// A consumer subscription account (Claude.ai, ChatGPT).
    Subscription,
    /// An API key, from the environment or stored by the harness.
    ApiKey,
    /// A helper command that prints an API key (Claude Code `apiKeyHelper`).
    ApiKeyHelper,
    /// A long lived OAuth token (Claude Code `CLAUDE_CODE_OAUTH_TOKEN`).
    OAuthToken,
    /// A third party provider such as Bedrock or Vertex.
    ThirdParty,
    /// A method this crate does not map: the harness's own name for it, or
    /// `unknown` when the harness did not name it in a form safe to keep.
    Other(String),
}

/// A way to sign in to a harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoginKind {
    /// Browser OAuth flow with the default account type.
    Browser,
    /// Device code flow for machines without a browser (Codex).
    DeviceCode,
    /// Anthropic Console account billed by API usage (Claude Code `--console`).
    Console,
    /// Single sign on (Claude Code `--sso`).
    Sso,
}

/// Sign in state and login commands of one harness.
#[async_trait]
pub trait HarnessAuth: Send + Sync {
    /// Which harness this is.
    fn id(&self) -> HarnessId;

    /// Runs the harness's status command and reports whether it is signed in.
    async fn sign_in_state(&self) -> Result<SignInState>;

    /// The command that signs in with `kind`; [`crate::Error::Invalid`] when the
    /// harness has no such login.
    fn login_command(&self, kind: LoginKind) -> Result<CommandSpec>;

    /// Starts [`HarnessAuth::login_command`] so the app can show it in its
    /// terminal view and forward the user's input (some flows ask to paste a
    /// code).
    async fn start_login(&self, kind: LoginKind) -> Result<Box<dyn RunningProcess>>;

    /// The command that signs out.
    fn logout_command(&self) -> CommandSpec;
}

/// Checks a model name before it is written to a harness config: not empty and
/// free of whitespace and control characters.
pub(crate) fn validate_model_name(field: &str, name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(crate::Error::invalid(field, "model name is empty"));
    }
    if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(crate::Error::invalid(
            field,
            "model name contains whitespace or control characters",
        ));
    }
    Ok(())
}
