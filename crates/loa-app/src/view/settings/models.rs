//! Model settings, sign in and picker data as [`loa_view::settings::models`] views, and the
//! edited views back as domain values.

use loa_contracts::harness::LoginUpdate;
use loa_settings::models::login::{Auth, Credential, ProviderEntry, ProviderKey};
use loa_settings::models::selector::{Catalog, ModelInfo, ModelRef, RoleAssignment};
use loa_settings::models::settings::{
    AuthMode, CustomProvider, FieldValue, HarnessModel, ModelOverride, ModelSettings, ModelToggle, OverrideField, ProviderToggle, RoleChain, ValidationError,
};
use loa_view::settings::models as view;

pub fn model_ref_to_view(model: &ModelRef) -> view::ModelRef { view::ModelRef { provider: model.provider.clone(), id: model.id.clone() } }

pub fn model_ref_from_view(model: view::ModelRef) -> ModelRef { ModelRef { provider: model.provider, id: model.id } }

pub fn catalog_to_view(catalog: &Catalog) -> view::Catalog {
    view::Catalog {
        models: catalog.models.iter().map(|m: &ModelInfo| view::ModelInfo { model: model_ref_to_view(&m.model), name: m.name.clone(), provider_name: m.provider_name.clone() }).collect(),
        roles: catalog.roles.iter().map(|r: &RoleAssignment| view::RoleAssignment { role: r.role.clone(), model: model_ref_to_view(&r.model) }).collect(),
    }
}

pub fn provider_key_to_view(key: &ProviderKey) -> view::ProviderKey { view::ProviderKey { harness: key.harness.clone(), provider: key.provider.clone() } }

pub fn provider_key_from_view(key: view::ProviderKey) -> ProviderKey { ProviderKey { harness: key.harness, provider: key.provider } }

pub fn provider_to_view(entry: &ProviderEntry) -> view::ProviderEntry {
    let auth = match &entry.auth {
        Auth::Login { signed_in, account } => view::Auth::Login { signed_in: *signed_in, account: account.clone() },
        Auth::ApiKey { configured_via, instructions } => view::Auth::ApiKey { configured_via: configured_via.clone(), instructions: instructions.clone() },
        Auth::Unavailable { reason } => view::Auth::Unavailable { reason: reason.clone() },
    };
    view::ProviderEntry { key: provider_key_to_view(&entry.key), name: entry.name.clone(), harness_name: entry.harness_name.clone(), auth }
}

pub fn credential_to_view(credential: &Credential) -> view::Credential {
    view::Credential { id: credential.id, label: credential.label.clone(), detail: credential.detail.clone(), active: credential.active }
}

pub fn login_update_to_view(update: LoginUpdate) -> view::LoginUpdate {
    match update {
        LoginUpdate::Url { url, launch_url, instructions } => view::LoginUpdate::Url { url, launch_url, instructions },
        LoginUpdate::Progress(message) => view::LoginUpdate::Progress(message),
        LoginUpdate::Prompt { message, placeholder } => view::LoginUpdate::Prompt { message, placeholder },
        LoginUpdate::Succeeded { account } => view::LoginUpdate::Succeeded { account },
        LoginUpdate::Failed(message) => view::LoginUpdate::Failed(message),
        LoginUpdate::Cancelled => view::LoginUpdate::Cancelled,
    }
}

pub fn validation_error_to_view(error: &ValidationError) -> view::ValidationError { view::ValidationError { location: error.location.clone(), message: error.message.clone() } }

fn auth_mode_to_view(mode: AuthMode) -> view::AuthMode {
    match mode {
        AuthMode::ApiKey => view::AuthMode::ApiKey,
        AuthMode::None => view::AuthMode::None,
        AuthMode::OAuth => view::AuthMode::OAuth,
    }
}

fn auth_mode_from_view(mode: view::AuthMode) -> AuthMode {
    match mode {
        view::AuthMode::ApiKey => AuthMode::ApiKey,
        view::AuthMode::None => AuthMode::None,
        view::AuthMode::OAuth => AuthMode::OAuth,
    }
}

fn field_value_to_view(value: &FieldValue) -> view::FieldValue {
    match value {
        FieldValue::Text(text) => view::FieldValue::Text(text.clone()),
        FieldValue::Int(n) => view::FieldValue::Int(*n),
        FieldValue::Float(n) => view::FieldValue::Float(*n),
        FieldValue::Bool(b) => view::FieldValue::Bool(*b),
    }
}

fn field_value_from_view(value: view::FieldValue) -> FieldValue {
    match value {
        view::FieldValue::Text(text) => FieldValue::Text(text),
        view::FieldValue::Int(n) => FieldValue::Int(n),
        view::FieldValue::Float(n) => FieldValue::Float(n),
        view::FieldValue::Bool(b) => FieldValue::Bool(b),
    }
}

pub fn settings_to_view(s: &ModelSettings) -> view::ModelSettings {
    view::ModelSettings {
        roles: s.roles.iter().map(|r| view::RoleChain { role: r.role.clone(), chain: r.chain.clone() }).collect(),
        models: s.models.iter().map(|m| view::ModelToggle { selector: m.selector.clone(), enabled: m.enabled }).collect(),
        providers: s.providers.iter().map(|p| view::ProviderToggle { id: p.id.clone(), name: p.name.clone(), enabled: p.enabled }).collect(),
        overrides: s
            .overrides
            .iter()
            .map(|o| view::ModelOverride { selector: o.selector.clone(), fields: o.fields.iter().map(|f| view::OverrideField { key: f.key.clone(), value: field_value_to_view(&f.value) }).collect() })
            .collect(),
        custom_providers: s
            .custom_providers
            .iter()
            .map(|p| view::CustomProvider { id: p.id.clone(), base_url: p.base_url.clone(), api: p.api.clone(), auth: auth_mode_to_view(p.auth), api_key_env: p.api_key_env.clone(), models: p.models.clone() })
            .collect(),
        harnesses: s
            .harnesses
            .iter()
            .map(|h| view::HarnessModel { harness: h.harness.clone(), name: h.name.clone(), config_path: h.config_path.clone(), model: h.model.clone(), effort: h.effort.clone(), efforts: h.efforts.clone() })
            .collect(),
    }
}

pub fn settings_from_view(s: view::ModelSettings) -> ModelSettings {
    ModelSettings {
        roles: s.roles.into_iter().map(|r| RoleChain { role: r.role, chain: r.chain }).collect(),
        models: s.models.into_iter().map(|m| ModelToggle { selector: m.selector, enabled: m.enabled }).collect(),
        providers: s.providers.into_iter().map(|p| ProviderToggle { id: p.id, name: p.name, enabled: p.enabled }).collect(),
        overrides: s
            .overrides
            .into_iter()
            .map(|o| ModelOverride { selector: o.selector, fields: o.fields.into_iter().map(|f| OverrideField { key: f.key, value: field_value_from_view(f.value) }).collect() })
            .collect(),
        custom_providers: s
            .custom_providers
            .into_iter()
            .map(|p| CustomProvider { id: p.id, base_url: p.base_url, api: p.api, auth: auth_mode_from_view(p.auth), api_key_env: p.api_key_env, models: p.models })
            .collect(),
        harnesses: s
            .harnesses
            .into_iter()
            .map(|h| HarnessModel { harness: h.harness, name: h.name, config_path: h.config_path, model: h.model, effort: h.effort, efforts: h.efforts })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_settings_round_trip() {
        let settings = ModelSettings {
            roles: vec![RoleChain { role: "default".into(), chain: vec!["anthropic/claude-sonnet-4-5:high".into(), "openai/gpt-5".into()] }],
            models: vec![ModelToggle { selector: "openai/gpt-5".into(), enabled: true }, ModelToggle { selector: "google/gemini".into(), enabled: false }],
            providers: vec![ProviderToggle { id: "anthropic".into(), name: "Anthropic".into(), enabled: true }],
            overrides: vec![ModelOverride {
                selector: "openai/gpt-5".into(),
                fields: vec![
                    OverrideField { key: "name".into(), value: FieldValue::Text("GPT".into()) },
                    OverrideField { key: "contextWindow".into(), value: FieldValue::Int(400_000) },
                    OverrideField { key: "temperature".into(), value: FieldValue::Float(0.5) },
                    OverrideField { key: "reasoning".into(), value: FieldValue::Bool(true) },
                ],
            }],
            custom_providers: [AuthMode::ApiKey, AuthMode::None, AuthMode::OAuth]
                .into_iter()
                .enumerate()
                .map(|(i, auth)| CustomProvider { id: format!("local-{i}"), base_url: "http://localhost/v1".into(), api: "openai-completions".into(), auth, api_key_env: "LOCAL_KEY".into(), models: vec!["qwen3".into()] })
                .collect(),
            harnesses: vec![HarnessModel { harness: "codex".into(), name: "Codex".into(), config_path: "~/.codex/config.toml".into(), model: "o3".into(), effort: "high".into(), efforts: vec!["low".into(), "high".into()] }],
        };
        assert_eq!(settings_from_view(settings_to_view(&settings)), settings);
    }

    #[test]
    fn model_refs_and_provider_keys_round_trip() {
        let model = ModelRef::new("openrouter", "vendor/model");
        assert_eq!(model_ref_from_view(model_ref_to_view(&model)), model);
        let key = ProviderKey::new("omp", "anthropic");
        assert_eq!(provider_key_from_view(provider_key_to_view(&key)), key);
    }
}
