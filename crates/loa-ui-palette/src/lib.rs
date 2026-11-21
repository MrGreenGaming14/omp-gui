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

pub use loa_ui_kit::text::fuzzy;
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
    /// Start a new session.
    pub const SESSION_NEW: CommandId = CommandId::from_static("session.new");
    /// Show the next session in the sidebar's order (Pinned, then Recents).
    pub const SESSION_NEXT: CommandId = CommandId::from_static("session.next");
    /// Show the previous session in the sidebar's order.
    pub const SESSION_PREVIOUS: CommandId = CommandId::from_static("session.previous");
    /// Show the next project tab (shown only with the project tabs setting).
    pub const PROJECT_TAB_NEXT: CommandId = CommandId::from_static("project.tab.next");
    /// Show the previous project tab.
    pub const PROJECT_TAB_PREVIOUS: CommandId = CommandId::from_static("project.tab.previous");
    /// Put the keyboard focus in the composer.
    pub const COMPOSER_FOCUS: CommandId = CommandId::from_static("composer.focus");
    /// Put the keyboard focus in the sidebar's session search.
    pub const SEARCH_FOCUS: CommandId = CommandId::from_static("sidebar.search");
    /// Open the palette listing only keyboard shortcuts.
    pub const KEYMAP_SHOW: CommandId = CommandId::from_static("keymap.show");
    /// Move the keyboard focus to the next text field.
    pub const FOCUS_NEXT: CommandId = CommandId::from_static("focus.next");
    /// Move the keyboard focus to the previous text field.
    pub const FOCUS_PREVIOUS: CommandId = CommandId::from_static("focus.previous");
    /// Prefix of `session.goto.<n>`: show the n-th session (1 based) in the
    /// sidebar's order.
    pub const SESSION_GOTO: &str = "session.goto.";
    /// Prefix of `panel.tab.<n>`: show the n-th right panel tab (1 based).
    pub const PANEL_TAB: &str = "panel.tab.";

    /// `session.goto.<n>`.
    pub fn session_goto(n: usize) -> CommandId {
        CommandId::new(format!("{SESSION_GOTO}{n}"))
    }

    /// `panel.tab.<n>`.
    pub fn panel_tab(n: usize) -> CommandId {
        CommandId::new(format!("{PANEL_TAB}{n}"))
    }
}
