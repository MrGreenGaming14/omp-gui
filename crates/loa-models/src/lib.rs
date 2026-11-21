//! Sign in, model listing and model settings through omp and other harnesses.
//!
//! The app has no provider code of its own. This crate is the logic behind the
//! sign in screen, the model picker and the model settings screen:
//!
//! - [`rpc`] declares the omp RPC port: the commands this crate needs
//!   (`get_login_providers`, `login`, `get_available_models`, `set_model`)
//!   and the login UI frames omp sends while a login runs.
//! - [`login::OmpProviderAccount`] and [`files::OmpFilesResolver`] are the
//!   ports for omp's provider logins and its active agent directory;
//!   [`error::SettingsValidator`] checks edited files.
//! - `loa-harness` implements those ports over a dedicated
//!   `omp --mode rpc --no-session` process and omp's own CLI; the app wires
//!   them into a [`service::Backend`] through [`service::Parts`].
//! - [`service`] and [`login`] list providers and drive login flows.
//! - [`keys`] reports where omp would read a provider's API key from, without ever
//!   reading or returning a key value.
//! - [`config`] and [`models_yml`] edit omp's `config.yml` and `models.yml`
//!   in place through [`yaml`], keeping comments and ordering of untouched keys.
//! - [`selector`] is the model and role selector grammar shared with the
//!   harness adapters.
//!
//! # API keys never pass through the app
//!
//! No function in this crate accepts, stores or returns an API key value. The only
//! way to write a provider's `apiKey` in `models.yml` is a [`models_yml::KeyReference`],
//! which can only be built from an environment variable name or a `!command`.
//! Reading reports a [`models_yml::ApiKeySetting::Literal`] with no payload when
//! the file holds an inline key, and [`keys`] reports sources by name only.

pub mod config;
pub mod error;
pub mod files;
pub mod keys;
pub mod login;
pub mod models_yml;
pub mod rpc;
pub mod selector;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod settings;
pub mod service;
pub mod yaml;
