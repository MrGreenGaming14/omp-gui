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

pub use loa_contracts::harness::{LoginKind, SignInMethod, SignInState};

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
