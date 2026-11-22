//! omp model settings, the files that hold them and the ports that reach omp.
//!
//! - [`rpc`] declares the omp RPC port: the commands model settings need
//!   (`get_login_providers`, `login`, `get_available_models`, `set_model`)
//!   and the login UI frames omp sends while a login runs.
//! - [`login::OmpProviderAccount`] and [`files::OmpFilesResolver`] are the
//!   ports for omp's provider logins and its active agent directory;
//!   [`error::SettingsValidator`] checks edited files.
//! - [`keys`] reports where omp would read a provider's API key from, without ever
//!   reading or returning a key value.
//! - [`config`] and [`models_yml`] edit omp's `config.yml` and `models.yml`
//!   in place through [`yaml`], keeping comments and ordering of untouched keys.
//! - [`selector`] is the model and role selector grammar shared with the
//!   harness adapters.
//! - [`settings`] is the model settings screen's view of both files.
//! - [`agents`] lists, validates and writes omp agent definition Markdown files.
//!
//! # API keys never pass through the app
//!
//! No function here accepts, stores or returns an API key value. The only
//! way to write a provider's `apiKey` in `models.yml` is a [`models_yml::KeyReference`],
//! which can only be built from an environment variable name or a `!command`.
//! Reading reports a [`models_yml::ApiKeySetting::Literal`] with no payload when
//! the file holds an inline key, and [`keys`] reports sources by name only.

pub mod agents;
pub mod config;
pub mod error;
pub mod files;
pub mod keys;
pub mod login;
pub mod models_yml;
pub mod rpc;
pub mod selector;
pub mod settings;
pub mod workflows;
pub mod yaml;
