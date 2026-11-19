//! Model, effort and mode pickers. The app fills them from the harness
//! (omp's `get_available_models`, the session's `model`, `model_role`,
//! `effort` and `mode` attributes) and acts on the change events.

use std::fmt;

use loa_core::SessionMode;

/// A model the session can switch to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    /// Id the harness takes in `set_model`, e.g. `anthropic/claude-sonnet-4`.
    pub id: String,
    /// Name shown in the picker.
    pub name: String,
    pub provider: String,
    /// Model role this model fills (`default`, `smol`, `slow`, …), if any.
    pub role: Option<String>,
}

impl fmt::Display for ModelOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.role {
            Some(role) => write!(f, "{role} · {} ({})", self.name, self.provider),
            None => write!(f, "{} ({})", self.name, self.provider),
        }
    }
}

/// Picker contents and the current choices.
#[derive(Debug)]
pub struct Pickers {
    pub models: Vec<ModelOption>,
    pub model: Option<ModelOption>,
    /// Effort levels the current model supports. Empty hides the picker.
    pub efforts: Vec<String>,
    pub effort: Option<String>,
    pub mode: SessionMode,
}

impl Default for Pickers {
    fn default() -> Self {
        Self { models: Vec::new(), model: None, efforts: Vec::new(), effort: None, mode: SessionMode::Normal }
    }
}
