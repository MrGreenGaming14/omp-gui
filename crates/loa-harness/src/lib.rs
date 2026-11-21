//! Harness adapters. Each harness is a module behind its own cargo feature,
//! built on the shared protocol machinery in [`kit`].

pub mod account_support;
pub mod kit;
pub mod native;
pub mod registry;

#[cfg(feature = "acp")]
pub mod acp;
#[cfg(feature = "claude")]
pub mod claude;
#[cfg(feature = "codex")]
pub mod codex;
#[cfg(feature = "omp")]
pub mod omp;
#[cfg(feature = "omp2")]
pub mod omp2;
#[cfg(feature = "opencode")]
pub mod opencode;
#[cfg(feature = "testing")]
pub mod testing;

pub use registry::{HarnessEnv, factories};
