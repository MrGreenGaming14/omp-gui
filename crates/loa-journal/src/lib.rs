//! Append only prost journal of patches, replay, snapshots and forks.
//!
//! - [`proto`]: the protobuf wire format of patches and DOMs, shared with peers
//! - [`Journal`]: one session's journal file, its snapshots and forks
//! - [`Store`]: every session under a root directory
//! - [`JournalWriter`]: the journal port of a session writer, with batched fsync

mod error;
mod journal;
pub mod proto;
mod record;
mod snapshot;
mod store;
mod writer;

pub use error::{Error, Result};
pub use journal::{
    FORMAT_VERSION, JOURNAL_FILE, Journal, JournalOptions, Parent, Patches, SessionInfo, Truncation,
};
pub use store::{Listing, Store};
pub use writer::{JournalWriter, SyncPolicy};
