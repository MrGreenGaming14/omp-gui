//! Where the node's seed lives between runs: the [`KeyStore`] port, memory,
//! OS keychain and file adapters, and the [`FallbackStore`] migration policy.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use loa_util::LockExt;
use zeroize::Zeroizing;

use crate::key::seed_of;
use crate::{NodeIdentity, Result, TrustError};

/// Secret bytes read from a [`KeyStore`]; wiped from memory when dropped.
pub type SecretBytes = Zeroizing<Vec<u8>>;

/// Storage for this node's seed. Adapters store it verbatim and never log it.
pub trait KeyStore: Send + Sync {
    /// The stored secret, `None` before the first store.
    fn load(&self) -> Result<Option<SecretBytes>>;
    fn store(&self, secret: &[u8]) -> Result<()>;
    /// Removes the secret; removing nothing succeeds.
    fn delete(&self) -> Result<()>;
    /// The file holding the secret, for stores that have one.
    fn path(&self) -> Option<&Path> { None }
}

/// In-process store for tests and ephemeral nodes.
#[derive(Default)]
pub struct MemoryKeyStore(Mutex<Option<SecretBytes>>);

impl MemoryKeyStore {
    pub fn new() -> Self { Self::default() }
    /// A store already holding `secret`, of any length.
    pub fn with_secret(secret: &[u8]) -> Self { Self(Mutex::new(Some(Zeroizing::new(secret.to_vec())))) }
}

impl KeyStore for MemoryKeyStore {
    fn load(&self) -> Result<Option<SecretBytes>> { Ok(self.0.locked().clone()) }
    fn store(&self, secret: &[u8]) -> Result<()> {
        *self.0.locked() = Some(Zeroizing::new(secret.to_vec()));
        Ok(())
    }
    fn delete(&self) -> Result<()> {
        *self.0.locked() = None;
        Ok(())
    }
}

/// The OS credential store (Secret Service, Keychain Services, Credential
/// Manager): the app's `loa`/`node-key` entry.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsKeychain;

impl OsKeychain { fn entry() -> Result<keyring::Entry> { keyring::Entry::new("loa", "node-key").map_err(keychain_error) } }

impl KeyStore for OsKeychain {
    fn load(&self) -> Result<Option<SecretBytes>> {
        match Self::entry()?.get_secret() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keychain_error(e)),
        }
    }
    fn store(&self, secret: &[u8]) -> Result<()> { Self::entry()?.set_secret(secret).map_err(keychain_error) }
    fn delete(&self) -> Result<()> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keychain_error(e)),
        }
    }
}

/// Keeps only the error's text: some variants carry the raw stored bytes.
fn keychain_error(e: keyring::Error) -> TrustError {
    match e {
        keyring::Error::NoDefaultStore => TrustError::Unavailable("no platform credential store".into()),
        keyring::Error::NoStorageAccess(_) | keyring::Error::PlatformFailure(_) => TrustError::Unavailable(e.to_string()),
        e => TrustError::Store(e.to_string()),
    }
}

/// `node.key` in the data directory, readable only by the user (0600 on Unix).
#[derive(Debug, Clone)]
pub struct FileKeyStore(PathBuf);

impl FileKeyStore {
    pub fn in_dir(dir: &Path) -> Self { Self(dir.join("node.key")) }
    fn error(&self, e: &io::Error) -> TrustError { TrustError::Store(format!("{}: {e}", self.0.display())) }
}

impl KeyStore for FileKeyStore {
    fn load(&self) -> Result<Option<SecretBytes>> { Ok(loa_util::fs::read_optional(&self.0).map_err(|e| self.error(&e))?.map(Zeroizing::new)) }
    fn store(&self, secret: &[u8]) -> Result<()> { loa_util::fs::write_atomic(&self.0, secret, Some(0o600)).map_err(|e| self.error(&e)) }
    fn delete(&self) -> Result<()> {
        match std::fs::remove_file(&self.0) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(self.error(&e)),
            _ => Ok(()),
        }
    }
    fn path(&self) -> Option<&Path> { Some(&self.0) }
}

/// Where the loaded node key lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    Keychain,
    /// The key file, because the keychain could not be used.
    File { path: PathBuf, keychain_error: String },
    /// Memory only: neither store works, or the key was handed in.
    Memory { reason: String },
}

/// The migration policy as a decorator: `primary` unless it is
/// [`TrustError::Unavailable`], then `fallback`. A key left in `fallback` moves
/// into `primary` once that works, so the address stays. Other primary errors
/// (a malformed value) never switch stores.
pub struct FallbackStore<P, F> {
    primary: P,
    fallback: F,
    /// Why `primary` was skipped, once it was.
    fell_back: Mutex<Option<String>>,
}

impl<P: KeyStore, F: KeyStore> FallbackStore<P, F> {
    pub fn new(primary: P, fallback: F) -> Self { Self { primary, fallback, fell_back: Mutex::new(None) } }
    /// Which store the last load used.
    pub fn source(&self) -> KeySource {
        match self.fell_back.locked().clone() {
            None => KeySource::Keychain,
            Some(keychain_error) => KeySource::File { path: self.fallback.path().map(Path::to_owned).unwrap_or_default(), keychain_error },
        }
    }

    fn active(&self) -> &dyn KeyStore { if self.fell_back.locked().is_some() { &self.fallback } else { &self.primary } }
}

impl<P: KeyStore, F: KeyStore> KeyStore for FallbackStore<P, F> {
    fn load(&self) -> Result<Option<SecretBytes>> {
        match self.primary.load() {
            Ok(None) => {
                let Some(secret) = self.fallback.load()? else { return Ok(None) };
                self.primary.store(&seed_of(&secret)?)?;
                self.fallback.delete()?;
                Ok(Some(secret))
            }
            Err(TrustError::Unavailable(reason)) => {
                *self.fell_back.locked() = Some(reason);
                self.fallback.load()
            }
            other => other,
        }
    }
    fn store(&self, secret: &[u8]) -> Result<()> { self.active().store(secret) }
    fn delete(&self) -> Result<()> { self.active().delete() }
}

/// The app's node key: the OS keychain, else `<data_dir>/node.key`.
pub fn load_app_identity(data_dir: &Path) -> Result<(NodeIdentity, KeySource)> {
    let store = FallbackStore::new(OsKeychain, FileKeyStore::in_dir(data_dir));
    let identity = NodeIdentity::load_or_create(&store)?;
    Ok((identity, store.source()))
}
