//! The models screen: provider sign in ([`signin`]), the session's model ([`picker`]) and omp's
//! model settings ([`settings`]), mirrored from the model settings domain.

use std::fmt;

/// One model, named the way omp's `set_model` takes it: a provider id and the model id within
/// that provider.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self { Self { provider: provider.into(), id: id.into() } }

    /// The `provider/id` selector.
    pub fn selector(&self) -> String { format!("{}/{}", self.provider, self.id) }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}/{}", self.provider, self.id) }
}

/// A model the user can pick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub model: ModelRef,
    /// Display name, e.g. "Claude Sonnet 4.5". Empty falls back to the id.
    pub name: String,
    /// Display name of the provider, e.g. "Anthropic". Empty falls back to the provider id.
    pub provider_name: String,
}

impl ModelInfo {
    pub fn display_name(&self) -> &str { if self.name.is_empty() { &self.model.id } else { &self.name } }
    pub fn provider_display_name(&self) -> &str { if self.provider_name.is_empty() { &self.model.provider } else { &self.provider_name } }
}

/// A model role (`default`, `smol`, `plan`, …) and the model it resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleAssignment {
    pub role: String,
    pub model: ModelRef,
}

/// The models and role assignments the picker offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    pub models: Vec<ModelInfo>,
    pub roles: Vec<RoleAssignment>,
}

/// The session's model as its `<session>` element records it (`model`, `model_role`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionModel {
    /// Model selector, `provider/id` or a bare id some harnesses write.
    pub model: Option<String>,
    /// The role the model was picked through, when it follows one.
    pub role: Option<String>,
}

/// A model picked for the session: through `role` it follows that role, without one it follows none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionModelChange {
    pub model: ModelRef,
    pub role: Option<String>,
}

/// Identifies one sign in row: the harness that owns the login and its provider id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProviderKey {
    pub harness: String,
    pub provider: String,
}

impl ProviderKey {
    pub fn new(harness: impl Into<String>, provider: impl Into<String>) -> Self { Self { harness: harness.into(), provider: provider.into() } }
}

/// How a provider authenticates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    /// Subscription login (OAuth) run by the harness.
    Login { signed_in: bool, account: Option<String> },
    /// Provider authenticated by an API key the harness reads itself.
    ApiKey {
        /// Where the key was found; `None` when no key is configured.
        configured_via: Option<String>,
        /// Tells the user where to put the key.
        instructions: String,
    },
    /// No sign in can be offered now.
    Unavailable { reason: String },
}

/// One provider row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderEntry {
    pub key: ProviderKey,
    /// Display name of the provider, e.g. "Anthropic".
    pub name: String,
    /// Display name of the owning harness; rows are grouped under it.
    pub harness_name: String,
    pub auth: Auth,
}

/// What a row shows about its login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginState {
    Idle,
    /// Sign in requested, waiting for the URL; progress so far is carried into `Browser`.
    Starting { progress: Option<String> },
    /// Browser step: URL shown and copyable, waiting for the user to finish.
    Browser {
        url: String,
        /// The URL the Copy button writes: the launch URL when the harness sent one, the full URL otherwise.
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
    pub fn is_running(&self) -> bool { matches!(self, Self::Starting { .. } | Self::Browser { .. }) }
}

/// A pending prompt of a running login and what the user typed so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginPrompt {
    pub message: String,
    pub placeholder: Option<String>,
    pub input: String,
}

/// Progress of a running login, as the harness reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginUpdate {
    Url { url: String, launch_url: Option<String>, instructions: Option<String> },
    Progress(String),
    Prompt { message: String, placeholder: Option<String> },
    Succeeded { account: Option<String> },
    Failed(String),
    Cancelled,
}

/// Everything the model settings page edits.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ModelSettings {
    pub roles: Vec<RoleChain>,
    pub models: Vec<ModelToggle>,
    pub providers: Vec<ProviderToggle>,
    pub overrides: Vec<ModelOverride>,
    pub custom_providers: Vec<CustomProvider>,
    /// The other harnesses' own default model settings.
    pub harnesses: Vec<HarnessModel>,
}

/// The default model and effort another harness keeps in its own config file. Empty values
/// leave the choice to the harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessModel {
    /// Harness key, e.g. `claude`.
    pub harness: String,
    /// Display name, e.g. "Claude Code".
    pub name: String,
    /// The config file the values live in, for display.
    pub config_path: String,
    pub model: String,
    pub effort: String,
    /// Effort values the harness accepts in its config.
    pub efforts: Vec<String>,
}

/// A model role and its ordered chain: `chain[0]` is the primary model, the rest its fallbacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleChain {
    pub role: String,
    pub chain: Vec<String>,
}

/// Whether a model selector is enabled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelToggle {
    pub selector: String,
    pub enabled: bool,
}

/// A provider and whether it is enabled, in provider order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderToggle {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

/// The override fields set for one model selector.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelOverride {
    pub selector: String,
    pub fields: Vec<OverrideField>,
}

/// One overridden model field, e.g. `contextWindow: 200000`.
#[derive(Clone, Debug, PartialEq)]
pub struct OverrideField {
    pub key: String,
    pub value: FieldValue,
}

/// A typed override value.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// A custom provider. The API key is only ever the name of the environment variable holding it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CustomProvider {
    pub id: String,
    pub base_url: String,
    pub api: String,
    pub auth: AuthMode,
    pub api_key_env: String,
    pub models: Vec<String>,
}

/// A problem the app (or omp) reported for a saved settings value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    pub location: String,
    pub message: String,
}

loa_util::str_enum! {
    /// How a custom provider authenticates.
    #[derive(Default)]
    pub AuthMode { #[default] ApiKey => "apiKey", None => "none", OAuth => "oauth" }
}

loa_util::str_enum! {
    /// Names an override field.
    pub OverrideKey {
        Name => "name",
        ContextWindow => "contextWindow",
        MaxContextWindow => "maxContextWindow",
        MaxTokens => "maxTokens",
        Reasoning => "reasoning",
        Input => "input",
        ContextPromotionTarget => "contextPromotionTarget",
        CompactionModel => "compactionModel",
        PreferWebsockets => "preferWebsockets",
        Cost => "cost",
    }
}

/// The override fields the settings page edits, with the type each is edited as.
pub const OVERRIDE_FIELDS: [(&str, OverrideKey); 8] = [
    ("name", OverrideKey::Name),
    ("contextWindow", OverrideKey::ContextWindow),
    ("maxContextWindow", OverrideKey::MaxContextWindow),
    ("maxTokens", OverrideKey::MaxTokens),
    ("reasoning", OverrideKey::Reasoning),
    ("contextPromotionTarget", OverrideKey::ContextPromotionTarget),
    ("compactionModel", OverrideKey::CompactionModel),
    ("preferWebsockets", OverrideKey::PreferWebsockets),
];

/// Whether `name` looks like an environment variable name by convention.
pub fn is_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase() || c == '_') && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Whether `id` is a provider id omp accepts: letters, digits, `-`, `_` or `.`.
pub fn is_provider_id(id: &str) -> bool { !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) }

/// The sign in page.
pub mod signin {
    use super::ProviderKey;

    /// What the app acts on.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Intent {
        /// Start this provider's login with its harness.
        SignIn(ProviderKey),
        /// Abort the running login.
        Cancel(ProviderKey),
        /// Answer the running login's prompt.
        SubmitInput { key: ProviderKey, text: String },
        /// Open a login's URL in the system browser.
        OpenUrl(String),
        /// Put a login's URL on the clipboard.
        CopyUrl(String),
    }
}

/// The model picker.
pub mod picker {
    use super::{ModelRef, SessionModelChange};

    /// What a pick changes.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Target {
        /// The session's model; roles are rows too, and picking one makes the session follow it.
        Session,
        /// The model a role resolves to.
        Role(String),
    }

    /// What the app acts on.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Intent {
        /// Switch the session's model, following the change's role (or none).
        SetSessionModel(SessionModelChange),
        /// Assign `model` to `role` in the harness config.
        SetRoleModel { role: String, model: ModelRef },
        /// Dismissed without a pick.
        Close,
    }
}

/// The model settings page.
pub mod settings {
    use super::ModelSettings;

    /// What the app acts on.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Intent {
        /// Write these settings, then report the result back to the page.
        Save(ModelSettings),
        /// The unsaved changes were dropped.
        Reverted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids_follow_omps_rules() {
        assert!(is_env_var_name("OPENAI_API_KEY") && is_env_var_name("_X1"));
        assert!(!is_env_var_name("sk-ant-api03") && !is_env_var_name("1KEY") && !is_env_var_name(""));
        assert!(is_provider_id("local-llm.v1_x") && !is_provider_id("my provider") && !is_provider_id(""));
        assert_eq!(ModelRef::new("openai", "gpt-5").selector(), ModelRef::new("openai", "gpt-5").to_string());
    }
}
