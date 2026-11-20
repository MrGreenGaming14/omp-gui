//! Harness, model, effort and mode pickers. The app fills them from the harness
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

/// A row under the model choices that opens something instead of picking a
/// model, e.g. the full model list or the model settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerAction {
    /// Id reported by [`Event::ModelAction`](crate::Event::ModelAction).
    pub id: String,
    pub label: String,
}

/// A harness prompts can be routed to, e.g. `omp` or `claude`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessOption {
    /// Id reported by [`Event::HarnessChanged`](crate::Event::HarnessChanged).
    pub id: String,
    /// Name shown in the picker.
    pub label: String,
    /// Whether it can run here. Unavailable harnesses are listed, not picked.
    pub available: bool,
    /// Muted note beside the label, e.g. `not installed`.
    pub detail: Option<String>,
}

/// A permission level: what the agent may do without asking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessOption {
    /// Id the app acts on, e.g. omp's approval mode.
    pub id: String,
    /// Short label on the picker, e.g. "Full access".
    pub label: String,
    /// One line on what it allows.
    pub detail: String,
}

/// One of the composer's bottom-row pickers.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Picker {
    Harness,
    Model,
    Effort,
    Mode,
    /// What the agent may do without asking.
    Access,
    /// The folder bar above the editor.
    Folder,
}

/// Picker contents and the current choices.
#[derive(Debug)]
pub struct Pickers {
    pub harnesses: Vec<HarnessOption>,
    /// Id of the current harness.
    pub harness: Option<String>,
    pub models: Vec<ModelOption>,
    pub model: Option<ModelOption>,
    /// Rows listed after the models.
    pub model_actions: Vec<PickerAction>,
    /// Effort levels the current model supports. Empty hides the picker.
    pub efforts: Vec<String>,
    pub effort: Option<String>,
    pub mode: SessionMode,
    /// Permission levels; empty hides the picker.
    pub access: Vec<AccessOption>,
    /// Id of the current permission level.
    pub access_current: Option<String>,
    /// The picker whose menu is open, if any.
    pub open: Option<Picker>,
}

impl Default for Pickers {
    fn default() -> Self {
        Self {
            harnesses: Vec::new(),
            harness: None,
            models: Vec::new(),
            model: None,
            model_actions: Vec::new(),
            efforts: Vec::new(),
            effort: None,
            mode: SessionMode::Normal,
            access: Vec::new(),
            access_current: None,
            open: None,
        }
    }
}
