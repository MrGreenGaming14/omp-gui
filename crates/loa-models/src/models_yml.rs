//! Typed editor for omp's `models.yml`: custom providers and per model overrides.
//!
//! Shape, as omp's `models.md` defines it:
//!
//! ```yaml
//! providers:
//!   <provider-id>:
//!     baseUrl: https://api.example.com/v1
//!     api: openai-completions
//!     apiKey: MY_PROVIDER_API_KEY   # env var name, or "!command"
//!     auth: apiKey | none | oauth
//!     discovery: { type: ollama }
//!     modelOverrides:
//!       <model-id>: { name: ..., contextWindow: ... }
//!     models:
//!       - id: ...
//! ```
//!
//! Provider keys this editor does not model (`headers`, `compat`, ...) are left
//! untouched. Every edit is checked against omp's validation rules for the
//! provider it touched, and changes only that provider's lines.
//!
//! API keys: `apiKey` can only be written as a [`KeyReference`] (an environment
//! variable name or a `!command`). An inline key found in the file is reported as
//! [`ApiKeySetting::Literal`], which carries no value.

use std::path::Path;

use crate::fsutil::Snapshot;
use crate::rpc::ModelCost;
use crate::validate::SettingsValidator;
use crate::yaml::{Yaml, YamlDocument, get_path, ymap, ystr, ystrs};
use crate::{Error, Result};

/// The `api` values omp accepts for providers and models.
pub const APIS: [&str; 14] = [
    "openai-completions",
    "openai-responses",
    "openai-codex-responses",
    "azure-openai-responses",
    "anthropic-messages",
    "bedrock-converse-stream",
    "google-generative-ai",
    "google-gemini-cli",
    "google-vertex",
    "typesafe",
    "openrouter-decisions",
    "openai-decisions",
    "google-decisions",
    "workers-ai-systemone",
];

/// The `discovery.type` values omp accepts.
pub const DISCOVERY_TYPES: [&str; 7] = [
    "ollama",
    "llama.cpp",
    "lm-studio",
    "openai-models-list",
    "proxy",
    "litellm",
    "apple-foundation-models",
];

/// How a provider authenticates (`auth`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    ApiKey,
    None,
    OAuth,
}

impl AuthMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "apiKey",
            Self::None => "none",
            Self::OAuth => "oauth",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "apiKey" => Some(Self::ApiKey),
            "none" => Some(Self::None),
            "oauth" => Some(Self::OAuth),
            _ => None,
        }
    }
}

/// Where omp reads a provider's key from. There is deliberately no variant that
/// holds a key value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyReference {
    /// An environment variable name, written as is.
    EnvVar(String),
    /// A shell command whose trimmed stdout is the key, written `!command`.
    Command(String),
}

impl KeyReference {
    /// A reference to environment variable `name` (upper case letters, digits
    /// and `_`, not starting with a digit). The upper case rule keeps a pasted
    /// key from being stored as if it were a variable name.
    pub fn env_var(name: &str) -> Result<Self> {
        if !is_env_var_name(name) {
            return Err(Error::invalid(
                "apiKey",
                "an environment variable name is upper case letters, digits and `_`",
            ));
        }
        Ok(Self::EnvVar(name.to_owned()))
    }

    /// A reference to a command that prints the key, such as
    /// `op read op://dev/openai/api-key`.
    pub fn command(command: &str) -> Result<Self> {
        let command = command.trim();
        let command = command.strip_prefix('!').unwrap_or(command).trim();
        if command.is_empty() || command.contains(['\n', '\r']) {
            return Err(Error::invalid("apiKey", "a key command is one non-empty line"));
        }
        Ok(Self::Command(command.to_owned()))
    }

    fn yaml(&self) -> Yaml {
        match self {
            Self::EnvVar(name) => ystr(name.clone()),
            Self::Command(cmd) => ystr(format!("!{cmd}")),
        }
    }
}

/// Whether `name` looks like an environment variable name by convention.
pub fn is_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// What a provider's `apiKey` holds, as reported to the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeySetting {
    /// Names an environment variable.
    EnvVar(String),
    /// Runs a secret command; command text is never exposed.
    Command,
    /// An inline value; its content is never reported.
    Literal,
}

impl ApiKeySetting {
    fn from_yaml(value: &str) -> Self {
        if value.starts_with('!') {
            Self::Command
        } else if is_env_var_name(value) {
            Self::EnvVar(value.to_owned())
        } else {
            Self::Literal
        }
    }
}

/// A model defined by a custom provider (`providers.<id>.models[]`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CustomModel {
    pub id: String,
    pub name: Option<String>,
    pub api: Option<String>,
    pub reasoning: Option<bool>,
    pub input: Option<Vec<Modality>>,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
    pub cost: Option<ModelCost>,
}

/// An input modality a model accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modality {
    Text,
    Image,
}

impl Modality {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "text" => Some(Self::Text),
            "image" => Some(Self::Image),
            _ => None,
        }
    }
}

/// The fields of a custom provider this editor writes. `None` removes the key.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CustomProvider {
    pub id: String,
    pub base_url: Option<String>,
    pub api: Option<String>,
    pub api_key: Option<KeyReference>,
    pub auth: Option<AuthMode>,
    /// Send the resolved key as `Authorization: Bearer`.
    pub auth_header: Option<bool>,
    pub discovery: Option<String>,
    pub models: Vec<CustomModel>,
}

/// A provider entry as read from the file.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderEntry {
    pub id: String,
    pub base_url: Option<String>,
    pub api: Option<String>,
    pub api_key: Option<ApiKeySetting>,
    pub auth: Option<AuthMode>,
    pub auth_header: Option<bool>,
    pub discovery: Option<String>,
    pub models: Vec<CustomModel>,
    pub overrides: Vec<(String, ModelOverride)>,
}

/// A per model override as read from the file. Keys the editor does not model
/// are listed in `other` so the screen can say they exist.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelOverride {
    pub name: Option<String>,
    pub context_window: Option<u64>,
    pub max_context_window: Option<u64>,
    pub max_tokens: Option<u64>,
    pub reasoning: Option<bool>,
    pub input: Option<Vec<Modality>>,
    pub context_promotion_target: Option<String>,
    pub compaction_model: Option<String>,
    pub prefer_websockets: Option<bool>,
    pub cost: Option<ModelCost>,
    pub other: Vec<String>,
}

/// One override field with its value, for writing.
#[derive(Debug, Clone, PartialEq)]
pub enum OverrideField {
    Name(String),
    ContextWindow(u64),
    MaxContextWindow(u64),
    MaxTokens(u64),
    Reasoning(bool),
    Input(Vec<Modality>),
    ContextPromotionTarget(String),
    CompactionModel(String),
    PreferWebsockets(bool),
    Cost(ModelCost),
}

/// Names an override field, for clearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideKey {
    Name,
    ContextWindow,
    MaxContextWindow,
    MaxTokens,
    Reasoning,
    Input,
    ContextPromotionTarget,
    CompactionModel,
    PreferWebsockets,
    Cost,
}

impl OverrideKey {
    fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::ContextWindow => "contextWindow",
            Self::MaxContextWindow => "maxContextWindow",
            Self::MaxTokens => "maxTokens",
            Self::Reasoning => "reasoning",
            Self::Input => "input",
            Self::ContextPromotionTarget => "contextPromotionTarget",
            Self::CompactionModel => "compactionModel",
            Self::PreferWebsockets => "preferWebsockets",
            Self::Cost => "cost",
        }
    }
}

impl OverrideField {
    /// The field's key.
    pub fn key(&self) -> OverrideKey {
        match self {
            Self::Name(_) => OverrideKey::Name,
            Self::ContextWindow(_) => OverrideKey::ContextWindow,
            Self::MaxContextWindow(_) => OverrideKey::MaxContextWindow,
            Self::MaxTokens(_) => OverrideKey::MaxTokens,
            Self::Reasoning(_) => OverrideKey::Reasoning,
            Self::Input(_) => OverrideKey::Input,
            Self::ContextPromotionTarget(_) => OverrideKey::ContextPromotionTarget,
            Self::CompactionModel(_) => OverrideKey::CompactionModel,
            Self::PreferWebsockets(_) => OverrideKey::PreferWebsockets,
            Self::Cost(_) => OverrideKey::Cost,
        }
    }

    fn yaml(&self) -> Result<Yaml> {
        Ok(match self {
            Self::Name(s) | Self::ContextPromotionTarget(s) | Self::CompactionModel(s) => {
                non_empty(self.key().as_str(), s)?;
                ystr(s.clone())
            }
            Self::ContextWindow(n) | Self::MaxContextWindow(n) | Self::MaxTokens(n) => {
                positive_int(self.key().as_str(), *n)?
            }
            Self::Reasoning(b) | Self::PreferWebsockets(b) => Yaml::Boolean(*b),
            Self::Input(m) => modalities_yaml(m)?,
            Self::Cost(c) => cost_yaml(c)?,
        })
    }
}

fn non_empty(field: &str, s: &str) -> Result<()> {
    if s.trim().is_empty() || s.chars().any(char::is_control) {
        return Err(Error::invalid(field, "must be a non-empty single line"));
    }
    Ok(())
}

fn positive_int(field: &str, n: u64) -> Result<Yaml> {
    match i64::try_from(n) {
        Ok(v) if v > 0 && v <= (1_i64 << 53) => Ok(Yaml::Integer(v)),
        _ => Err(Error::invalid(field, "must be a positive integer")),
    }
}

fn modalities_yaml(m: &[Modality]) -> Result<Yaml> {
    if !m.contains(&Modality::Text) {
        return Err(Error::invalid("input", "must include text"));
    }
    let mut seen = Vec::new();
    for x in m {
        if !seen.contains(x) {
            seen.push(*x);
        }
    }
    Ok(ystrs(seen.iter().map(|x| x.as_str())))
}

fn number_yaml(field: &str, v: f64) -> Result<Yaml> {
    if !v.is_finite() || v < 0.0 {
        return Err(Error::invalid(field, "must be a non-negative number"));
    }
    if v.fract() == 0.0 && v < 9.0e15 {
        // Whole numbers are written as YAML integers; the conversion is exact
        // below 2^53.
        #[allow(clippy::cast_possible_truncation)]
        return Ok(Yaml::Integer(v as i64));
    }
    Ok(Yaml::Real(format!("{v}")))
}

fn cost_yaml(c: &ModelCost) -> Result<Yaml> {
    Ok(ymap([
        ("input", number_yaml("cost.input", c.input)?),
        ("output", number_yaml("cost.output", c.output)?),
        ("cacheRead", number_yaml("cost.cacheRead", c.cache_read)?),
        ("cacheWrite", number_yaml("cost.cacheWrite", c.cache_write)?),
    ]))
}

fn read_cost(y: &Yaml) -> Option<ModelCost> {
    let h = y.as_hash()?;
    let n = |k: &str| -> f64 {
        match h.get(&ystr(k)) {
            Some(Yaml::Integer(i)) => *i as f64,
            Some(v) => v.as_f64().unwrap_or(0.0),
            None => 0.0,
        }
    };
    Some(ModelCost {
        input: n("input"),
        output: n("output"),
        cache_read: n("cacheRead"),
        cache_write: n("cacheWrite"),
    })
}

fn read_u64(y: Option<&Yaml>) -> Option<u64> {
    y.and_then(Yaml::as_i64).and_then(|v| u64::try_from(v).ok())
}

fn read_str(y: Option<&Yaml>) -> Option<String> {
    y.and_then(Yaml::as_str).map(str::to_owned)
}

fn read_modalities(y: Option<&Yaml>) -> Option<Vec<Modality>> {
    y.and_then(Yaml::as_vec)
        .map(|v| v.iter().filter_map(Yaml::as_str).filter_map(Modality::parse).collect())
}

fn read_model(y: &Yaml) -> Option<CustomModel> {
    let h = y.as_hash()?;
    let get = |k: &str| h.get(&ystr(k));
    Some(CustomModel {
        id: read_str(get("id"))?,
        name: read_str(get("name")),
        api: read_str(get("api")),
        reasoning: get("reasoning").and_then(Yaml::as_bool),
        input: read_modalities(get("input")),
        context_window: read_u64(get("contextWindow")),
        max_tokens: read_u64(get("maxTokens")),
        cost: get("cost").and_then(read_cost),
    })
}

fn model_yaml(m: &CustomModel) -> Result<Yaml> {
    non_empty("models[].id", &m.id)?;
    let mut pairs = vec![("id", ystr(m.id.clone()))];
    if let Some(name) = &m.name {
        non_empty("models[].name", name)?;
        pairs.push(("name", ystr(name.clone())));
    }
    if let Some(api) = &m.api {
        check_api(api)?;
        pairs.push(("api", ystr(api.clone())));
    }
    if let Some(r) = m.reasoning {
        pairs.push(("reasoning", Yaml::Boolean(r)));
    }
    if let Some(input) = &m.input {
        pairs.push(("input", modalities_yaml(input)?));
    }
    if let Some(n) = m.context_window {
        pairs.push(("contextWindow", positive_int("contextWindow", n)?));
    }
    if let Some(n) = m.max_tokens {
        pairs.push(("maxTokens", positive_int("maxTokens", n)?));
    }
    if let Some(c) = &m.cost {
        pairs.push(("cost", cost_yaml(c)?));
    }
    Ok(ymap(pairs))
}

fn check_api(api: &str) -> Result<()> {
    if APIS.contains(&api) {
        Ok(())
    } else {
        Err(Error::invalid("api", format!("`{api}` is not an api omp knows")))
    }
}

fn check_provider_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(Error::invalid(
            "provider id",
            format!("`{id}` must be letters, digits, `-`, `_` or `.`"),
        ));
    }
    Ok(())
}

/// Checks a provider entry against omp's `models.yml` validation rules.
fn validate_provider_node(id: &str, node: &Yaml) -> Result<()> {
    let field = |f: &str| format!("providers.{id}.{f}");
    let Some(h) = node.as_hash() else {
        return Err(Error::invalid(field(""), "must be a mapping"));
    };
    let get = |k: &str| h.get(&ystr(k));
    let api = get("api").and_then(Yaml::as_str);
    if let Some(api) = api {
        check_api(api)?;
    }
    let auth = get("auth").and_then(Yaml::as_str).and_then(AuthMode::parse);
    let keyless = matches!(auth, Some(AuthMode::None | AuthMode::OAuth));
    let models = get("models").and_then(Yaml::as_vec).cloned().unwrap_or_default();
    if !models.is_empty() {
        if get("baseUrl").is_none() {
            return Err(Error::invalid(field("baseUrl"), "is required when defining custom models"));
        }
        if get("apiKey").is_none() && !keyless {
            return Err(Error::invalid(
                field("apiKey"),
                "is required unless auth is none or oauth",
            ));
        }
        let mut ids = Vec::new();
        for (i, m) in models.iter().enumerate() {
            let Some(model) = read_model(m) else {
                return Err(Error::invalid(field(&format!("models[{i}].id")), "is required"));
            };
            if api.is_none() && model.api.is_none() {
                return Err(Error::invalid(
                    field(&format!("models[{i}].api")),
                    "is required when the provider has no api",
                ));
            }
            if ids.contains(&model.id) {
                return Err(Error::invalid(field("models"), format!("`{}` is listed twice", model.id)));
            }
            ids.push(model.id);
        }
    } else {
        const SUFFICIENT: [&str; 10] = [
            "baseUrl",
            "apiKey",
            "headers",
            "compat",
            "guardrailIdentifier",
            "requestMetadata",
            "modelOverrides",
            "discovery",
            "remoteCompaction",
            "transport",
        ];
        let has_override = SUFFICIENT.iter().any(|k| match get(k) {
            Some(Yaml::Hash(m)) => !m.is_empty(),
            Some(_) => true,
            None => false,
        }) || auth == Some(AuthMode::None)
            || get("disableStrictTools").and_then(Yaml::as_bool) == Some(true);
        if !has_override {
            return Err(Error::invalid(
                field(""),
                "a provider without models must set baseUrl, apiKey, auth: none, headers, compat, discovery or modelOverrides",
            ));
        }
    }
    if let Some(d) = get("discovery") {
        let kind = d.as_hash().and_then(|m| m.get(&ystr("type"))).and_then(Yaml::as_str);
        match kind {
            Some(k) if DISCOVERY_TYPES.contains(&k) => {
                if k != "proxy" && api.is_none() {
                    return Err(Error::invalid(field("api"), "is required with discovery"));
                }
            }
            _ => return Err(Error::invalid(field("discovery.type"), "is not a discovery type omp knows")),
        }
    }
    if let Some(overrides) = get("modelOverrides").and_then(Yaml::as_hash) {
        for (model, o) in overrides {
            let Some(o) = o.as_hash() else { continue };
            let cw = read_u64(o.get(&ystr("contextWindow")));
            let mcw = read_u64(o.get(&ystr("maxContextWindow")));
            if let (Some(cw), Some(mcw)) = (cw, mcw)
                && mcw < cw
            {
                return Err(Error::invalid(
                    field(&format!(
                        "modelOverrides.{}.maxContextWindow",
                        model.as_str().unwrap_or_default()
                    )),
                    "must not be smaller than contextWindow",
                ));
            }
        }
    }
    Ok(())
}

/// omp's `models.yml`, loaded for editing.
#[derive(Debug, Clone)]
pub struct ModelsYml {
    doc: YamlDocument,
    snapshot: Snapshot,
}

impl ModelsYml {
    /// Loads `path`; a missing file is an empty document.
    pub fn load(path: &Path) -> Result<Self> {
        let (snapshot, text) = Snapshot::load(path)?;
        let doc = YamlDocument::parse(text.as_deref().unwrap_or(""), &path.display().to_string())?;
        Ok(Self { doc, snapshot })
    }

    /// The edited document text.
    pub fn text(&self) -> &str {
        self.doc.text()
    }

    /// Writes the edits atomically.
    pub fn save(&mut self) -> Result<()> {
        self.snapshot.save(self.doc.text())
    }

    /// Asks `validator` (omp itself) to load the edited file, then saves.
    pub async fn save_validated(&mut self, validator: &dyn SettingsValidator) -> Result<()> {
        validator.check_models(self.doc.text()).await?;
        self.save()
    }

    /// Every provider entry, in file order.
    pub fn providers(&self) -> Vec<ProviderEntry> {
        let Some(map) = self.doc.get(&["providers"]).and_then(Yaml::as_hash) else {
            return Vec::new();
        };
        map.iter()
            .filter_map(|(k, v)| Some(read_provider(k.as_str()?, v)))
            .collect()
    }

    /// One provider entry.
    pub fn provider(&self, id: &str) -> Option<ProviderEntry> {
        self.doc
            .get(&["providers", id])
            .map(|v| read_provider(id, v))
    }

    /// Creates or updates a custom provider. Only the keys of [`CustomProvider`]
    /// change; `headers`, `compat`, `modelOverrides` and other keys stay. The
    /// result must pass omp's rules or nothing changes.
    pub fn set_custom_provider(&mut self, p: &CustomProvider) -> Result<()> {
        check_provider_id(&p.id)?;
        if let Some(url) = &p.base_url {
            non_empty("baseUrl", url)?;
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(Error::invalid("baseUrl", "must be an http or https URL"));
            }
        }
        if let Some(api) = &p.api {
            check_api(api)?;
        }
        if let Some(d) = &p.discovery
            && !DISCOVERY_TYPES.contains(&d.as_str())
        {
            return Err(Error::invalid("discovery", format!("`{d}` is not a discovery type omp knows")));
        }
        let models = p
            .models
            .iter()
            .map(model_yaml)
            .collect::<Result<Vec<_>>>()?;

        let mut next = self.doc.clone();
        let id = p.id.as_str();
        let mut put = |key: &str, value: Option<Yaml>| -> Result<()> {
            match value {
                Some(v) => next.set(&["providers", id, key], v),
                None => next.remove(&["providers", id, key]).map(|_| ()),
            }
        };
        put("baseUrl", p.base_url.clone().map(ystr))?;
        put("api", p.api.clone().map(ystr))?;
        put("apiKey", p.api_key.as_ref().map(KeyReference::yaml))?;
        put("auth", p.auth.map(|a| ystr(a.as_str())))?;
        put("authHeader", p.auth_header.map(Yaml::Boolean))?;
        put(
            "discovery",
            p.discovery.clone().map(|d| ymap([("type", ystr(d))])),
        )?;
        put(
            "models",
            if models.is_empty() { None } else { Some(Yaml::Array(models)) },
        )?;
        let node = get_path(next.root(), &["providers", id])
            .ok_or_else(|| Error::invalid(format!("providers.{id}"), "nothing to write"))?;
        validate_provider_node(id, node)?;
        self.doc = next;
        Ok(())
    }

    /// Removes a provider entry entirely.
    pub fn remove_provider(&mut self, id: &str) -> Result<bool> {
        self.doc.remove(&["providers", id])
    }

    /// Sets one override field for `model` under `provider` (a built in or
    /// custom provider id).
    pub fn set_override(&mut self, provider: &str, model: &str, field: &OverrideField) -> Result<()> {
        check_provider_id(provider)?;
        non_empty("model id", model)?;
        let value = field.yaml()?;
        self.edit_provider(provider, |doc| {
            doc.set(
                &["providers", provider, "modelOverrides", model, field.key().as_str()],
                value,
            )
        })
    }

    /// Removes one override field. An override left empty is removed, and a
    /// provider left with nothing is removed too.
    pub fn clear_override(&mut self, provider: &str, model: &str, key: OverrideKey) -> Result<bool> {
        let mut removed = false;
        self.edit_provider(provider, |doc| {
            removed = doc.remove(&["providers", provider, "modelOverrides", model, key.as_str()])?;
            Ok(())
        })?;
        Ok(removed)
    }

    fn edit_provider(
        &mut self,
        provider: &str,
        edit: impl FnOnce(&mut YamlDocument) -> Result<()>,
    ) -> Result<()> {
        let mut next = self.doc.clone();
        edit(&mut next)?;
        if let Some(node) = get_path(next.root(), &["providers", provider]) {
            validate_provider_node(provider, node)?;
        }
        self.doc = next;
        Ok(())
    }
}

fn read_provider(id: &str, v: &Yaml) -> ProviderEntry {
    let h = v.as_hash();
    let get = |k: &str| h.and_then(|h| h.get(&ystr(k)));
    let overrides = get("modelOverrides")
        .and_then(Yaml::as_hash)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.as_str()?.to_owned(), read_override(v))))
                .collect()
        })
        .unwrap_or_default();
    ProviderEntry {
        id: id.to_owned(),
        base_url: read_str(get("baseUrl")),
        api: read_str(get("api")),
        api_key: get("apiKey").and_then(Yaml::as_str).map(ApiKeySetting::from_yaml),
        auth: get("auth").and_then(Yaml::as_str).and_then(AuthMode::parse),
        auth_header: get("authHeader").and_then(Yaml::as_bool),
        discovery: get("discovery")
            .and_then(Yaml::as_hash)
            .and_then(|d| read_str(d.get(&ystr("type")))),
        models: get("models")
            .and_then(Yaml::as_vec)
            .map(|v| v.iter().filter_map(read_model).collect())
            .unwrap_or_default(),
        overrides,
    }
}

fn read_override(v: &Yaml) -> ModelOverride {
    let Some(h) = v.as_hash() else {
        return ModelOverride::default();
    };
    let get = |k: &str| h.get(&ystr(k));
    const KNOWN: [&str; 10] = [
        "name",
        "contextWindow",
        "maxContextWindow",
        "maxTokens",
        "reasoning",
        "input",
        "contextPromotionTarget",
        "compactionModel",
        "preferWebsockets",
        "cost",
    ];
    ModelOverride {
        name: read_str(get("name")),
        context_window: read_u64(get("contextWindow")),
        max_context_window: read_u64(get("maxContextWindow")),
        max_tokens: read_u64(get("maxTokens")),
        reasoning: get("reasoning").and_then(Yaml::as_bool),
        input: read_modalities(get("input")),
        context_promotion_target: read_str(get("contextPromotionTarget")),
        compaction_model: read_str(get("compactionModel")),
        prefer_websockets: get("preferWebsockets").and_then(Yaml::as_bool),
        cost: get("cost").and_then(read_cost),
        other: h
            .keys()
            .filter_map(Yaml::as_str)
            .filter(|k| !KNOWN.contains(k))
            .map(str::to_owned)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODELS: &str = "\
# Custom providers for this machine
providers:
  # Corporate OpenRouter relay
  openrouter:
    baseUrl: https://my-proxy.example.com/v1
    headers:
      X-Team: platform
    modelOverrides:
      anthropic/claude-sonnet-4:
        name: Sonnet 4 (Corp)
        compat:
          openRouterRouting:
            only: [anthropic]

  local-openai:
    baseUrl: http://127.0.0.1:8000/v1
    auth: none
    api: openai-completions
    models:
      - id: Qwen/Qwen2.5-Coder-32B-Instruct
        name: Qwen 2.5 Coder 32B (local)

  pinned:
    baseUrl: https://gw.example.com/v1
    api: openai-completions
    apiKey: sk-live-0123456789abcdef
    models:
      - id: fast
";

    fn load(text: &str) -> (tempfile::TempDir, ModelsYml) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yml");
        std::fs::write(&path, text).unwrap();
        let m = ModelsYml::load(&path).unwrap();
        (dir, m)
    }

    #[test]
    fn reads_providers_overrides_and_never_reports_an_inline_key() {
        let (_d, m) = load(MODELS);
        let providers = m.providers();
        assert_eq!(providers.len(), 3);
        let or = &providers[0];
        assert_eq!(or.overrides[0].0, "anthropic/claude-sonnet-4");
        assert_eq!(or.overrides[0].1.name.as_deref(), Some("Sonnet 4 (Corp)"));
        assert_eq!(or.overrides[0].1.other, ["compat"]);
        assert_eq!(providers[1].auth, Some(AuthMode::None));
        assert_eq!(providers[1].models[0].id, "Qwen/Qwen2.5-Coder-32B-Instruct");
        assert_eq!(providers[2].api_key, Some(ApiKeySetting::Literal));
    }

    #[test]
    fn setting_an_override_keeps_compat_comments_and_other_providers() {
        let (_d, mut m) = load(MODELS);
        m.set_override(
            "openrouter",
            "anthropic/claude-sonnet-4",
            &OverrideField::ContextWindow(200_000),
        )
        .unwrap();
        let expected = MODELS.replace(
            "            only: [anthropic]\n",
            "            only: [anthropic]\n        contextWindow: 200000\n",
        );
        assert_eq!(m.text(), expected);
    }

    #[test]
    fn override_for_a_builtin_provider_creates_the_provider_entry() {
        let (_d, mut m) = load(MODELS);
        m.set_override(
            "openai-codex",
            "gpt-5.5",
            &OverrideField::ContextPromotionTarget("openai-codex/gpt-5.4".into()),
        )
        .unwrap();
        assert!(m.text().ends_with(
            "      - id: fast\n  openai-codex:\n    modelOverrides:\n      gpt-5.5:\n        contextPromotionTarget: openai-codex/gpt-5.4\n"
        ));
        assert!(m.text().starts_with("# Custom providers for this machine\n"));
    }

    #[test]
    fn clearing_the_only_override_removes_the_override_only_provider() {
        let (_d, mut m) = load("providers:\n  openai:\n    modelOverrides:\n      gpt-5:\n        preferWebsockets: false\n");
        assert!(m.clear_override("openai", "gpt-5", OverrideKey::PreferWebsockets).unwrap());
        assert_eq!(m.text(), "");
    }

    #[test]
    fn max_context_window_below_context_window_is_refused() {
        let (_d, mut m) = load(MODELS);
        m.set_override("openrouter", "x/y", &OverrideField::ContextWindow(200_000)).unwrap();
        let before = m.text().to_owned();
        let err = m
            .set_override("openrouter", "x/y", &OverrideField::MaxContextWindow(100_000))
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }));
        assert_eq!(m.text(), before);
    }

    #[test]
    fn custom_provider_with_models_needs_base_url_key_and_api() {
        let (_d, mut m) = load("");
        let mut p = CustomProvider {
            id: "my-gateway".into(),
            api: Some("openai-completions".into()),
            models: vec![CustomModel {
                id: "claude-sonnet".into(),
                context_window: Some(200_000),
                max_tokens: Some(8192),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(m.set_custom_provider(&p).is_err());
        p.base_url = Some("https://gateway.example.com/v1".into());
        assert!(m.set_custom_provider(&p).is_err(), "no key and auth is not none");
        p.api_key = Some(KeyReference::env_var("MY_GATEWAY_API_KEY").unwrap());
        m.set_custom_provider(&p).unwrap();
        assert_eq!(
            m.text(),
            "providers:\n  my-gateway:\n    baseUrl: https://gateway.example.com/v1\n    api: openai-completions\n    apiKey: MY_GATEWAY_API_KEY\n    models:\n      - id: claude-sonnet\n        contextWindow: 200000\n        maxTokens: 8192\n"
        );
        let read = m.provider("my-gateway").unwrap();
        assert_eq!(read.api_key, Some(ApiKeySetting::EnvVar("MY_GATEWAY_API_KEY".into())));
    }

    #[test]
    fn updating_a_provider_keeps_its_headers_and_overrides() {
        let (_d, mut m) = load(MODELS);
        let p = CustomProvider {
            id: "openrouter".into(),
            base_url: Some("https://relay2.example.com/v1".into()),
            api_key: Some(KeyReference::command("!op read op://dev/openrouter/key").unwrap()),
            ..Default::default()
        };
        m.set_custom_provider(&p).unwrap();
        assert!(m.text().contains("    baseUrl: https://relay2.example.com/v1\n    headers:\n      X-Team: platform\n"));
        assert!(m.text().contains("name: Sonnet 4 (Corp)"));
        assert!(m.text().contains("apiKey: \"!op read op://dev/openrouter/key\""));
        assert_eq!(
            m.provider("openrouter").unwrap().api_key,
            Some(ApiKeySetting::Command)
        );
    }

    #[test]
    fn discovery_without_api_is_refused_except_proxy() {
        let (_d, mut m) = load("");
        let mut p = CustomProvider {
            id: "team-proxy".into(),
            base_url: Some("https://models.example.com/v1".into()),
            discovery: Some("openai-models-list".into()),
            api_key: Some(KeyReference::env_var("TEAM_PROXY_API_KEY").unwrap()),
            ..Default::default()
        };
        assert!(m.set_custom_provider(&p).is_err());
        p.discovery = Some("proxy".into());
        m.set_custom_provider(&p).unwrap();
        assert!(m.text().contains("    discovery:\n      type: proxy\n"));
    }

    #[test]
    fn key_references_refuse_pasted_keys() {
        assert!(KeyReference::env_var("sk-ant-api03-abcdef").is_err());
        assert!(KeyReference::env_var("my_key").is_err());
        assert!(KeyReference::env_var("OPENAI_API_KEY").is_ok());
        assert!(KeyReference::command("").is_err());
        assert!(KeyReference::command("a\nb").is_err());
    }

    #[test]
    fn cost_overrides_round_trip_fractions() {
        let (_d, mut m) = load("");
        let cost = ModelCost {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        };
        m.set_override("anthropic", "claude-sonnet-5", &OverrideField::Cost(cost)).unwrap();
        assert!(m.text().contains("cost:\n          input: 3\n          output: 15\n          cacheRead: 0.3\n          cacheWrite: 3.75\n"));
        assert_eq!(m.provider("anthropic").unwrap().overrides[0].1.cost, Some(cost));
    }
}
