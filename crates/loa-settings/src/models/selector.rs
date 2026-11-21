//! Model and role selector grammar.

use std::fmt;

use crate::models::error::{Error, Result};

/// Thinking levels a chat selector may carry as a `:level` suffix.
pub const THINKING_LEVELS: [&str; 9] = [
    "off", "minimal", "low", "medium", "high", "xhigh", "max", "auto", "inherit",
];

pub fn validate_role_name(role: &str) -> Result<()> {
    if role.is_empty()
        || !role
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Error::invalid(
            "role",
            format!("`{role}` must be letters, digits, `_` or `-`"),
        ));
    }
    Ok(())
}

/// One candidate of a role assignment or fallback chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// A model selector: `provider/model`, a bare or fuzzy id, or a glob.
    Model {
        model: String,
        thinking: Option<String>,
    },
    /// Another role, written `@role`.
    Role {
        role: String,
        thinking: Option<String>,
    },
}

impl Selector {
    /// A model selector without a thinking suffix.
    pub fn model(model: impl Into<String>) -> Self {
        Self::Model {
            model: model.into(),
            thinking: None,
        }
    }

    /// A role alias without a thinking suffix.
    pub fn role(role: impl Into<String>) -> Self {
        Self::Role {
            role: role.into(),
            thinking: None,
        }
    }

    /// Parses one candidate such as `anthropic/claude-opus-4-5:high` or `@smol`.
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::invalid("selector", "empty selector"));
        }
        let (body, thinking) = match text.rsplit_once(':') {
            Some((body, level)) if THINKING_LEVELS.contains(&level) => {
                (body, Some(level.to_owned()))
            }
            _ => (text, None),
        };
        let selector = match body.strip_prefix('@') {
            Some(role) => Self::Role {
                role: role.to_owned(),
                thinking,
            },
            None => Self::Model {
                model: body.to_owned(),
                thinking,
            },
        };
        selector.validate()?;
        Ok(selector)
    }

    /// The model part of a model selector.
    pub fn model_name(&self) -> Option<&str> {
        match self {
            Self::Model { model, .. } => Some(model),
            Self::Role { .. } => None,
        }
    }

    /// The thinking suffix, if any.
    pub fn thinking(&self) -> Option<&str> {
        match self {
            Self::Model { thinking, .. } | Self::Role { thinking, .. } => thinking.as_deref(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        let (name, thinking) = match self {
            Self::Model { model, thinking } => (model.as_str(), thinking),
            Self::Role { role, thinking } => {
                validate_role_name(role)?;
                (role.as_str(), thinking)
            }
        };
        if name.is_empty() || name.chars().any(|c| c.is_whitespace() || c.is_control() || c == ',') {
            return Err(Error::invalid(
                "selector",
                format!("`{name}` must be non-empty without spaces or commas"),
            ));
        }
        if let Some(level) = thinking
            && !THINKING_LEVELS.contains(&level.as_str())
        {
            return Err(Error::invalid("selector", format!("unknown thinking level `{level}`")));
        }
        Ok(())
    }
}

impl std::fmt::Display for Selector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model { model, thinking } => {
                f.write_str(model)?;
                if let Some(t) = thinking {
                    write!(f, ":{t}")?;
                }
            }
            Self::Role { role, thinking } => {
                write!(f, "@{role}")?;
                if let Some(t) = thinking {
                    write!(f, ":{t}")?;
                }
            }
        }
        Ok(())
    }
}

/// One model, named the way omp's `set_model` takes it: a provider id and the
/// model id within that provider.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self { provider: provider.into(), id: id.into() }
    }

    /// Splits a `provider/id` selector without allocating. IDs may contain `/`.
    pub fn split(selector: &str) -> Option<(&str, &str)> {
        selector.split_once('/').filter(|(provider, id)| !provider.is_empty() && !id.is_empty())
    }

    /// Parses the `provider/id` selector form.
    pub fn parse(selector: &str) -> Option<Self> {
        let (provider, id) = Self::split(selector)?;
        Some(Self::new(provider, id))
    }

    /// The `provider/id` selector, the value written to `<session model>`.
    pub fn selector(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.id)
    }
}

/// A model the user can pick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub model: ModelRef,
    /// Display name, e.g. "Claude Sonnet 4.5". Empty falls back to the id.
    pub name: String,
    /// Display name of the provider, e.g. "Anthropic". Empty falls back to
    /// the provider id.
    pub provider_name: String,
}

impl ModelInfo {
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() { &self.model.id } else { &self.name }
    }

    pub fn provider_display_name(&self) -> &str {
        if self.provider_name.is_empty() { &self.model.provider } else { &self.provider_name }
    }
}

/// A model role (`default`, `smol`, `slow`, `plan`, …) and the model it
/// currently resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleAssignment {
    pub role: String,
    pub model: ModelRef,
}
/// The models and role assignments the picker offers, as omp lists them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    pub models: Vec<ModelInfo>,
    pub roles: Vec<RoleAssignment>,
}

#[cfg(test)]
mod tests {
    use super::{ModelRef, Selector};

    #[test]
    fn model_references_split_only_the_first_slash() {
        assert_eq!(ModelRef::split("openrouter/vendor/model"), Some(("openrouter", "vendor/model")));
        assert_eq!(ModelRef::parse("openrouter/vendor/model"), Some(ModelRef::new("openrouter", "vendor/model")));
        for invalid in ["model", "/model", "provider/"] {
            assert_eq!(ModelRef::split(invalid), None);
            assert_eq!(ModelRef::parse(invalid), None);
        }
    }

    #[test]
    fn role_and_thinking_suffix_round_trip() {
        for text in ["@slow", "provider/model:high", "provider/vendor/model"] {
            assert_eq!(Selector::parse(text).unwrap().to_string(), text);
        }
    }
}
