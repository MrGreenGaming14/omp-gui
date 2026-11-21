//! Loading, editing and saving the config file.

use super::codec::{decode, encode, version_of};
use super::{CURRENT_VERSION, Config, ConfigError, Result};
use std::path::{Path, PathBuf};
use std::{fs, io, sync::Arc};
use tokio::sync::watch;

/// File name of the config inside the app's config directory.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Where the config file lives inside `config_dir`; callers pick the directory.
pub fn config_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CONFIG_FILE_NAME)
}

/// Reads and writes one config file. Clones share the save counter.
#[derive(Clone, Debug)]
pub struct ConfigStore {
    path: PathBuf,
    saves: Arc<watch::Sender<u64>>,
}

impl ConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), saves: Arc::new(watch::Sender::new(0)) }
    }

    /// A store for [`config_path`] in `config_dir`.
    pub fn in_dir(config_dir: &Path) -> Self {
        Self::new(config_path(config_dir))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the file, or the defaults when it doesn't exist yet. A file of
    /// another schema version is refused ([`ConfigError::TooNew`] or
    /// [`ConfigError::MissingMigration`]; no migrations exist yet).
    pub fn load(&self) -> Result<Config> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(source) => return Err(ConfigError::Read { path: self.path.clone(), source }),
        };
        let doc: toml::Table = text.parse().map_err(|e| ConfigError::Parse(Box::new(e)))?;
        match version_of(&doc)? {
            found if found > CURRENT_VERSION => Err(ConfigError::TooNew { found, supported: CURRENT_VERSION }),
            from if from < CURRENT_VERSION => Err(ConfigError::MissingMigration { from }),
            _ => decode(doc),
        }
    }

    /// Validates `config` and replaces the file with it atomically.
    pub fn save(&self, config: &Config) -> Result<()> {
        config.validate()?;
        loa_util::fs::write_atomic(&self.path, encode(config)?.as_bytes(), None)
            .map_err(|source| ConfigError::Write { path: self.path.clone(), source })?;
        self.saves.send_modify(|n| *n += 1);
        Ok(())
    }

    /// Loads the file, lets `change` edit it and saves it when anything
    /// changed, so keys other callers wrote meanwhile survive.
    pub fn edit<R>(&self, change: impl FnOnce(&mut Config) -> R) -> Result<R> {
        let mut config = self.load()?;
        let before = config.clone();
        let out = change(&mut config);
        if config != before {
            self.save(&config)?;
        }
        Ok(out)
    }

    /// Counts the saves made through this store and its clones.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.saves.subscribe()
    }
}
