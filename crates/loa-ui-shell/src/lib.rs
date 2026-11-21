//! The app frame ([`layout::State`]: sidebar, top bar, right panel, theme) and the command [`palette`].

pub mod data;
pub mod layout;
pub mod palette;
pub mod sidebar;
pub mod theme;
pub mod topbar;

pub use data::{NodeFilter, NodeStatus, Nodes, UsageMeter};
pub use layout::{Event, Message, PanelTab, State};
pub use theme::{Appearance, ThemeRef};
