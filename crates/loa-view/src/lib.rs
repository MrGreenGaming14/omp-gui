//! Stable presentation API between loa-app and the UI crates.
//!
//! UI crates read the plain view models in these modules and emit each screen's
//! `Intent`; loa-app projects domain state into views and maps intents back.
//! The public surface is versioned by [`VERSION`] and snapshotted in `api.txt`.

/// Semver version of this presentation API. Bump it whenever `api.txt` changes:
/// major for removals/renames, minor for additions.
pub const VERSION: &str = "3.2.0";

pub mod panel;
pub mod session;
pub mod settings;
pub mod shell;
pub mod surface;
pub mod terminal;
pub mod theme;
