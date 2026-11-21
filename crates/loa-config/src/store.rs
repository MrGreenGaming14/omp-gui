//! Loading and saving the config file.

use crate::codec::{decode, encode, version_of};
use crate::{CURRENT_VERSION, Config, ConfigError, Result};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use toml::Table;

/// File name of the config inside the app's config directory.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Where the config file lives inside `config_dir`. The only place the
/// file's location is decided; callers pick the directory.
pub fn config_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CONFIG_FILE_NAME)
}

/// The result of [`ConfigStore::load`].
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    pub config: Config,
}

/// Reads and writes one config file.
#[derive(Clone, Debug)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// A store for [`config_path`] in `config_dir`.
    pub fn in_dir(config_dir: &Path) -> Self {
        Self::new(config_path(config_dir))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the file, or the defaults when it doesn't exist yet. A file of
    /// another schema version is refused: newer as [`ConfigError::TooNew`],
    /// older as [`ConfigError::MissingMigration`] (no migrations exist yet).
    pub fn load(&self) -> Result<Loaded> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Loaded { config: Config::default() }),
            Err(source) => return Err(ConfigError::Read { path: self.path.clone(), source }),
        };
        let doc: Table = text.parse().map_err(|e| ConfigError::Parse(Box::new(e)))?;
        match version_of(&doc)? {
            found if found > CURRENT_VERSION => Err(ConfigError::TooNew { found, supported: CURRENT_VERSION }),
            from if from < CURRENT_VERSION => Err(ConfigError::MissingMigration { from }),
            _ => Ok(Loaded { config: decode(doc)? }),
        }
    }

    /// Validates `config` and replaces the file with it atomically: the new
    /// contents are written and synced to a temporary file in the same
    /// directory, then renamed over the old one.
    pub fn save(&self, config: &Config) -> Result<()> {
        config.validate()?;
        self.write_atomic(encode(config)?.as_bytes())
    }

    fn write_atomic(&self, bytes: &[u8]) -> Result<()> {
        loa_util::fs::write_atomic(&self.path, bytes, None).map_err(|source| ConfigError::Write { path: self.path.clone(), source })
    }
}
