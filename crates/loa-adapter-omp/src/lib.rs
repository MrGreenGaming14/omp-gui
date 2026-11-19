//! omp adapter over RPC.

pub mod launch;
pub mod mapping;
pub mod native;
mod runtime;

pub use launch::{MemoryMode, OmpAdapterConfig};
pub use runtime::{OmpAdapter, OmpSession};
