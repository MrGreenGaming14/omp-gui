//! Central app config with a schema version and migrations.
//!
//! The app owns one TOML file (see [`config_path`]). [`ConfigStore`] loads it,
//! runs the [`Migrator`] chain when its `version` is older than the app's,
//! validates it into a typed [`Config`] and saves it atomically. Keys the app
//! doesn't know are kept on load and written back on save, so a config written
//! by a newer app with additive keys survives being edited by an older one.
//! Adapters translate the central config into a harness's native config through
//! [`NativeConfigWriter`].

mod codec;
mod migrate;
mod native;
mod schema;
mod store;

pub use migrate::{Migrator, Step, StepFn};
pub use native::{NativeConfig, NativeConfigError, NativeConfigWriter};
pub use schema::{
    CURRENT_VERSION, Config, DEFAULT_PORT, DecisionDefault, Discovery, HarnessSettings, McpServer,
    NetworkSettings, ProjectSettings, Relay, RemoteHost, Theme, UnattendedSettings,
    UpdaterSettings,
};
pub use store::{CONFIG_FILE_NAME, ConfigStore, Loaded, config_path};

use std::path::PathBuf;

/// Everything that can go wrong loading, migrating, validating or saving the config.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("can't read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("can't write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the config isn't valid TOML: {0}")]
    Parse(#[source] Box<toml::de::Error>),
    #[error("the config has no top level `version` key")]
    MissingVersion,
    /// A key holds a value the schema doesn't accept. `key` is the dotted path.
    #[error("`{key}` {reason}")]
    Invalid { key: String, reason: String },
    /// The file was written by a newer app. It is never downgraded.
    #[error(
        "the config is schema version {found} but this app understands up to version {supported}; update the app to use it"
    )]
    TooNew { found: u32, supported: u32 },
    #[error("there is no migration from config version {from} to {}", from + 1)]
    MissingMigration { from: u32 },
    #[error("migrating the config from version {from} to {}: {reason}", from + 1)]
    Migration { from: u32, reason: String },
    #[error("can't serialize the config: {0}")]
    Serialize(#[from] toml::ser::Error),
}

pub type Result<T, E = ConfigError> = std::result::Result<T, E>;

pub(crate) fn invalid(key: impl Into<String>, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid { key: key.into(), reason: reason.into() }
}
