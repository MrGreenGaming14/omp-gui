//! Models as the app receives them from omp (`get_available_models`) and the
//! roles they serve (`modelRoles`).

use std::fmt;

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

    /// Parses the `provider/id` selector form. The model id may itself contain
    /// `/` (OpenRouter ids do), so only the first `/` separates.
    pub fn parse(selector: &str) -> Option<Self> {
        let (provider, id) = selector.split_once('/')?;
        (!provider.is_empty() && !id.is_empty()).then(|| Self::new(provider, id))
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
