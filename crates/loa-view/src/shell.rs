//! The app frame: sidebar sessions, nodes and usage, the right panel's tabs, and what the frame asks the app to do.

pub use loa_contracts::session::SessionSummary;

use crate::theme::{Icon, ThemeRef};

/// The sidebar footer's usage meter: the used share (0 to 1) of the tightest limit and its window, e.g. "7d".
#[derive(Clone, Debug, PartialEq)]
pub struct UsageMeter {
    pub fraction: f32,
    pub label: String,
}

/// Which machine's sessions the sidebar lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NodeFilter {
    #[default]
    All,
    Node(String),
}

impl NodeFilter {
    pub fn matches(&self, node: &str) -> bool {
        if let Self::Node(key) = self { key == node } else { true }
    }
}

/// A machine the sidebar lists sessions of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeStatus {
    /// Public key, as written in `<session node>`.
    pub key: String,
    pub name: String,
    pub online: bool,
    /// This is the node the app runs on.
    pub local: bool,
}

/// One tab of the right panel; the app draws the body of the active one by `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelTab {
    pub id: String,
    pub title: String,
    pub icon: Option<Icon>,
}

impl PanelTab {
    pub fn new(id: impl Into<String>, title: impl Into<String>, icon: Option<Icon>) -> Self {
        Self { id: id.into(), title: title.into(), icon }
    }
}

/// What the frame asks the app to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    /// Navigation from the sidebar, the top bar crumb or the project tabs.
    Sidebar(SidebarIntent),
    /// Drop this project's tab; its sessions stay open.
    CloseProjectTab(String),
    DetachTab(String),
    ShowDetached(String),
    ReattachTab(String),
    /// The user picked a theme; persist it.
    SetTheme(ThemeRef),
    /// The active tokens changed to this generation; rebuild cached looks.
    ThemeChanged(u64),
}

/// Navigation, pin and project changes from the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarIntent {
    NewSession,
    OpenSession(String),
    OpenProject(String),
    OpenPeers,
    OpenTools,
    OpenUpdates,
    OpenUsage,
    OpenConfigs,
    OpenSettings,
    SetPinned { id: String, pinned: bool },
    SetProjectPinned { project: String, pinned: bool },
    DeleteSession(String),
    /// Open the New project screen: the `+` in the Projects header.
    NewProject,
    /// Start a new session in this project, in its folder: the `+` on its row.
    NewSessionIn(String),
    /// Ask to delete this project; its sessions are kept.
    DeleteProject(String),
    /// Move a session to a project, or out of every project with `None`.
    MoveSession { id: String, project: Option<String> },
}
