//! Workflows, their editable rules, and intents from the workflows screen.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workflow {
    pub role: String,
    pub models: Vec<String>,
    pub rules: Rules,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Rules {
    Managed { name: String, allowed: Vec<String>, disallowed: Vec<String>, notes: String, fields: ManagedFields },
    /// A marker-delimited body that cannot be represented by the structured editor.
    External { text: String },
    HandWritten { text: String },
    #[default]
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManagedFields {
    pub description: String,
    pub routing: Vec<RoutingNote>,
    pub steps: Vec<String>,
    pub blocks: Vec<RuleBlock>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutingNote {
    pub agent: String,
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleBlock {
    pub heading: String,
    pub text: String,
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
