//! Typed editor for omp's `config.yml`: model roles, fallback chains, enabled
//! models, enabled and disabled providers and provider order.
//!
//! Keys, as omp's `settings.md` and `models.md` define them:
//!
//! | Setting | YAML |
//! | --- | --- |
//! | Role assignments | `modelRoles.<role>`: a selector or comma separated candidates |
//! | Fallback chains | `retry.fallbackChains.<role, provider/model or provider/*>`: a list; `[]` means no fallback |
//! | Enabled models | `enabledModels`: selectors, globs or path scoped `{ path, models }` entries |
//! | Foreign discovery sources | `enabledProviders`: ids or path scoped `{ path, providers }` entries |
//! | Disabled providers | `disabledProviders`: ids or path scoped `{ path, providers }` entries |
//! | Provider order | `modelProviderOrder`: provider ids |
//!
//! Every edit changes only the touched key's lines; saving
//! writes atomically and refuses when the file changed since it was loaded.

use std::path::Path;

use crate::models::error::{Error, Result, SettingsValidator, load_snapshot, save_snapshot};
use loa_util::fs::Snapshot;
use crate::models::yaml::{Yaml, YamlDocument, ymap, ystr, ystrs};
use crate::models::selector::{Selector, validate_role_name};


/// Built in chat roles, in the order omp's model picker lists them.
pub const CHAT_ROLES: [&str; 11] = [
    "default", "smol", "slow", "vision", "plan", "commit", "tiny", "memory", "task", "advisor",
    "widget",
];

/// Built in model-kind roles: they select runners and take no thinking suffix.
pub const KIND_ROLES: [&str; 5] = ["image", "web", "speech", "dictation", "judge"];



/// Whether `role` is one of omp's model-kind roles, which select runners and
/// accept no chat thinking suffix.
pub fn is_kind_role(role: &str) -> bool {
    KIND_ROLES.contains(&role)
}

/// A role and its ordered primary candidates (`modelRoles.<role>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleAssignment {
    pub role: String,
    pub candidates: Vec<Selector>,
}

/// The key a fallback chain applies to (`retry.fallbackChains.<key>`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChainKey {
    /// A role, such as `default` or `judge`.
    Role(String),
    /// An exact `provider/model` selector.
    Model(String),
    /// Every model of a provider, written `provider/*`.
    Provider(String),
}

impl ChainKey {
    /// Parses a key as written in `config.yml`.
    pub fn parse(text: &str) -> Result<Self> {
        let key = if let Some(provider) = text.strip_suffix("/*") {
            Self::Provider(provider.to_owned())
        } else if text.contains('/') {
            Self::Model(text.to_owned())
        } else {
            Self::Role(text.to_owned())
        };
        key.validate()?;
        Ok(key)
    }

    fn validate(&self) -> Result<()> {
        let s = match self {
            Self::Role(r) => return validate_role_name(r),
            Self::Model(m) => m,
            Self::Provider(p) => p,
        };
        if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c.is_control() || c == ',') {
            return Err(Error::invalid("fallback chain key", format!("`{s}` is not a selector")));
        }
        Ok(())
    }

    fn yaml_key(&self) -> String {
        match self {
            Self::Role(r) => r.clone(),
            Self::Model(m) => m.clone(),
            Self::Provider(p) => format!("{p}/*"),
        }
    }
}

/// A fallback chain. An empty list is meaningful: omp then tries no fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackChain {
    pub key: ChainKey,
    pub fallbacks: Vec<Selector>,
}

/// An entry of a path scopable list (`enabledModels`, `enabledProviders`,
/// `disabledProviders`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedEntry {
    /// Applies everywhere.
    Everywhere(String),
    /// Applies when the working directory is one of `paths` or below it.
    Paths { paths: Vec<String>, values: Vec<String> },
}

/// Which path scopable list an entry belongs to; picks the value key omp reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedList {
    /// `enabledModels`: model selectors, globs or fuzzy ids.
    EnabledModels,
    /// `enabledProviders`: foreign discovery sources to load (not a model allowlist).
    EnabledProviders,
    /// `disabledProviders`: model providers or discovery sources to hide.
    DisabledProviders,
}

struct ScopedKey {
    key: &'static str,
    value_key: &'static str,
}

const SCOPED_KEYS: [ScopedKey; 3] = [
    ScopedKey { key: "enabledModels", value_key: "models" },
    ScopedKey { key: "enabledProviders", value_key: "providers" },
    ScopedKey { key: "disabledProviders", value_key: "providers" },
];

struct ListKey {
    key: &'static str,
}

const PROVIDER_ORDER: ListKey = ListKey { key: "modelProviderOrder" };

impl ScopedList {
    fn metadata(self) -> &'static ScopedKey {
        &SCOPED_KEYS[match self {
            Self::EnabledModels => 0,
            Self::EnabledProviders => 1,
            Self::DisabledProviders => 2,
        }]
    }

    fn key(self) -> &'static str { self.metadata().key }
    fn value_key(self) -> &'static str { self.metadata().value_key }
}

const PATH_KEYS: [&str; 4] = ["path", "paths", "pathPrefix", "pathPrefixes"];
const VALUE_KEYS: [&str; 5] = ["models", "providers", "values", "items", "patterns"];

/// omp's `config.yml`, loaded for editing.
#[derive(Debug, Clone)]
pub struct OmpConfig {
    doc: YamlDocument,
    snapshot: Snapshot,
    touched: Vec<Vec<String>>,
}

impl OmpConfig {
    /// Loads `path`; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<Self> {
        let (snapshot, text) = load_snapshot(path)?;
        let doc = YamlDocument::parse(text.as_deref().unwrap_or(""), &path.display().to_string())?;
        Ok(Self {
            doc,
            snapshot,
            touched: Vec::new(),
        })
    }

    /// The edited document text.
    pub fn text(&self) -> &str {
        self.doc.text()
    }

    /// The file this config was loaded from.
    pub fn path(&self) -> &Path {
        self.snapshot.path()
    }

    /// Writes the edits atomically.
    pub fn save(&mut self) -> Result<()> {
        save_snapshot(&mut self.snapshot, self.doc.text())?;
        self.touched.clear();
        Ok(())
    }

    /// Asks `validator` (omp itself) whether it reads the edited keys back as
    /// written, then saves.
    pub async fn save_validated(&mut self, validator: &dyn SettingsValidator) -> Result<()> {
        self.validate(validator).await?;
        self.save()
    }

    /// Asks `validator` (omp itself) whether it reads the keys edited since the
    /// last save back as written. Writes nothing.
    pub async fn validate(&self, validator: &dyn SettingsValidator) -> Result<()> {
        let expected: Vec<(String, serde_json::Value)> = self
            .touched
            .iter()
            .map(|path| {
                let refs: Vec<&str> = path.iter().map(String::as_str).collect();
                let value = self
                    .doc
                    .get(&refs)
                    .map_or(serde_json::Value::Null, crate::models::yaml::to_json);
                (path.join("."), value)
            })
            .collect();
        validator.check_config(self.doc.text(), &expected).await
    }

    /// Whether keys were edited since the load or the last save.
    pub fn is_edited(&self) -> bool {
        !self.touched.is_empty()
    }

    fn set(&mut self, path: &[&str], value: Yaml) -> Result<()> {
        self.doc.set(path, value)?;
        self.touch(path);
        Ok(())
    }

    fn remove(&mut self, path: &[&str]) -> Result<bool> {
        let removed = self.doc.remove(path)?;
        if removed {
            self.touch(path);
        }
        Ok(removed)
    }

    fn touch(&mut self, path: &[&str]) {
        // omp exposes records (modelRoles, retry.fallbackChains) and arrays as
        // one setting, so validation compares the whole setting.
        let setting: Vec<String> = match path {
            ["retry", "fallbackChains", ..] => vec!["retry".into(), "fallbackChains".into()],
            [first, ..] => vec![(*first).to_owned()],
            [] => return,
        };
        if !self.touched.contains(&setting) {
            self.touched.push(setting);
        }
    }

    /// Role assignments in file order. Comma separated values become several
    /// candidates.
    pub fn model_roles(&self) -> Result<Vec<RoleAssignment>> {
        let Some(map) = self.doc.get(&["modelRoles"]) else {
            return Ok(Vec::new());
        };
        let Some(map) = map.as_hash() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for (k, v) in map {
            let (Some(role), Some(value)) = (k.as_str(), v.as_str()) else {
                continue;
            };
            let candidates = value
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(Selector::parse)
                .collect::<Result<Vec<_>>>()?;
            out.push(RoleAssignment {
                role: role.to_owned(),
                candidates,
            });
        }
        Ok(out)
    }

    /// The assignment of one role, if set.
    pub fn model_role(&self, role: &str) -> Result<Option<RoleAssignment>> {
        Ok(self.model_roles()?.into_iter().find(|r| r.role == role))
    }

    /// Assigns `candidates` (first available wins) to `role`. Model-kind roles
    /// refuse thinking suffixes, as omp ignores them there.
    pub fn set_model_role(&mut self, role: &str, candidates: &[Selector]) -> Result<()> {
        validate_role_name(role)?;
        if candidates.is_empty() {
            return Err(Error::invalid(
                "modelRoles",
                "a role needs at least one model; clear it instead",
            ));
        }
        for c in candidates {
            c.validate()?;
            if is_kind_role(role) && c.thinking().is_some() {
                return Err(Error::invalid(
                    "modelRoles",
                    format!("`{role}` selects a runner and takes no thinking suffix"),
                ));
            }
        }
        let value = candidates
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        self.set(&["modelRoles", role], ystr(value))
    }

    /// Removes `role`'s assignment so omp's default applies.
    pub fn clear_model_role(&mut self, role: &str) -> Result<bool> {
        self.remove(&["modelRoles", role])
    }

    /// Fallback chains in file order.
    pub fn fallback_chains(&self) -> Result<Vec<FallbackChain>> {
        let Some(map) = self.doc.get(&["retry", "fallbackChains"]).and_then(Yaml::as_hash) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for (k, v) in map {
            let Some(key) = k.as_str() else { continue };
            let fallbacks = match v {
                Yaml::Array(items) => items
                    .iter()
                    .filter_map(Yaml::as_str)
                    .map(Selector::parse)
                    .collect::<Result<Vec<_>>>()?,
                Yaml::String(s) => vec![Selector::parse(s)?],
                _ => continue,
            };
            out.push(FallbackChain {
                key: ChainKey::parse(key)?,
                fallbacks,
            });
        }
        Ok(out)
    }

    /// Sets the fallback chain for `key`. An empty list writes `[]`, which omp
    /// reads as "no fallback" (unlike removing the key, which restores the
    /// built in chain for model-kind roles).
    pub fn set_fallback_chain(&mut self, key: &ChainKey, fallbacks: &[Selector]) -> Result<()> {
        key.validate()?;
        for f in fallbacks {
            f.validate()?;
        }
        let yaml_key = key.yaml_key();
        self.set(
            &["retry", "fallbackChains", &yaml_key],
            ystrs(fallbacks.iter().map(ToString::to_string)),
        )
    }

    /// Removes the fallback chain for `key`.
    pub fn clear_fallback_chain(&mut self, key: &ChainKey) -> Result<bool> {
        let yaml_key = key.yaml_key();
        self.remove(&["retry", "fallbackChains", &yaml_key])
    }

    /// Entries of a path scopable list. Malformed entries, which omp ignores,
    /// are skipped.
    pub fn scoped_list(&self, list: ScopedList) -> Vec<ScopedEntry> {
        let Some(items) = self.doc.get(&[list.key()]).and_then(Yaml::as_vec) else {
            return Vec::new();
        };
        items.iter().filter_map(parse_scoped).collect()
    }

    /// Replaces a path scopable list. An empty list removes the key, which means
    /// the same to omp (all models, no foreign sources, nothing disabled).
    pub fn set_scoped_list(&mut self, list: ScopedList, entries: &[ScopedEntry]) -> Result<()> {
        if entries.is_empty() {
            self.remove(&[list.key()])?;
            return Ok(());
        }
        let mut items = Vec::with_capacity(entries.len());
        for entry in entries {
            items.push(scoped_yaml(list, entry)?);
        }
        self.set(&[list.key()], Yaml::Array(items))
    }

    /// The provider order used when a bare model id exists under several
    /// providers.
    pub fn provider_order(&self) -> Vec<String> {
        self.string_list(&PROVIDER_ORDER)
    }

    /// Replaces the provider order; an empty order removes the key.
    pub fn set_provider_order(&mut self, providers: &[String]) -> Result<()> {
        self.set_string_list(&PROVIDER_ORDER, providers)
    }

    fn string_list(&self, key: &ListKey) -> Vec<String> {
        self.doc.get(&[key.key]).and_then(Yaml::as_vec)
            .map(|items| items.iter().filter_map(Yaml::as_str).map(str::to_owned).collect())
            .unwrap_or_default()
    }

    fn set_string_list(&mut self, key: &ListKey, values: &[String]) -> Result<()> {
        if values.is_empty() {
            self.remove(&[key.key])?;
            return Ok(());
        }
        let mut seen = std::collections::HashSet::new();
        for value in values {
            validate_value(key.key, value)?;
            if !seen.insert(value.as_str()) {
                return Err(Error::invalid(key.key, format!("`{value}` is listed twice")));
            }
        }
        self.set(&[key.key], ystrs(values.iter().cloned()))
    }
}

fn validate_value(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().any(char::is_control) || value != value.trim() {
        return Err(Error::invalid(field, format!("`{value}` is not a valid entry")));
    }
    Ok(())
}

fn parse_scoped(item: &Yaml) -> Option<ScopedEntry> {
    match item {
        Yaml::String(s) => Some(ScopedEntry::Everywhere(s.clone())),
        Yaml::Hash(map) => {
            let strings = |key: &str| -> Vec<String> {
                match map.get(&ystr(key)) {
                    Some(Yaml::String(s)) => vec![s.clone()],
                    Some(Yaml::Array(a)) => a.iter().filter_map(Yaml::as_str).map(str::to_owned).collect(),
                    _ => Vec::new(),
                }
            };
            let paths: Vec<String> = PATH_KEYS.iter().flat_map(|k| strings(k)).collect();
            let values: Vec<String> = VALUE_KEYS.iter().flat_map(|k| strings(k)).collect();
            if paths.is_empty() || values.is_empty() {
                return None;
            }
            Some(ScopedEntry::Paths { paths, values })
        }
        _ => None,
    }
}

fn scoped_yaml(list: ScopedList, entry: &ScopedEntry) -> Result<Yaml> {
    match entry {
        ScopedEntry::Everywhere(value) => {
            validate_value(list.key(), value)?;
            Ok(ystr(value.clone()))
        }
        ScopedEntry::Paths { paths, values } => {
            if paths.is_empty() || values.is_empty() {
                return Err(Error::invalid(
                    list.key(),
                    "a path scoped entry needs at least one path and one value",
                ));
            }
            for v in paths.iter().chain(values) {
                validate_value(list.key(), v)?;
            }
            let path_yaml = match paths.as_slice() {
                [one] => ("path", ystr(one.clone())),
                many => ("paths", ystrs(many.iter().cloned())),
            };
            Ok(ymap([path_yaml, (list.value_key(), ystrs(values.iter().cloned()))]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "\
# Global omp settings
theme:
  dark: titanium

modelRoles:
  default: anthropic/claude-sonnet-4-5 # day to day
  slow: anthropic/claude-opus-4-5:high
  plan: \"@slow\"

retry:
  maxRetries: 5 # keep this
  fallbackChains:
    default:
      - openai/gpt-5
    judge: []

disabledProviders:
  - ollama
  - path: ~/private
    providers: [anthropic]
";

    fn load(text: &str) -> (tempfile::TempDir, OmpConfig) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yml");
        std::fs::write(&path, text).unwrap();
        let cfg = OmpConfig::load(&path).unwrap();
        (dir, cfg)
    }

    #[test]
    fn reads_roles_with_thinking_suffixes_and_aliases() {
        let (_d, cfg) = load(CONFIG);
        let roles = cfg.model_roles().unwrap();
        assert_eq!(roles.len(), 3);
        assert_eq!(
            roles[1].candidates,
            [Selector::Model {
                model: "anthropic/claude-opus-4-5".into(),
                thinking: Some("high".into())
            }]
        );
        assert_eq!(roles[2].candidates, [Selector::role("slow")]);
    }

    #[test]
    fn assigning_a_role_rewrites_only_that_line_and_saves_atomically() {
        let (dir, mut cfg) = load(CONFIG);
        cfg.set_model_role(
            "default",
            &[Selector::parse("openai/gpt-5:medium").unwrap(), Selector::model("anthropic/claude-sonnet-4-5")],
        )
        .unwrap();
        cfg.save().unwrap();
        let on_disk = std::fs::read_to_string(dir.path().join("config.yml")).unwrap();
        assert_eq!(
            on_disk,
            CONFIG.replace(
                "  default: anthropic/claude-sonnet-4-5 # day to day",
                "  default: openai/gpt-5:medium,anthropic/claude-sonnet-4-5 # day to day"
            )
        );
        let again = OmpConfig::load(&dir.path().join("config.yml")).unwrap();
        assert_eq!(again.model_role("default").unwrap().unwrap().candidates.len(), 2);
    }

    #[test]
    fn new_role_is_quoted_when_it_starts_with_at() {
        let (_d, mut cfg) = load(CONFIG);
        cfg.set_model_role("commit", &[Selector::role("smol")]).unwrap();
        assert!(cfg.text().contains("  plan: \"@slow\"\n  commit: \"@smol\"\n"));
        assert_eq!(cfg.model_role("commit").unwrap().unwrap().candidates, [Selector::role("smol")]);
    }

    #[test]
    fn kind_roles_refuse_thinking_suffixes() {
        let (_d, mut cfg) = load(CONFIG);
        let err = cfg
            .set_model_role("judge", &[Selector::parse("typesafe/jev-1.14.0:high").unwrap()])
            .unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }));
        assert_eq!(cfg.text(), CONFIG);
    }

    #[test]
    fn selectors_with_commas_or_spaces_are_rejected() {
        assert!(Selector::parse("a/b c").is_err());
        assert!(Selector::parse("@").is_err());
        assert!(Selector::model("a,b").validate().is_err());
    }

    #[test]
    fn clearing_the_only_role_removes_model_roles_entirely() {
        let (_d, mut cfg) = load("modelRoles:\n  default: a/b\nother: 1\n");
        assert!(cfg.clear_model_role("default").unwrap());
        assert_eq!(cfg.text(), "other: 1\n");
    }

    #[test]
    fn fallback_chains_keep_empty_lists_and_sibling_retry_settings() {
        let (_d, mut cfg) = load(CONFIG);
        let chains = cfg.fallback_chains().unwrap();
        assert_eq!(chains[1].key, ChainKey::Role("judge".into()));
        assert!(chains[1].fallbacks.is_empty());

        cfg.set_fallback_chain(
            &ChainKey::parse("google-antigravity/*").unwrap(),
            &[Selector::model("google/*"), Selector::role("smol")],
        )
        .unwrap();
        cfg.set_fallback_chain(&ChainKey::Role("default".into()), &[]).unwrap();
        assert!(cfg.text().contains("  maxRetries: 5 # keep this\n"));
        assert!(cfg.text().contains("    default: []\n    judge: []\n    google-antigravity/*:\n      - google/*\n      - \"@smol\"\n"));
        let chains = cfg.fallback_chains().unwrap();
        assert_eq!(chains[2].key, ChainKey::Provider("google-antigravity".into()));
        assert_eq!(chains[2].fallbacks.len(), 2);
    }

    #[test]
    fn removing_the_last_chain_keeps_other_retry_settings() {
        let (_d, mut cfg) = load("retry:\n  maxRetries: 5\n  fallbackChains:\n    smol: [a/b]\n");
        cfg.clear_fallback_chain(&ChainKey::Role("smol".into())).unwrap();
        assert_eq!(cfg.text(), "retry:\n  maxRetries: 5\n");
    }

    #[test]
    fn scoped_lists_round_trip_with_path_entries() {
        let (_d, mut cfg) = load(CONFIG);
        let disabled = cfg.scoped_list(ScopedList::DisabledProviders);
        assert_eq!(
            disabled,
            [
                ScopedEntry::Everywhere("ollama".into()),
                ScopedEntry::Paths {
                    paths: vec!["~/private".into()],
                    values: vec!["anthropic".into()]
                }
            ]
        );
        cfg.set_scoped_list(
            ScopedList::EnabledModels,
            &[
                ScopedEntry::Everywhere("claude-sonnet-4-5".into()),
                ScopedEntry::Paths {
                    paths: vec!["~/work".into(), "~/oss".into()],
                    values: vec!["anthropic/claude-opus-4-5".into()],
                },
            ],
        )
        .unwrap();
        assert!(cfg.text().ends_with(
            "enabledModels:\n  - claude-sonnet-4-5\n  - paths:\n      - ~/work\n      - ~/oss\n    models:\n      - anthropic/claude-opus-4-5\n"
        ));
        assert_eq!(cfg.scoped_list(ScopedList::EnabledModels).len(), 2);
        assert_eq!(cfg.scoped_list(ScopedList::DisabledProviders), disabled);
    }

    #[test]
    fn emptying_a_list_removes_the_key() {
        let (_d, mut cfg) = load(CONFIG);
        cfg.set_scoped_list(ScopedList::DisabledProviders, &[]).unwrap();
        assert!(!cfg.text().contains("disabledProviders"));
        assert!(cfg.text().contains("judge: []"));
    }

    #[test]
    fn provider_order_rejects_duplicates_and_writes_a_list() {
        let (_d, mut cfg) = load(CONFIG);
        assert!(
            cfg.set_provider_order(&["anthropic".into(), "anthropic".into()])
                .is_err()
        );
        cfg.set_provider_order(&["anthropic".into(), "openai".into()]).unwrap();
        assert_eq!(cfg.provider_order(), ["anthropic", "openai"]);
        assert!(cfg.text().ends_with("modelProviderOrder:\n  - anthropic\n  - openai\n"));
    }

    #[test]
    fn missing_file_is_created_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent").join("config.yml");
        let mut cfg = OmpConfig::load(&path).unwrap();
        cfg.set_model_role("default", &[Selector::model("openai/gpt-5")]).unwrap();
        cfg.save().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "modelRoles:\n  default: openai/gpt-5\n"
        );
    }
}
