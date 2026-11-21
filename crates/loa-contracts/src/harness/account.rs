//! Sign-in, model config and construction of one harness (ports only; `loa-harness` implements them).

use super::{Adapter, AdapterError};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Whether a harness has usable credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInState {
    /// `method` names the credential kind (subscription, api key, …).
    SignedIn { method: String },
    SignedOut,
    /// Never holds CLI output that could contain a credential.
    Unknown { reason: String },
}

/// A way to sign in.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum LoginKind {
    Browser,
    DeviceCode,
    Console,
    Sso,
}

/// A program invocation; the child inherits the app's environment plus `env`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub current_dir: Option<PathBuf>,
}

/// The model and effort a harness is configured with outside any session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelSettings {
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// What the sign-in screen sends to a running login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginControl {
    SubmitInput(String),
    Cancel,
}

/// Progress a running login reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginUpdate {
    Url(String),
    Progress(String),
    Prompt { message: String, placeholder: Option<String> },
    Succeeded(Option<String>),
    Failed(String),
    Cancelled,
}

/// One login flow, run to its end.
#[async_trait]
pub trait LoginStrategy: Send {
    async fn run(self: Box<Self>, ctl: mpsc::Receiver<LoginControl>, out: mpsc::Sender<LoginUpdate>);
}

/// Sign-in state and model config of one harness.
#[async_trait]
pub trait HarnessAccount: Send + Sync {
    async fn sign_in_state(&self) -> SignInState;
    fn login(&self, kind: LoginKind) -> Result<Box<dyn LoginStrategy>, AdapterError>;
    fn logout_command(&self) -> Option<CommandSpec>;
    fn efforts(&self) -> &'static [&'static str];
    fn read_model(&self) -> Result<ModelSettings, AdapterError>;
    fn write_model(&self, m: &ModelSettings) -> Result<(), AdapterError>;
}

/// Per-harness behavior the app queries instead of comparing harness ids.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct HarnessTraits {
    pub approval_modes: &'static [&'static str],
    pub has_roles: bool,
    pub server_url: bool,
}

/// Builds the adapter and account of one harness; the registry entry for it.
pub trait HarnessFactory: Send + Sync {
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str;
    /// Icon token, e.g. `harness.claude`.
    fn icon_token(&self) -> &'static str;
    fn default_program(&self) -> &'static str;
    fn build(&self, exe: Option<PathBuf>) -> Arc<dyn Adapter>;
    fn account(&self, exe: Option<PathBuf>) -> Option<Arc<dyn HarnessAccount>>;
    fn traits(&self) -> HarnessTraits;
}
