//! Domain-free helpers shared by every loa crate. No domain types, no iced.

pub mod async_util;
pub mod exec;
pub mod fs;
pub mod registry;
pub mod ssh;
mod str_enum;
pub mod sync;
#[cfg(feature = "testkit")]
pub mod testkit;
pub mod text;
pub mod time;

pub use sync::{LockExt, RwLockExt};

#[doc(hidden)]
pub use serde as __serde;

str_enum! {
    /// Semantic state for theming; domain status enums map onto it with `fn semantic(&self) -> Semantic`.
    pub Semantic { Ok => "ok", Warn => "warn", Error => "error", Muted => "muted", Busy => "busy", Accent => "accent", Neutral => "neutral" }
}
