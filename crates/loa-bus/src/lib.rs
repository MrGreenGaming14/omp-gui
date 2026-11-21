//! Session writer, publish subscribe of patches with per subscriber
//! backpressure, and the append only journal they are persisted to.
//!
//! - [`SessionWriter`]: the [`loa_core::PatchSink`] of a live session. Only the
//!   owner node orders patches; peers send requests that pass an [`Authorize`]
//!   check first. Every patch is persisted through [`Persist`] before it is
//!   applied and published.
//! - [`Subscription`]: a subscriber's own bounded queue of patches, with a
//!   [`Policy`] that decides how it is fed and when it lags.
//! - [`Bus`]: the registry of live sessions.
//! - [`Journal`]: one session's journal file, its snapshots and forks;
//!   [`Store`] holds every session under a root directory, and
//!   [`JournalPersist`] is the [`Persist`] adapter over a [`JournalWriter`].
//! - [`proto`]: the protobuf wire format of patches and DOMs, shared with peers.

mod bus;
mod error;
mod frame;
mod journal;
pub mod proto;
mod queue;
mod snapshot;
mod store;
mod writer;

pub use bus::{Bus, BusError};
pub use error::JournalError;
pub use journal::{FORMAT_VERSION, JOURNAL_FILE, Journal, JournalOptions, Parent, Patches, SessionInfo, Truncation};
pub use queue::{Appends, Event, Policy, Subscription, TryRecvError};
pub use store::{JournalPersist, JournalWriter, Listing, Store, SyncPolicy, SyncStrategy};
pub use writer::{Authorize, Denied, Persist, PersistError, ReplicateError, RequestError, SessionWriter, WriterConfig};

use std::fmt;
use std::sync::Arc;

/// Id of a session, the `id` attribute of its `<session>`. Cheap to clone.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(Arc<str>);

impl SessionId {
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SessionId {
    fn from(id: &str) -> Self {
        Self::new(id)
    }
}

impl From<String> for SessionId {
    fn from(id: String) -> Self {
        Self::new(id)
    }
}

impl std::borrow::Borrow<str> for SessionId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A node's ed25519 public key: its network address and the identity the
/// single writer rule and peer authorization are checked against.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeKey(pub [u8; 32]);

impl fmt::Debug for NodeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NodeKey(")?;
        for byte in &self.0[..4] {
            write!(f, "{byte:02x}")?;
        }
        f.write_str("…)")
    }
}
