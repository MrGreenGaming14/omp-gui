//! Conversions between omp's settings files and catalog (`loa-models`) and the
//! plain data the screens of this crate show.
//!
//! Reading builds [`ModelSettings`] from `config.yml`, `models.yml` and the
//! available models. Writing is a diff: [`apply_settings`] compares what the
//! screen started from with what it saves and edits only the keys that changed,
//! so settings the screen does not show (path scoped entries, globs, secret
//! key commands, model metadata, headers) stay as they are.


use std::collections::{BTreeMap, HashMap};
use crate::config::{ChainKey, OmpConfig, ScopedEntry, ScopedList};
use crate::selector::{Selector, ModelRef};
use crate::models_yml::{self, ApiKeySetting, ApiKeyWrite, AuthMode, KeyReference, ModelsYml, OverrideKey};
use crate::rpc;
use std::fmt;

/// Everything the screen edits, as the app reads it from omp's config files.
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

/// The default model and effort another harness (Claude Code, Codex) keeps in
/// its own config file. Empty values leave the choice to the harness.
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
/// A model role and its ordered chain: `chain[0]` is the primary model
/// (`modelRoles.<role>`), the rest are its fallbacks
/// (`retry.fallbackChains.<role>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleChain {
    pub role: String,
    pub chain: Vec<String>,
}

/// Whether a model selector is listed in `enabledModels`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelToggle {
    pub selector: String,
    pub enabled: bool,
}

/// A provider and whether it is enabled. In provider order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderToggle {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

/// The `modelOverrides` fields set for one model selector.
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

/// How omp authenticates against a custom provider (`auth` in models.yml).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ProviderAuth {
    #[default]
    ApiKey,
    None,
    OAuth,
}

impl ProviderAuth {
    /// All variants, in the order the auth pick list shows them.
    pub const ALL: &'static [ProviderAuth] = &[Self::ApiKey, Self::None, Self::OAuth];
}

impl fmt::Display for ProviderAuth {
    /// Spelled the way omp writes the value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ApiKey => "apiKey",
            Self::None => "none",
            Self::OAuth => "oauth",
        })
    }
}

/// A provider defined in models.yml. The API key is only ever the name of an
/// environment variable that holds it; the app never handles the key itself.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CustomProvider {
    pub id: String,
    pub base_url: String,
    pub api: String,
    pub auth: ProviderAuth,
    pub api_key_env: String,
    pub models: Vec<String>,
}

/// An error the app's validation (or omp) reported for a saved settings value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    pub location: String,
    pub message: String,
}

/// The override fields the screen edits, with the type each is edited as.
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

fn error(location: impl Into<String>, message: impl ToString) -> ValidationError {
    ValidationError { location: location.into(), message: message.to_string() }
}

/// Whether `pattern` (an `enabledModels` entry: exact selector, bare id or
/// glob) selects the model `provider/id`.
pub(crate) fn pattern_matches(pattern: &str, provider: &str, id: &str) -> bool {
    let selector = format!("{provider}/{id}");
    let glob = |text: &str| glob_match(pattern.as_bytes(), text.as_bytes());
    pattern == selector || pattern == id || glob(&selector) || (!pattern.contains('/') && glob(id))
}

fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.split_first(), text.split_first()) {
        (None, None) => true,
        (Some((b'*', rest)), _) => glob_match(rest, text) || (!text.is_empty() && glob_match(pattern, &text[1..])),
        (Some((b'?', rest)), Some((_, text_rest))) => glob_match(rest, text_rest),
        (Some((p, rest)), Some((t, text_rest))) => p == t && glob_match(rest, text_rest),
        _ => false,
    }
}

/// Reads the settings the screen shows.
pub fn settings(
    config: &OmpConfig,
    models: &ModelsYml,
    available: &[rpc::ModelInfo],
    provider_names: &HashMap<String, String>,
) -> Result<ModelSettings, String> {
    Ok(ModelSettings {
        roles: roles(config)?,
        models: model_toggles(config, available),
        providers: provider_toggles(config, available, provider_names),
        overrides: overrides(models),
        custom_providers: custom_providers(models),
        // Filled by the backend, which owns the harness configs.
        harnesses: Vec::new(),
    })
}

fn roles(config: &OmpConfig) -> Result<Vec<RoleChain>, String> {
    let chains = config.fallback_chains().map_err(|e| e.to_string())?;
    Ok(config
        .model_roles()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|assignment| {
            let primary = assignment.candidates.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
            let mut chain = vec![primary];
            if let Some(fallbacks) = chains.iter().find(|c| c.key == ChainKey::Role(assignment.role.clone())) {
                chain.extend(fallbacks.fallbacks.iter().map(ToString::to_string));
            }
            RoleChain { role: assignment.role, chain }
        })
        .collect())
}

fn everywhere(entries: &[ScopedEntry]) -> Vec<&str> {
    entries
        .iter()
        .filter_map(|e| match e {
            ScopedEntry::Everywhere(value) => Some(value.as_str()),
            ScopedEntry::Paths { .. } => None,
        })
        .collect()
}

fn model_toggles(config: &OmpConfig, available: &[rpc::ModelInfo]) -> Vec<ModelToggle> {
    let entries = config.scoped_list(ScopedList::EnabledModels);
    let patterns = everywhere(&entries);
    let mut toggles: Vec<ModelToggle> = available
        .iter()
        .map(|m| ModelToggle {
            selector: m.selector(),
            enabled: patterns.is_empty() || patterns.iter().any(|p| pattern_matches(p, &m.provider, &m.id)),
        })
        .collect();
    // Exact selectors of models omp does not list now (signed out provider,
    // stopped local server) stay visible so they can be turned off.
    for pattern in patterns {
        let exact = pattern.contains('/') && !pattern.contains(['*', '?']);
        if exact && !toggles.iter().any(|t| t.selector == pattern) {
            toggles.push(ModelToggle { selector: pattern.to_owned(), enabled: true });
        }
    }
    toggles
}

fn provider_toggles(
    config: &OmpConfig,
    available: &[rpc::ModelInfo],
    provider_names: &HashMap<String, String>,
) -> Vec<ProviderToggle> {
    let disabled_entries = config.scoped_list(ScopedList::DisabledProviders);
    let disabled = everywhere(&disabled_entries);
    let mut ids: Vec<String> = config.provider_order();
    for id in available.iter().map(|m| m.provider.as_str()).chain(disabled.iter().copied()) {
        if !ids.iter().any(|known| known == id) {
            ids.push(id.to_owned());
        }
    }
    ids.into_iter()
        .map(|id| ProviderToggle {
            name: provider_names.get(&id).cloned().unwrap_or_default(),
            enabled: !disabled.contains(&id.as_str()),
            id,
        })
        .collect()
}

fn override_fields(o: &models_yml::ModelOverride) -> Vec<OverrideField> {
    let int = |v: Option<u64>| v.map(|v| FieldValue::Int(i64::try_from(v).unwrap_or(i64::MAX)));
    let values = [
        o.name.clone().map(FieldValue::Text),
        int(o.context_window),
        int(o.max_context_window),
        int(o.max_tokens),
        o.reasoning.map(FieldValue::Bool),
        o.context_promotion_target.clone().map(FieldValue::Text),
        o.compaction_model.clone().map(FieldValue::Text),
        o.prefer_websockets.map(FieldValue::Bool),
    ];
    OVERRIDE_FIELDS
        .iter()
        .zip(values)
        .filter_map(|((key, _), value)| Some(OverrideField { key: (*key).to_owned(), value: value? }))
        .collect()
}

fn overrides(models: &ModelsYml) -> Vec<ModelOverride> {
    models
        .providers()
        .into_iter()
        .flat_map(|entry| {
            entry.overrides.into_iter().map(move |(model, o)| ModelOverride {
                selector: format!("{}/{model}", entry.id),
                fields: override_fields(&o),
            })
        })
        .collect()
}

fn is_custom(entry: &models_yml::ProviderEntry) -> bool {
    entry.base_url.is_some() || entry.api.is_some() || entry.discovery.is_some() || !entry.models.is_empty()
}

fn ui_auth(auth: Option<AuthMode>) -> ProviderAuth {
    match auth {
        None | Some(AuthMode::ApiKey) => ProviderAuth::ApiKey,
        Some(AuthMode::None) => ProviderAuth::None,
        Some(AuthMode::OAuth) => ProviderAuth::OAuth,
    }
}

fn file_auth(auth: ProviderAuth) -> AuthMode {
    match auth {
        ProviderAuth::ApiKey => AuthMode::ApiKey,
        ProviderAuth::None => AuthMode::None,
        ProviderAuth::OAuth => AuthMode::OAuth,
    }
}

fn custom_providers(models: &ModelsYml) -> Vec<CustomProvider> {
    models
        .providers()
        .into_iter()
        .filter(is_custom)
        .map(|entry| CustomProvider {
            api_key_env: match &entry.api_key {
                Some(ApiKeySetting::EnvVar(name)) => name.clone(),
                _ => String::new(),
            },
            base_url: entry.base_url.unwrap_or_default(),
            api: entry.api.unwrap_or_default(),
            auth: ui_auth(entry.auth),
            models: entry.models.into_iter().map(|m| m.id).collect(),
            id: entry.id,
        })
        .collect()
}

/// Which files [`apply_settings`] edited.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Edited {
    pub config: bool,
    pub models: bool,
}

/// Applies the difference between `before` (what the screen loaded) and
/// `after` (what it saves) to the loaded files. Nothing is written to disk.
pub fn apply_settings(
    config: &mut OmpConfig,
    models: &mut ModelsYml,
    before: &ModelSettings,
    after: &ModelSettings,
) -> Result<Edited, Vec<ValidationError>> {
    let mut errors = Vec::new();
    let mut edited = Edited::default();
    let mut track = |result: Result<bool, ValidationError>, file: &mut bool| match result {
        Ok(changed) => *file |= changed,
        Err(e) => errors.push(e),
    };
    for result in apply_roles(config, &before.roles, &after.roles) {
        track(result, &mut edited.config);
    }
    if before.models != after.models {
        track(apply_enabled_models(config, &after.models), &mut edited.config);
    }
    if before.providers != after.providers {
        track(apply_providers(config, &before.providers, &after.providers), &mut edited.config);
    }
    for result in apply_custom_providers(models, &before.custom_providers, &after.custom_providers) {
        track(result, &mut edited.models);
    }
    for result in apply_overrides(models, &before.overrides, &after.overrides) {
        track(result, &mut edited.models);
    }
    if errors.is_empty() { Ok(edited) } else { Err(errors) }
}

fn parse_selectors(location: &str, entries: &[String]) -> Result<Vec<Selector>, ValidationError> {
    entries
        .iter()
        .flat_map(|entry| entry.split(','))
        .filter(|s| !s.trim().is_empty())
        .map(|s| Selector::parse(s).map_err(|e| error(location, e)))
        .collect()
}

fn apply_roles(config: &mut OmpConfig, before: &[RoleChain], after: &[RoleChain]) -> Vec<Result<bool, ValidationError>> {
    let mut results = Vec::new();
    for old in before.iter().filter(|old| !after.iter().any(|new| new.role == old.role)) {
        let location = format!("Model roles › {}", old.role);
        results.push(config.clear_model_role(&old.role).map_err(|e| error(&location, e)));
        if old.chain.len() > 1 {
            results.push(
                config.clear_fallback_chain(&ChainKey::Role(old.role.clone())).map_err(|e| error(&location, e)),
            );
        }
    }
    for new in after {
        let old = before.iter().find(|old| old.role == new.role);
        if old == Some(new) {
            continue;
        }
        let location = format!("Model roles › {}", new.role);
        let key = ChainKey::Role(new.role.clone());
        let old_fallbacks = old.map_or(&[][..], |old| old.chain.get(1..).unwrap_or_default());
        let Some(primary) = new.chain.first() else {
            results.push(config.clear_model_role(&new.role).map_err(|e| error(&location, e)));
            if !old_fallbacks.is_empty() {
                results.push(config.clear_fallback_chain(&key).map_err(|e| error(&location, e)));
            }
            continue;
        };
        if old.and_then(|old| old.chain.first()) != Some(primary) {
            results.push(
                parse_selectors(&location, std::slice::from_ref(primary))
                    .and_then(|candidates| config.set_model_role(&new.role, &candidates).map_err(|e| error(&location, e)))
                    .map(|()| true),
            );
        }
        let fallbacks = &new.chain[1..];
        if fallbacks != old_fallbacks {
            results.push(if fallbacks.is_empty() {
                config.clear_fallback_chain(&key).map_err(|e| error(&location, e))
            } else {
                parse_selectors(&location, fallbacks)
                    .and_then(|chain| config.set_fallback_chain(&key, &chain).map_err(|e| error(&location, e)))
                    .map(|()| true)
            });
        }
    }
    results
}

fn apply_enabled_models(config: &mut OmpConfig, toggles: &[ModelToggle]) -> Result<bool, ValidationError> {
    const LOCATION: &str = "Enabled models";
    if !toggles.iter().any(|t| t.enabled) {
        return Err(error(LOCATION, "at least one model must stay enabled"));
    }
    let entries = config.scoped_list(ScopedList::EnabledModels);
    let mut list: Vec<ScopedEntry> =
        entries.iter().filter(|e| matches!(e, ScopedEntry::Paths { .. })).cloned().collect();
    if toggles.iter().any(|t| !t.enabled) {
        let matched = |pattern: &str, toggle: &ModelToggle| {
            ModelRef::split(&toggle.selector).is_some_and(|(provider, id)| pattern_matches(pattern, provider, id))
        };
        // Patterns that are not one of the listed selectors stay while every
        // listed model they select stays enabled.
        let kept: Vec<&str> = everywhere(&entries)
            .into_iter()
            .filter(|p| !toggles.iter().any(|t| t.selector == *p))
            .filter(|p| toggles.iter().filter(|t| matched(p, t)).all(|t| t.enabled))
            .collect();
        let mut everywhere: Vec<ScopedEntry> = kept.iter().map(|p| ScopedEntry::Everywhere((*p).to_owned())).collect();
        everywhere.extend(
            toggles
                .iter()
                .filter(|t| t.enabled && !kept.iter().any(|p| matched(p, t)))
                .map(|t| ScopedEntry::Everywhere(t.selector.clone())),
        );
        everywhere.append(&mut list);
        list = everywhere;
    }
    config.set_scoped_list(ScopedList::EnabledModels, &list).map_err(|e| error(LOCATION, e))?;
    Ok(true)
}

fn apply_providers(
    config: &mut OmpConfig,
    before: &[ProviderToggle],
    after: &[ProviderToggle],
) -> Result<bool, ValidationError> {
    const LOCATION: &str = "Providers";
    let ids = |list: &[ProviderToggle]| list.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
    let mut changed = false;
    if ids(before) != ids(after) {
        config.set_provider_order(&ids(after)).map_err(|e| error(LOCATION, e))?;
        changed = true;
    }
    let disabled = |list: &[ProviderToggle]| list.iter().filter(|p| !p.enabled).map(|p| p.id.clone()).collect::<Vec<_>>();
    let mut now_disabled = disabled(after);
    let mut was_disabled = disabled(before);
    now_disabled.sort();
    was_disabled.sort();
    if now_disabled != was_disabled {
        let entries = config.scoped_list(ScopedList::DisabledProviders);
        let mut list: Vec<ScopedEntry> = entries
            .iter()
            .filter(|e| match e {
                ScopedEntry::Everywhere(id) => !after.iter().any(|p| &p.id == id),
                ScopedEntry::Paths { .. } => true,
            })
            .cloned()
            .collect();
        list.extend(after.iter().filter(|p| !p.enabled).map(|p| ScopedEntry::Everywhere(p.id.clone())));
        config.set_scoped_list(ScopedList::DisabledProviders, &list).map_err(|e| error(LOCATION, e))?;
        changed = true;
    }
    Ok(changed)
}

fn override_field(key: &str, value: &FieldValue) -> Result<models_yml::OverrideField, String> {
    use models_yml::OverrideField as F;
    let (_, field) = OVERRIDE_FIELDS.iter().find(|(name, _)| *name == key).ok_or_else(|| {
        let names: Vec<&str> = OVERRIDE_FIELDS.iter().map(|(name, _)| *name).collect();
        format!("`{key}` is not an override field this screen edits ({})", names.join(", "))
    })?;
    let text = || match value {
        FieldValue::Text(s) => Ok(s.clone()),
        _ => Err(format!("`{key}` is text")),
    };
    let count = || match value {
        FieldValue::Int(n) if *n > 0 => Ok(n.unsigned_abs()),
        _ => Err(format!("`{key}` is a whole number above zero")),
    };
    let flag = || match value {
        FieldValue::Bool(b) => Ok(*b),
        _ => Err(format!("`{key}` is yes or no")),
    };
    Ok(match field {
        OverrideKey::Name => F::Name(text()?),
        OverrideKey::ContextWindow => F::ContextWindow(count()?),
        OverrideKey::MaxContextWindow => F::MaxContextWindow(count()?),
        OverrideKey::MaxTokens => F::MaxTokens(count()?),
        OverrideKey::Reasoning => F::Reasoning(flag()?),
        OverrideKey::ContextPromotionTarget => F::ContextPromotionTarget(text()?),
        OverrideKey::CompactionModel => F::CompactionModel(text()?),
        OverrideKey::PreferWebsockets => F::PreferWebsockets(flag()?),
        OverrideKey::Input | OverrideKey::Cost => unreachable!("not listed in OVERRIDE_FIELDS"),
    })
}

fn apply_overrides(
    models: &mut ModelsYml,
    before: &[ModelOverride],
    after: &[ModelOverride],
) -> Vec<Result<bool, ValidationError>> {
    let fields = |list: &[ModelOverride]| -> BTreeMap<String, Vec<OverrideField>> {
        list.iter().map(|o| (o.selector.clone(), o.fields.clone())).collect()
    };
    let (old, new) = (fields(before), fields(after));
    let mut results = Vec::new();
    for selector in old.keys().chain(new.keys().filter(|s| !old.contains_key(*s))) {
        let (old_fields, new_fields) = (old.get(selector), new.get(selector));
        if old_fields == new_fields {
            continue;
        }
        let location = format!("Model overrides › {selector}");
        let Some((provider, model)) = ModelRef::split(selector) else {
            results.push(Err(error(&location, "write the model as provider/model")));
            continue;
        };
        for field in old_fields.into_iter().flatten() {
            if new_fields.is_none_or(|fields| !fields.iter().any(|f| f.key == field.key))
                && let Some((_, key)) = OVERRIDE_FIELDS.iter().find(|(name, _)| *name == field.key)
            {
                results.push(models.clear_override(provider, model, *key).map_err(|e| error(&location, e)));
            }
        }
        for field in new_fields.into_iter().flatten() {
            let unchanged = old_fields.is_some_and(|fields| fields.contains(field));
            if unchanged {
                continue;
            }
            results.push(
                override_field(&field.key, &field.value)
                    .map_err(|message| error(&location, message))
                    .and_then(|f| models.set_override(provider, model, &f).map_err(|e| error(&location, e)))
                    .map(|()| true),
            );
        }
    }
    results
}

fn apply_custom_providers(
    models: &mut ModelsYml,
    before: &[CustomProvider],
    after: &[CustomProvider],
) -> Vec<Result<bool, ValidationError>> {
    let mut results = Vec::new();
    for old in before.iter().filter(|old| !after.iter().any(|new| new.id == old.id)) {
        let location = format!("Custom providers › {}", old.id);
        results.push(models.remove_provider(&old.id).map_err(|e| error(&location, e)));
    }
    for new in after {
        let old = before.iter().find(|old| old.id == new.id);
        if old == Some(new) {
            continue;
        }
        let location = format!("Custom providers › {}", new.id);
        results.push(write_custom_provider(models, old, new).map_err(|message| error(&location, message)).map(|()| true));
    }
    results
}

fn write_custom_provider(models: &mut ModelsYml, old: Option<&CustomProvider>, new: &CustomProvider) -> Result<(), String> {
    let existing = old.and_then(|_| models.provider(&new.id));
    let mut provider = match &existing {
        Some(entry) => models_yml::CustomProvider {
            id: entry.id.clone(),
            base_url: entry.base_url.clone(),
            api: entry.api.clone(),
            api_key: ApiKeyWrite::Keep,
            auth: entry.auth,
            auth_header: entry.auth_header,
            discovery: entry.discovery.clone(),
            models: entry.models.clone(),
        },
        None => models_yml::CustomProvider { id: new.id.clone(), ..Default::default() },
    };
    let changed = |f: fn(&CustomProvider) -> &str| old.is_none_or(|old| f(old) != f(new));
    let optional = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    if changed(|p| &p.base_url) {
        provider.base_url = optional(&new.base_url);
    }
    if changed(|p| &p.api) {
        provider.api = optional(&new.api);
    }
    if changed(|p| &p.api_key_env) {
        provider.api_key = match optional(&new.api_key_env) {
            Some(name) => ApiKeyWrite::Set(KeyReference::env_var(&name).map_err(|e| e.to_string())?),
            None => ApiKeyWrite::Remove,
        };
    }
    let auth_changed = old.is_none_or(|old| old.auth != new.auth);
    if auth_changed {
        // A new provider on the default (API key) auth leaves `auth` out, as omp does.
        provider.auth = (old.is_some() || new.auth != ProviderAuth::ApiKey).then(|| file_auth(new.auth));
    }
    if old.is_none_or(|old| old.models != new.models) {
        let kept = existing.map(|e| e.models).unwrap_or_default();
        provider.models = new
            .models
            .iter()
            .map(|id| {
                kept.iter().find(|m| &m.id == id).cloned().unwrap_or_else(|| models_yml::CustomModel {
                    id: id.clone(),
                    ..Default::default()
                })
            })
            .collect();
    }
    models.set_custom_provider(&provider).map_err(|e| e.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const CONFIG: &str = "\
# my omp settings
modelRoles:
  default: anthropic/claude-x:high
  smol: \"@default\"
retry:
  fallbackChains:
    default:
      - openai/gpt-y
modelProviderOrder:
  - openai
enabledModels:
  - anthropic/*
  - path: ~/work
    models:
      - openai/gpt-y
disabledProviders:
  - ollama
  - paths:
      - ~/secret
    providers:
      - openai
theme:
  dark: titanium
";

    const MODELS: &str = "\
providers:
  gw:
    baseUrl: https://gw.example.com/v1
    api: openai-completions
    apiKey: \"!op read op://dev/gw\"
    headers:
      X-Team: core
    models:
      - id: m1
        name: Model One
        contextWindow: 64000
  anthropic:
    modelOverrides:
      claude-x:
        name: Claude X (team)
        contextWindow: 200000
        cost:
          input: 1
          output: 2
";

    pub(crate) fn model(provider: &str, id: &str) -> rpc::ModelInfo {
        rpc::ModelInfo {
            id: id.into(),
            name: id.to_uppercase(),
            provider: provider.into(),
            api: String::new(),
            reasoning: false,
            input: Vec::new(),
            cost: rpc::ModelCost::default(),
            context_window: None,
            max_tokens: None,
            thinking: None,
            extra: Default::default(),
        }
    }

    fn available() -> Vec<rpc::ModelInfo> {
        vec![model("anthropic", "claude-x"), model("anthropic", "claude-z"), model("openai", "gpt-y")]
    }

    #[derive(Clone)]
    struct Files {
        _dir: std::rc::Rc<tempfile::TempDir>,
        config: OmpConfig,
        models: ModelsYml,
    }

    fn files(config: &str, models: &str) -> Files {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.yml"), config).unwrap();
        std::fs::write(dir.path().join("models.yml"), models).unwrap();
        Files {
            config: OmpConfig::load(&dir.path().join("config.yml")).unwrap(),
            models: ModelsYml::load(&dir.path().join("models.yml")).unwrap(),
            _dir: std::rc::Rc::new(dir),
        }
    }

    fn names() -> HashMap<String, String> {
        [("anthropic".to_owned(), "Anthropic".to_owned())].into()
    }

    fn read(f: &Files) -> ModelSettings {
        settings(&f.config, &f.models, &available(), &names()).unwrap()
    }

    fn save(f: &mut Files, before: &ModelSettings, after: &ModelSettings) -> Result<Edited, Vec<ValidationError>> {
        apply_settings(&mut f.config, &mut f.models, before, after)
    }

    #[test]
    fn reads_roles_toggles_providers_overrides_and_custom_providers() {
        let f = files(CONFIG, MODELS);
        let s = read(&f);
        assert_eq!(
            s.roles,
            [
                RoleChain { role: "default".into(), chain: vec!["anthropic/claude-x:high".into(), "openai/gpt-y".into()] },
                RoleChain { role: "smol".into(), chain: vec!["@default".into()] },
            ]
        );
        let toggles: Vec<(&str, bool)> = s.models.iter().map(|t| (t.selector.as_str(), t.enabled)).collect();
        assert_eq!(toggles, [("anthropic/claude-x", true), ("anthropic/claude-z", true), ("openai/gpt-y", false)]);
        let providers: Vec<(&str, &str, bool)> =
            s.providers.iter().map(|p| (p.id.as_str(), p.name.as_str(), p.enabled)).collect();
        assert_eq!(providers, [("openai", "", true), ("anthropic", "Anthropic", true), ("ollama", "", false)]);
        assert_eq!(
            s.overrides,
            [ModelOverride {
                selector: "anthropic/claude-x".into(),
                fields: vec![
                    OverrideField { key: "name".into(), value: FieldValue::Text("Claude X (team)".into()) },
                    OverrideField { key: "contextWindow".into(), value: FieldValue::Int(200_000) },
                ],
            }]
        );
        assert_eq!(
            s.custom_providers,
            [CustomProvider {
                id: "gw".into(),
                base_url: "https://gw.example.com/v1".into(),
                api: "openai-completions".into(),
                auth: ProviderAuth::ApiKey,
                api_key_env: String::new(),
                models: vec!["m1".into()],
            }]
        );
    }

    #[test]
    fn saving_an_unchanged_screen_edits_nothing() {
        let mut f = files(CONFIG, MODELS);
        let s = read(&f);
        assert_eq!(save(&mut f, &s, &s).unwrap(), Edited::default());
        assert_eq!(f.config.text(), CONFIG);
        assert_eq!(f.models.text(), MODELS);
    }

    #[test]
    fn role_edits_touch_only_their_keys() {
        let mut f = files(CONFIG, MODELS);
        let before = read(&f);
        let mut after = before.clone();
        after.roles[0].chain.push("anthropic/claude-z".into());
        after.roles.remove(1);
        after.roles.push(RoleChain { role: "plan".into(), chain: vec!["openai/gpt-y:low".into()] });
        assert_eq!(save(&mut f, &before, &after).unwrap(), Edited { config: true, models: false });
        let text = f.config.text();
        assert!(text.starts_with("# my omp settings\nmodelRoles:\n  default: anthropic/claude-x:high\n"), "{text}");
        assert!(!text.contains("smol"), "{text}");
        assert!(text.contains("plan: openai/gpt-y:low"), "{text}");
        assert!(text.contains("      - openai/gpt-y\n      - anthropic/claude-z\n"), "{text}");
        assert!(text.ends_with("theme:\n  dark: titanium\n"), "{text}");
        assert_eq!(read(&f).roles, after.roles);

        let bad = {
            let mut bad = after.clone();
            bad.roles[0].chain[0] = "has space".into();
            bad
        };
        let errors = save(&mut f, &after, &bad).unwrap_err();
        assert_eq!(errors[0].location, "Model roles › default");
    }

    #[test]
    fn enabled_models_keep_patterns_that_still_hold_and_path_entries() {
        let mut f = files(CONFIG, MODELS);
        let before = read(&f);
        let mut after = before.clone();
        after.models[2].enabled = true;
        save(&mut f, &before, &after).unwrap();
        let entries = f.config.scoped_list(ScopedList::EnabledModels);
        assert_eq!(entries.len(), 1, "every model enabled: only the path entry is left: {entries:?}");
        assert!(matches!(&entries[0], ScopedEntry::Paths { .. }));

        let before = read(&f);
        assert!(before.models.iter().all(|t| t.enabled));
        let mut after = before.clone();
        after.models[1].enabled = false;
        save(&mut f, &before, &after).unwrap();
        let entries = f.config.scoped_list(ScopedList::EnabledModels);
        assert_eq!(
            entries[..2],
            [ScopedEntry::Everywhere("anthropic/claude-x".into()), ScopedEntry::Everywhere("openai/gpt-y".into())]
        );
        assert!(matches!(&entries[2], ScopedEntry::Paths { .. }));

        let mut none = after.clone();
        none.models.iter_mut().for_each(|t| t.enabled = false);
        assert_eq!(save(&mut f, &after, &none).unwrap_err()[0].location, "Enabled models");
    }

    #[test]
    fn glob_patterns_stay_while_their_models_stay_enabled() {
        let mut f = files(CONFIG, MODELS);
        let mut models = available();
        models.push(model("openai", "gpt-w"));
        let before = settings(&f.config, &f.models, &models, &names()).unwrap();
        let mut after = before.clone();
        let gpt_y = after.models.iter().position(|t| t.selector == "openai/gpt-y").unwrap();
        after.models[gpt_y].enabled = true;
        save(&mut f, &before, &after).unwrap();
        let entries = f.config.scoped_list(ScopedList::EnabledModels);
        assert_eq!(
            entries[..2],
            [ScopedEntry::Everywhere("anthropic/*".into()), ScopedEntry::Everywhere("openai/gpt-y".into())]
        );

        let before = settings(&f.config, &f.models, &models, &names()).unwrap();
        let mut after = before.clone();
        after.models[1].enabled = false;
        save(&mut f, &before, &after).unwrap();
        let entries = f.config.scoped_list(ScopedList::EnabledModels);
        assert_eq!(
            entries[..2],
            [ScopedEntry::Everywhere("anthropic/claude-x".into()), ScopedEntry::Everywhere("openai/gpt-y".into())],
            "anthropic/* no longer holds, so its enabled model is listed itself"
        );
    }

    #[test]
    fn provider_order_and_disabling_keep_path_entries() {
        let mut f = files(CONFIG, MODELS);
        let before = read(&f);
        let mut after = before.clone();
        after.providers.swap(1, 0);
        after.providers[2].enabled = true;
        after.providers[1].enabled = false;
        save(&mut f, &before, &after).unwrap();
        assert_eq!(f.config.provider_order(), ["anthropic", "openai", "ollama"]);
        let disabled = f.config.scoped_list(ScopedList::DisabledProviders);
        assert!(matches!(&disabled[0], ScopedEntry::Paths { .. }), "{disabled:?}");
        assert_eq!(disabled[1], ScopedEntry::Everywhere("openai".into()));
        assert_eq!(disabled.len(), 2);
    }

    #[test]
    fn override_edits_change_only_shown_fields() {
        let mut f = files(CONFIG, MODELS);
        let before = read(&f);
        let mut after = before.clone();
        after.overrides[0].fields = vec![
            OverrideField { key: "contextWindow".into(), value: FieldValue::Int(300_000) },
            OverrideField { key: "maxTokens".into(), value: FieldValue::Int(8_192) },
        ];
        after.overrides.push(ModelOverride {
            selector: "openai/gpt-y".into(),
            fields: vec![OverrideField { key: "reasoning".into(), value: FieldValue::Bool(true) }],
        });
        assert_eq!(save(&mut f, &before, &after).unwrap(), Edited { config: false, models: true });
        let text = f.models.text();
        assert!(!text.contains("Claude X (team)"), "{text}");
        assert!(text.contains("contextWindow: 300000"), "{text}");
        assert!(text.contains("maxTokens: 8192"), "{text}");
        assert!(text.contains("cost:"), "unshown fields stay: {text}");
        assert!(text.contains("  openai:\n    modelOverrides:\n      gpt-y:\n        reasoning: true\n"), "{text}");
        assert_eq!(read(&f).overrides, after.overrides);

        let mut bad = after.clone();
        bad.overrides[0].fields.push(OverrideField { key: "temperature".into(), value: FieldValue::Float(0.2) });
        let errors = save(&mut f, &after, &bad).unwrap_err();
        assert_eq!(errors[0].location, "Model overrides › anthropic/claude-x");
        assert!(errors[0].message.contains("temperature"), "{errors:?}");
    }

    #[test]
    fn custom_provider_edits_keep_secret_commands_headers_and_model_details() {
        let mut f = files(CONFIG, MODELS);
        let before = read(&f);
        let mut after = before.clone();
        after.custom_providers[0].base_url = "https://gw2.example.com/v1".into();
        after.custom_providers[0].models.push("m2".into());
        after.custom_providers.push(CustomProvider {
            id: "local".into(),
            base_url: "http://127.0.0.1:8080/v1".into(),
            api: "openai-completions".into(),
            auth: ProviderAuth::ApiKey,
            api_key_env: "LOCAL_KEY".into(),
            models: vec!["qwen".into()],
        });
        save(&mut f, &before, &after).unwrap();
        let text = f.models.text();
        assert!(text.contains("baseUrl: https://gw2.example.com/v1"), "{text}");
        assert!(text.contains("apiKey: \"!op read op://dev/gw\""), "{text}");
        assert!(text.contains("X-Team: core"), "{text}");
        assert!(text.contains("name: Model One"), "{text}");
        assert!(text.contains("- id: m2"), "{text}");
        assert!(text.contains("  local:\n    baseUrl: http://127.0.0.1:8080/v1\n"), "{text}");
        assert!(text.contains("apiKey: LOCAL_KEY"), "{text}");
        assert!(!text.contains("auth:"), "{text}");

        let before = read(&f);
        let mut after = before.clone();
        after.custom_providers[0].api_key_env = "sk-pasted-secret".into();
        let errors = save(&mut f.clone(), &before, &after).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].location, "Custom providers › gw");

        let mut after = before.clone();
        after.custom_providers.retain(|p| p.id != "gw");
        after.custom_providers[0].api_key_env = String::new();
        after.custom_providers[0].auth = ProviderAuth::None;
        save(&mut f, &before, &after).unwrap();
        let text = f.models.text();
        assert!(!text.contains("gw"), "{text}");
        assert!(!text.contains("apiKey"), "clearing the variable removes the key: {text}");
        assert!(text.contains("    auth: none\n"), "{text}");
        assert!(text.contains("  anthropic:\n    modelOverrides:"), "{text}");
    }

    #[test]
    fn globs_match_selectors_and_bare_ids() {
        assert!(pattern_matches("anthropic/*", "anthropic", "claude-x"));
        assert!(pattern_matches("claude-?", "anthropic", "claude-x"));
        assert!(pattern_matches("claude-x", "anthropic", "claude-x"));
        assert!(!pattern_matches("openai/*", "anthropic", "claude-x"));
        assert!(!pattern_matches("anthropic/claude", "anthropic", "claude-x"));
    }
}
