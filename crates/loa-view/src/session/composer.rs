//! Composer rows the app feeds the pickers and popups with, and what the composer asks of it.

use super::{Prompt, SessionMode};

/// A `/` command offered in the popup; `name` has no slash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub description: String,
}

/// A model the session can switch to; `id` is what `set_model` takes, `role` the role it fills.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelOption {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub role: Option<String>,
}

impl std::fmt::Display for ModelOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(role) = &self.role {
            write!(f, "{role} · ")?;
        }
        write!(f, "{} ({})", self.name, self.provider)
    }
}

/// A row under the model choices that opens something instead of picking a model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerAction {
    pub id: String,
    pub label: String,
}

/// A harness prompts can be routed to; unavailable ones are listed, not picked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HarnessOption {
    pub id: String,
    pub label: String,
    pub available: bool,
    pub detail: Option<String>,
}

/// A permission level: what the agent may do without asking.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessOption {
    pub id: String,
    pub label: String,
    pub detail: String,
}

/// The folder a session works in: `path` shortened for display, `full` on hover.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Workspace {
    pub path: String,
    pub full: String,
    pub branch: Option<String>,
}

/// A plain-text picker option (effort levels, modes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Label(pub String);

/// What the composer asks the app to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    /// Start a turn: typed while idle, or the next queued message at a turn end. `@` mentions of
    /// known files are attachments.
    Submit(Prompt),
    /// Interrupt the running turn; queued messages went back into the editor.
    Stop,
    QueueChanged(Vec<String>),
    /// The `@` popup opened: pass the project's files back.
    FilesRequested { query: String },
    ModelChanged(ModelOption),
    EffortChanged(String),
    ModeChanged(SessionMode),
    AccessChanged(String),
    HarnessChanged(String),
    /// A model action row, by id.
    ModelAction(String),
    /// The folder menu's "Show in Files", a folder by full path, and "Choose another folder…".
    OpenWorkspace,
    FolderPicked(String),
    BrowseFolders,
}
