//! Composer rows the app feeds the pickers and popups with, and what the composer asks of it.

use std::path::PathBuf;

use super::{Prompt, SessionMode};

/// An image pasted or dropped into the composer, saved at `path`: shown as `[Image #n]` and a
/// card, and sent as an [`Attachment`](super::Attachment) while its token is in the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PastedImage {
    pub path: PathBuf,
    /// Width and height in pixels, when known.
    pub size: Option<(u32, u32)>,
}

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

/// The source of a workflow's rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkflowKind { Managed, HandWritten, #[default] None }

/// A workflow offered by the composer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkflowOption {
    pub role: String,
    pub label: String,
    pub kind: WorkflowKind,
    pub current: bool,
}

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
    /// Ctrl+V was pressed. When the clipboard holds an image, save it and hand it back with
    /// the composer's `attach_image`; the editor pastes any text itself.
    PasteImage,
    /// A project was picked in the project chip: the shown session moves there, or new
    /// sessions join it. `None` is "No project".
    ProjectPicked(Option<String>),
    WorkflowPicked(String),
    OpenRules,
}
