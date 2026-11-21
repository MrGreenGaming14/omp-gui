//! The app frame ([`layout::State`]: sidebar, top bar, right panel, theme) and the command [`palette`].
//! It reads and emits the plain types of [`loa_view::shell`].

pub mod data;
pub mod layout;
pub mod palette;
pub mod sidebar;
pub mod theme;
pub mod topbar;

pub use data::Nodes;
pub use layout::{Message, State};
