//! Right panel tabs: diff, files, session tree, agents.
//!
//! Each tab is one component in the shape every UI crate uses: a `State`
//! with `update` and `view`, an internal `Message` and an `Event` the app acts
//! on.
//!
//! - [`diff`]: unified and side by side diffs of one or many files
//! - [`files`]: lazy file tree over a [`files::FileSystem`] port
//! - [`session_tree`]: fork and rewind tree of a session family
//! - [`agents`]: live subagents and background jobs from `<job>` elements

pub mod agents;
pub mod diff;
pub mod files;
pub mod session_tree;
mod style;
