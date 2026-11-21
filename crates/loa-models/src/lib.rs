//! Sign in, model listing and model settings through omp and other harnesses.
//!
//! The app has no provider code of its own. This crate is the logic behind the
//! sign in screen, the model picker and the model settings screen:
//!
//! - [`rpc`] declares the [`rpc::OmpRpc`] port: the omp RPC commands this crate
//!   needs (`get_login_providers`, `login`, `get_available_models`, `set_model`)
//!   and the login UI frames omp sends while a login runs.
//! - [`omp`] implements that port over a dedicated `omp --mode rpc --no-session`
//!   process ([`omp::OmpService`]) and locates omp's settings files
//!   ([`omp::OmpFiles`]); [`opener`] opens login URLs in the browser.
//! - [`providers`] and [`login`] list providers with their signed in state and
//!   drive the OAuth login flow, opening the browser through [`login::UrlOpener`].
//! - [`keys`] reports where omp would read a provider's API key from, without ever
//!   reading or returning a key value.
//! - [`catalog`] groups available models by provider with their role assignments
//!   and switches the session model or a role's model.
//! - [`yaml`], [`config`] and [`models_yml`] edit omp's own `config.yml` and
//!   `models.yml` in place, keeping comments and ordering of untouched keys.
//! - [`validate`] checks edited files with omp itself, without a model call.
//! - [`process`] and [`harness`] handle sign in and model settings for the other
//!   harnesses (Claude Code and Codex) through their own CLIs and config files.
//!
//! # API keys never pass through the app
//!
//! No function in this crate accepts, stores or returns an API key value. The only
//! way to write a provider's `apiKey` in `models.yml` is a [`models_yml::KeyReference`],
//! which can only be built from an environment variable name or a `!command`.
//! Reading reports a [`models_yml::ApiKeySetting::Literal`] with no payload when
//! the file holds an inline key, and [`keys`] reports sources by name only.

pub mod catalog;
pub mod config;
mod error;
pub mod harness;
pub mod keys;
pub mod login;
pub mod models_yml;
pub mod omp;
pub mod opener;
pub mod process;
pub mod providers;
pub mod rpc;
pub mod validate;
pub mod yaml;

pub use error::{Error, Result};
