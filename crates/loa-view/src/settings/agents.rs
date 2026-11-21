//! The agents screen: the omp agent definitions found on disk and the form that builds one, mirrored
//! from the agent definition domain. The app checks a form's fields; the screen shows the
//! [`FieldError`]s it reports next to their fields.

use std::path::PathBuf;

/// Where an agent file was found: the user agents folder or the project's `.omp/agents`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AgentSource {
    User,
    Project,
}

/// The `model` key: one model or role alias, or a fallback chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelSpec {
    One(String),
    Chain(Vec<String>),
}

impl ModelSpec {
    /// Every model entry in order.
    pub fn entries(&self) -> &[String] {
        match self {
            Self::One(m) => std::slice::from_ref(m),
            Self::Chain(c) => c,
        }
    }
}

/// The parts of a definition the form edits. `None` leaves the key out, so omp's default applies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentFields {
    pub name: String,
    pub description: String,
    pub model: Option<ModelSpec>,
    pub thinking_level: Option<String>,
    pub tools: Option<Vec<String>>,
    pub spawns: Option<Vec<String>>,
    /// The agent's system prompt.
    pub body: String,
}

/// An agent file on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEntry {
    pub path: PathBuf,
    pub source: AgentSource,
    pub enabled: bool,
    pub fields: AgentFields,
}

/// What the form offers: models and role aliases, thinking levels and tools.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choices {
    /// `provider/model` ids and role aliases such as `@slow`.
    pub models: Vec<String>,
    pub thinking_levels: Vec<String>,
    pub tools: Vec<String>,
    /// Where the tool list comes from when it isn't omp's own.
    pub tools_note: Option<String>,
}

/// A problem with one field (`"form"` for the form as a whole).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}

/// The form as the app writes it: the edited fields, the listed file they started from (whose keys
/// the form has no field for are kept) and the file being edited.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub fields: AgentFields,
    /// The file the form was filled from (edited or copied); `None` for a blank agent.
    pub base: Option<PathBuf>,
    /// The file the form edits; `None` for a new agent or a copy.
    pub editing: Option<PathBuf>,
}

/// What the app does for the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Check the draft and answer with its errors; sent by edits once Save showed errors.
    Check(Draft),
    /// Check the draft and, when it has no errors, write it: back to the file it edits or to a new
    /// file in the user agents folder. Answer with the errors either way.
    Save(Draft),
    /// The user confirmed replacing the agent file at the path with the draft.
    Replace(Draft, PathBuf),
    /// Enable or disable the agent file at the path (the opposite of its state now).
    Toggle(PathBuf),
    /// The user confirmed deleting the agent file at the path.
    Delete(PathBuf),
    Close,
}
