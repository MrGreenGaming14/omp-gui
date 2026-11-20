//! Sidebar, top bar, right panel frame and themes.
//!
//! [`layout::State`] is the entry point: it owns the [`sidebar`], the
//! [`topbar`], the right panel and the [`theme`], and draws them around the
//! app's center content and panel tab bodies.

pub mod data;
pub mod layout;
pub mod sidebar;
pub mod theme;
pub mod topbar;

pub use data::{NodeFilter, NodeStatus, Nodes, SessionSummary, UsageMeter};
pub use layout::{Event, Message, PanelPrefs, PanelTab, State};
pub use theme::Preference;
