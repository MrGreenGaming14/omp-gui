//! Desktop application wiring: the components own presentation state while the
//! adapters own execution and the session DOM, persisted in its journal,
//! remains the source of truth.
//!
//! - [`app`]: the `App`, its `Message`, update, view and subscription
//! - [`area`]: the contract feature areas implement and the `Ctx` they get
//! - [`areas`]: one module per feature area
//! - [`services`]: identity, data directories, journal store, bus, harnesses
//! - [`session`]: live sessions persisted through their journals

pub mod app;
pub mod area;
pub mod areas;
pub mod services;
pub mod session;
