//! OpenCode HTTP adapter and lossless native session conversion.

pub mod client;
pub mod driver;
pub mod event;
pub mod ids;
pub mod json;
pub mod mapper;
pub mod native;

pub use driver::{OpenCodeAdapter, OpenCodeSession};

#[cfg(test)]
mod tests;
