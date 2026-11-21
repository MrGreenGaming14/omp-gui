//! Sign in, model listing and model settings through omp and other harnesses.
//!
//! The app has no provider code of its own. This crate is the logic behind the
//! sign in screen, the model picker and the model settings screen:
//!
//! - `loa_contracts::harness::model_rpc` declares the omp RPC port: the commands this crate
//!   needs (`get_login_providers`, `login`, `get_available_models`, `set_model`)
//!   and the login UI frames omp sends while a login runs.
//! - `loa_harness::omp::account` implements that port over a dedicated
//!   `omp --mode rpc --no-session` process and locates omp's settings files.
//! - [`service`] and [`login`] list providers and drive login flows.
//! - [`keys`] reports where omp would read a provider's API key from, without ever
//!   reading or returning a key value.
//! - `loa_harness::omp::config` and [`models_yml`] edit omp's `config.yml` and
//!   `models.yml` in place, keeping comments and ordering of untouched keys.
//! - [`validate`] checks edited files with omp itself, without a model call.
//! - Harness accounts and process execution live in `loa-harness`.
//!
//! # API keys never pass through the app
//!
//! No function in this crate accepts, stores or returns an API key value. The only
//! way to write a provider's `apiKey` in `models.yml` is a [`models_yml::KeyReference`],
//! which can only be built from an environment variable name or a `!command`.
//! Reading reports a [`models_yml::ApiKeySetting::Literal`] with no payload when
//! the file holds an inline key, and [`keys`] reports sources by name only.

pub mod keys;
pub mod login;
pub mod models_yml;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod settings;
pub mod service;
#[cfg(test)]
mod service_tests;
pub mod validate;

/// The model and role selector grammar shared with the harness adapters.
pub use loa_contracts::harness::selector;

