//! Reports credential *sources* without exposing their values. Omp owns all
//! credential resolution and login storage; this module never handles keys.

use crate::models_yml::{ApiKeySetting, ModelsYml};

/// A source omp is configured to consult for a provider key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// `models.yml` names an environment variable.
    ConfigEnvironment(String),
    /// `models.yml` runs a secret command; its text and output stay private.
    SecretCommand,
    /// `models.yml` contains an inline value, which is never returned.
    ConfigEntry,
    /// Omp's built-in provider variable is present.
    Environment(String),
    /// No configured source detected. Stored omp credentials are not inspected.
    NotConfigured,
}

/// Checks presence only, never yields the environment variable's contents.
pub trait EnvironmentPresence {
    fn contains(&self, name: &str) -> bool;
}

/// Presence from the current process environment; no values leave this method.
pub struct ProcessEnvironment;
impl EnvironmentPresence for ProcessEnvironment {
    fn contains(&self, name: &str) -> bool {
        std::env::var_os(name).is_some_and(|v| !v.is_empty())
    }
}

/// Built-in credential environment names from omp's provider catalog. Custom
/// providers can pass their own variable names in `environment_names`.
/// A provider's OAuth or key stored in omp's agent.db is not inspected by this
/// read-only interface; `get_login_providers` reports signed-in OAuth state.
pub fn credential_source(
    models: &ModelsYml,
    provider: &str,
    environment_names: &[&str],
    env: &dyn EnvironmentPresence,
) -> KeySource {
    if let Some(key) = models.provider(provider).and_then(|p| p.api_key) {
        return match key {
            ApiKeySetting::EnvVar(name) => KeySource::ConfigEnvironment(name),
            ApiKeySetting::Command => KeySource::SecretCommand,
            ApiKeySetting::Literal => KeySource::ConfigEntry,
        };
    }
    environment_names.iter().find(|name| env.contains(name))
        .map_or(KeySource::NotConfigured, |name| KeySource::Environment((*name).to_owned()))
}

/// Human-readable setup directions for the selected source, without a key.
pub fn key_instructions(provider: &str, source: &KeySource, environment_names: &[&str]) -> String {
    match source {
        KeySource::SecretCommand => "Configured through an omp models.yml secret command. Manage the credential with the command's own secret manager; omp runs it when making a request.".into(),
        KeySource::ConfigEnvironment(name) => format!("Configured through models.yml: set {name} in your shell or an omp .env file before starting omp."),
        KeySource::ConfigEntry => "Configured in omp models.yml. For safer storage, replace the inline value with an environment variable name or a !secret command.".into(),
        KeySource::Environment(name) => format!("Configured through {name}. Manage it in your shell or an omp .env file; restart omp to pick up changes."),
        KeySource::NotConfigured if !environment_names.is_empty() => format!("Set {} in your shell or an omp .env file, or configure providers.{provider}.apiKey in omp models.yml as an environment variable name or !secret command. Never paste a key into this app.", environment_names.join(" or ")),
        KeySource::NotConfigured => format!("Use omp's own login or configure providers.{provider}.apiKey in omp models.yml as an environment variable name or !secret command. Never paste a key into this app."),
    }
}

/// Known provider key names, in omp's credential fallback order. An empty
/// slice means the provider uses its own login or has no ordinary API key.
pub fn builtin_environment_names(provider: &str) -> &'static [&'static str] {
    match provider {
        "anthropic" => &["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "openai-codex" => &["OPENAI_CODEX_OAUTH_TOKEN"],
        "google" => &["GEMINI_API_KEY"],
        "groq" => &["GROQ_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "together" => &["TOGETHER_API_KEY"],
        "fireworks" => &["FIREWORKS_API_KEY"],
        "cerebras" => &["CEREBRAS_API_KEY"],
        "ollama" => &["OLLAMA_API_KEY"],
        "lm-studio" => &["LM_STUDIO_API_KEY"],
        "llama.cpp" => &["LLAMA_CPP_API_KEY"],
        "litellm" => &["LITELLM_API_KEY"],
        "bifrost" => &["BIFROST_API_KEY"],
        "typesafe" => &["TYPESAFE_API_KEY"],
        "azure" => &["AZURE_OPENAI_API_KEY"],
        "github-copilot" => &["COPILOT_GITHUB_TOKEN"],
        "cursor" => &["CURSOR_ACCESS_TOKEN"],
        "zai" => &["ZAI_API_KEY"],
        "moonshot" => &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        "huggingface" => &["HUGGINGFACE_HUB_TOKEN", "HF_TOKEN"],
        "amazon-bedrock" => &["AWS_BEARER_TOKEN_BEDROCK"],
        "google-vertex" => &["GOOGLE_CLOUD_API_KEY"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    struct Env(HashSet<String>);
    impl EnvironmentPresence for Env { fn contains(&self, name: &str) -> bool { self.0.contains(name) } }
    #[test]
    fn key_sources_never_include_the_inline_secret_or_environment_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yml");
        std::fs::write(&path, "providers:\n  gateway:\n    apiKey: sk-supersecret-example\n  proxy:\n    apiKey: '!op read private'\n").unwrap();
        let models = ModelsYml::load(&path).unwrap();
        let env = Env(["OPENAI_API_KEY".into()].into());
        assert_eq!(credential_source(&models, "gateway", &[], &env), KeySource::ConfigEntry);
        assert_eq!(credential_source(&models, "proxy", &[], &env), KeySource::SecretCommand);
        assert_eq!(credential_source(&models, "openai", &["OPENAI_API_KEY"], &env), KeySource::Environment("OPENAI_API_KEY".into()));
        assert!(!format!("{:?}", credential_source(&models, "gateway", &[], &env)).contains("sk-supersecret"));
    }
}
