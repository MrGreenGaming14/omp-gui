//! Desktop application wiring: the components own presentation state while the
//! adapters own execution and the session DOM, persisted in its journal,
//! remains the source of truth.

pub mod app;
pub mod area;
pub mod areas;
pub mod services;
pub mod session;
pub mod watch;

#[cfg(test)]
mod testing;
