//! Command palette and keymap.
//!
//! - [`keymap`]: the app's keyboard shortcuts as data, platform aware, with
//!   conflict detection and resolution of iced keyboard events to command ids.
//! - [`fuzzy`]: subsequence matching and ranking used by the palette filter.
//! - [`palette`]: the Ctrl/Cmd+K overlay component (`State`, `Message`, `Event`).
//!
//! Commands are plain data identified by [`CommandId`]. The app registers them
//! with the palette, binds chords to them in the [`keymap::Keymap`], and maps
//! the ids it receives back to actions.

pub mod fuzzy;
pub mod keymap;
pub mod palette;

use std::borrow::Cow;
use std::fmt;

pub use keymap::{Binding, Chord, ChordKey, Keymap, Platform, Scope};
pub use palette::{Command, Event, Message, State};

/// Stable identifier of an app command, such as `panel.toggle`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandId(Cow<'static, str>);

impl CommandId {
    /// An id from a string literal, usable in `const` items.
    pub const fn from_static(id: &'static str) -> Self {
        Self(Cow::Borrowed(id))
    }

    /// An id built at runtime, e.g. for commands contributed by MCP servers.
    pub fn new(id: impl Into<String>) -> Self {
        Self(Cow::Owned(id.into()))
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Ids of the commands this crate binds by default.
pub mod ids {
    use super::CommandId;

    /// Open the palette, or close it when it is open.
    pub const PALETTE_TOGGLE: CommandId = CommandId::from_static("palette.toggle");
    /// Show or hide the right panel.
    pub const PANEL_TOGGLE: CommandId = CommandId::from_static("panel.toggle");
    /// Interrupt the streaming turn of the focused session.
    pub const SESSION_INTERRUPT: CommandId = CommandId::from_static("session.interrupt");
    /// Send the composer's text.
    pub const COMPOSER_SUBMIT: CommandId = CommandId::from_static("composer.submit");
    /// Insert a line break in the composer.
    pub const COMPOSER_NEWLINE: CommandId = CommandId::from_static("composer.newline");
}
