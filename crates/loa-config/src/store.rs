//! Loading and saving the config file.

use crate::codec::{decode, encode};
use crate::{Config, ConfigError, Migrator, Result};
use std::fs;
use std::io::{self, Write};
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
    /// The version the file had when it was migrated on this load.
    pub migrated_from: Option<u32>,
    /// Copy of the file as it was before migration.
    pub backup: Option<PathBuf>,
}

/// Reads and writes one config file.
#[derive(Clone, Debug)]
pub struct ConfigStore {
    path: PathBuf,
    migrator: Migrator,
}

impl ConfigStore {
    /// A store for the file at `path`, migrating with [`Migrator::standard`].
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), migrator: Migrator::standard() }
    }

    /// A store for [`config_path`] in `config_dir`.
    pub fn in_dir(config_dir: &Path) -> Self {
        Self::new(config_path(config_dir))
    }

    /// Replaces the migration chain.
    #[must_use]
    pub fn with_migrator(mut self, migrator: Migrator) -> Self {
        self.migrator = migrator;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the file, or the defaults when it doesn't exist yet.
    ///
    /// An older file is migrated in memory and validated first. Only then is
    /// the original copied to a backup next to it and the migrated config
    /// written over it, so a failed migration leaves the file untouched.
    pub fn load(&self) -> Result<Loaded> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let config = Config { version: self.migrator.target(), ..Config::default() };
                return Ok(Loaded { config, migrated_from: None, backup: None });
            }
            Err(source) => return Err(ConfigError::Read { path: self.path.clone(), source }),
        };
        let mut doc: Table = text.parse().map_err(|e| ConfigError::Parse(Box::new(e)))?;
        let migrated_from = self.migrator.migrate(&mut doc)?;
        let config = decode(doc, self.migrator.target())?;
        let backup = match migrated_from {
            Some(from) => {
                let backup = self.write_backup(from, text.as_bytes())?;
                self.write_atomic(encode(&config)?.as_bytes())?;
                Some(backup)
            }
            None => None,
        };
        Ok(Loaded { config, migrated_from, backup })
    }

    /// Validates `config` and replaces the file with it atomically: the new
    /// contents are written and synced to a temporary file in the same
    /// directory, then renamed over the old one.
    pub fn save(&self, config: &Config) -> Result<()> {
        config.validate()?;
        self.write_atomic(encode(config)?.as_bytes())
    }

    fn write_atomic(&self, bytes: &[u8]) -> Result<()> {
        let write_err = |source| ConfigError::Write { path: self.path.clone(), source };
        let dir = match self.path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        fs::create_dir_all(dir).map_err(write_err)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(write_err)?;
        tmp.write_all(bytes).map_err(write_err)?;
        tmp.as_file().sync_all().map_err(write_err)?;
        tmp.persist(&self.path).map_err(|e| write_err(e.error))?;
        sync_dir(dir).map_err(write_err)
    }

    /// Writes `original` to `<file>.v<from>.bak`, adding `.1`, `.2`, … when an
    /// earlier backup of the same version exists. Backups are never overwritten.
    fn write_backup(&self, from: u32, original: &[u8]) -> Result<PathBuf> {
        let base = {
            let mut name = self.path.file_name().unwrap_or_default().to_os_string();
            name.push(format!(".v{from}.bak"));
            self.path.with_file_name(name)
        };
        let mut n = 0u32;
        loop {
            let candidate = if n == 0 {
                base.clone()
            } else {
                let mut name = base.clone().into_os_string();
                name.push(format!(".{n}"));
                PathBuf::from(name)
            };
            match fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
                Ok(mut file) => {
                    let write_err = |source| ConfigError::Write { path: candidate.clone(), source };
                    file.write_all(original).map_err(write_err)?;
                    file.sync_all().map_err(write_err)?;
                    return Ok(candidate);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
                Err(source) => return Err(ConfigError::Write { path: candidate, source }),
            }
        }
    }
}

/// Makes a rename in `dir` durable.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

/// Directories can't be opened for syncing on this platform; the rename is
/// as durable as the OS makes it.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}
