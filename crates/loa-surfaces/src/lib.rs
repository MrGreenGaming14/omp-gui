//! Surfaces: what the agent and the user look at and drive.
//!
//! - [`browser`]: tabs of the installed Chrome as pixel surfaces
//! - [`docs`]: PDF, Office and image files as document surfaces
//! - [`vnc`]: RFB endpoints as pixel surfaces
//! - [`hub`]: the app's surface service and the `surface_*` agent tools,
//!   re-exported at the crate root
//! - [`pixels`] and [`feed`]: the RGBA buffer, codecs and frame fan-out the
//!   servers and the viewer share
//! - `proto` (feature `remote-video`): `omp/surface/1` and live video

#[cfg(feature = "browser")]
pub mod browser;
#[cfg(feature = "docs")]
pub mod docs;
pub mod feed;
#[cfg(feature = "hub")]
pub mod hub;
pub mod pixels;
#[cfg(feature = "remote-video")]
pub mod proto;
#[cfg(feature = "testkit")]
pub mod testkit;
#[cfg(feature = "vnc")]
pub mod vnc;

#[cfg(feature = "hub")]
pub use hub::*;
