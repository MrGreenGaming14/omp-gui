//! Workflows, their editable rules, and intents from the workflows screen.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workflow {
    pub role: String,
    pub models: Vec<String>,
    pub rules: Rules,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Rules {
    Managed { name: String, allowed: Vec<String>, disallowed: Vec<String>, notes: String },
    HandWritten { text: String },
    #[default]
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub workflow: Workflow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentInfo {
    pub name: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    ChangedOnDisk,
    Message(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    Open(String),
    New,
    Save { draft: Draft, previous_role: Option<String> },
    Delete(String),
    Reload,
    Close,
}
