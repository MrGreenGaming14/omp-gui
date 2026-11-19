//! Release watching, staging, gate checks, swap and rollback.
//!
//! The flow for one product: [`feeds`] find a newer [`Release`], [`stage`]
//! installs it side by side under `<root>/versions/<product>/<version>/`,
//! [`gates`] check it against what the app depends on, [`decision`] turns the
//! [`gates::GateReport`] into swap or hold, and [`swap`] switches the active
//! version between sessions, keeping the previous one for rollback. App
//! updates must pass [`signature`] verification before the swap. [`peers`]
//! classifies peers by version for the Peers list.

mod fsio;

pub mod decision;
pub mod feeds;
pub mod gates;
pub mod http;
pub mod layout;
pub mod peers;
pub mod product;
pub mod signature;
pub mod stage;
pub mod state;
pub mod swap;

pub use layout::{Staged, StagedManifest};
pub use product::{
    ArchiveFormat, Artifact, CachedFeed, Checksum, Feed, PrereleasePolicy, Product, ProductKind,
    Release,
};
