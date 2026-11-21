//! Per-project settings and what the project screen shows about a project's sessions.

use loa_core::NodeId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A file every chat of the project should know about, with why it is pinned.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Settings shared by every session of a project; `digest_model` `None` uses the summarizer's default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSettings {
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub pinned: Vec<PinnedFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_model: Option<String>,
    /// Working directory of the project, set when it is created or its folder changed; `None` when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
}

/// One touched file and a one line note.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileNote {
    pub path: String,
    #[serde(default)]
    pub note: String,
}

impl FileNote {
    /// Parses the schema's `"path: note"` form.
    pub fn parse(text: &str) -> Self {
        let (path, note) = text.split_once(": ").unwrap_or((text, ""));
        Self { path: path.to_owned(), note: note.to_owned() }
    }

    pub fn encode(&self) -> String {
        if self.note.is_empty() { self.path.clone() } else { format!("{}: {}", self.path, self.note) }
    }
}

/// What a digest says about its session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DigestContent {
    pub summary: String,
    pub decisions: Vec<String>,
    pub files: Vec<FileNote>,
    /// Unfinished todos and unanswered questions.
    pub open_items: Vec<String>,
    /// The turns the digest came from.
    pub pointers: Vec<NodeId>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DigestState {
    Missing,
    Fresh,
    Stale,
}

/// One session on the project screen.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub harness: Option<String>,
    pub created: i64,
    pub last_active: i64,
    pub turns: usize,
    pub excluded: bool,
    pub digest_state: DigestState,
    pub digest: Option<DigestContent>,
    pub digest_model: Option<String>,
}

/// Everything the project screen shows; sessions newest activity first.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectOverview {
    pub name: String,
    pub cwd: Option<String>,
    pub settings: ProjectSettings,
    pub sessions: Vec<SessionRow>,
}
