//! Where the node's secret seed lives between runs: the [`KeyStore`] port, its
//! memory, OS keychain and file adapters, and the [`FallbackStore`] policy.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use loa_util::LockExt;
use zeroize::Zeroizing;

use crate::key::seed_of;
use crate::{NodeIdentity, Result, TrustError};

/// Secret bytes read from a [`KeyStore`]; wiped from memory when dropped.
pub type SecretBytes = Zeroizing<Vec<u8>>;

/// Storage for exactly one secret: this node's seed. Adapters store the bytes
/// verbatim and never log them.
pub trait KeyStore: Send + Sync {
    /// The stored secret, or `None` when nothing has been stored yet.
    fn load(&self) -> Result<Option<SecretBytes>>;
    /// Stores `secret`, replacing any previous value.
    fn store(&self, secret: &[u8]) -> Result<()>;
    /// Removes the secret. Removing nothing succeeds.
    fn delete(&self) -> Result<()>;
    /// The file holding the secret, for stores that have one.
    fn path(&self) -> Option<&Path> {
        None
    }
}

/// In-process store for tests and ephemeral nodes.
#[derive(Default)]
pub struct MemoryKeyStore(Mutex<Option<SecretBytes>>);

impl MemoryKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that already holds `secret`, of any length.
    pub fn with_secret(secret: &[u8]) -> Self {
        Self(Mutex::new(Some(Zeroizing::new(secret.to_vec()))))
    }
}

impl KeyStore for MemoryKeyStore {
    fn load(&self) -> Result<Option<SecretBytes>> {
        Ok(self.0.locked().clone())
    }
    fn store(&self, secret: &[u8]) -> Result<()> {
        *self.0.locked() = Some(Zeroizing::new(secret.to_vec()));
        Ok(())
    }
    fn delete(&self) -> Result<()> {
        *self.0.locked() = None;
        Ok(())
    }
}

impl fmt::Debug for MemoryKeyStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryKeyStore").field("occupied", &self.0.locked().is_some()).finish()
    }
}

/// The OS credential store (Secret Service, Keychain Services, Credential
/// Manager), one entry addressed by service and account name.
#[derive(Debug, Clone)]
pub struct OsKeychain {
    service: String,
    account: String,
}

impl OsKeychain {
    /// The entry that holds this app's node key.
    pub fn for_node() -> Self {
        Self::new("loa", "node-key")
    }

    pub fn new(service: &str, account: &str) -> Self {
        Self { service: service.to_owned(), account: account.to_owned() }
    }

    fn entry(&self) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, &self.account).map_err(keychain_error)
    }
}

impl KeyStore for OsKeychain {
    fn load(&self) -> Result<Option<SecretBytes>> {
        match self.entry()?.get_secret() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keychain_error(e)),
        }
    }
    fn store(&self, secret: &[u8]) -> Result<()> {
        self.entry()?.set_secret(secret).map_err(keychain_error)
    }
    fn delete(&self) -> Result<()> {
        match self.entry()?.delete_credential() {
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

/// A key file readable only by the user (0600 on Unix), for machines without
/// a usable keychain.
#[derive(Debug, Clone)]
pub struct FileKeyStore(PathBuf);

impl FileKeyStore {
    /// The node key file `dir/node.key`.
    pub fn in_dir(dir: &Path) -> Self {
        Self(dir.join("node.key"))
    }
}

impl KeyStore for FileKeyStore {
    fn load(&self) -> Result<Option<SecretBytes>> {
        let bytes = loa_util::fs::read_optional(&self.0).map_err(|e| self.error(&e))?;
        Ok(bytes.map(Zeroizing::new))
    }
    fn store(&self, secret: &[u8]) -> Result<()> {
        loa_util::fs::write_atomic(&self.0, secret, Some(0o600)).map_err(|e| self.error(&e))
    }
    fn delete(&self) -> Result<()> {
        match std::fs::remove_file(&self.0) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(self.error(&e)),
            _ => Ok(()),
        }
    }
    fn path(&self) -> Option<&Path> {
        Some(&self.0)
    }
}

impl FileKeyStore {
    fn error(&self, e: &io::Error) -> TrustError {
        TrustError::Store(format!("{}: {e}", self.0.display()))
    }
}

/// Where the loaded node key lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    Keychain,
    /// The key file, because the keychain could not be used; carries the reason.
    File { path: PathBuf, keychain_error: String },
    /// Memory only: neither store works, or the key was handed in.
    Memory { reason: String },
}

/// Decorator for the migration policy: uses `primary` unless it is
/// [`TrustError::Unavailable`], then `fallback`. A key left in `fallback` by an
/// earlier run moves into `primary` once that works, so the address stays.
/// Other primary errors (a malformed value) never switch stores.
pub struct FallbackStore<P, F> {
    primary: P,
    fallback: F,
    /// Why `primary` was skipped, once it was.
    fell_back: Mutex<Option<String>>,
}

impl<P: KeyStore, F: KeyStore> FallbackStore<P, F> {
    pub fn new(primary: P, fallback: F) -> Self {
        Self { primary, fallback, fell_back: Mutex::new(None) }
    }

    /// Which store the last load used.
    pub fn source(&self) -> KeySource {
        match self.fell_back.locked().clone() {
            None => KeySource::Keychain,
            Some(keychain_error) => {
                KeySource::File { path: self.fallback.path().map(Path::to_owned).unwrap_or_default(), keychain_error }
            }
        }
    }

    fn active(&self) -> &dyn KeyStore {
        if self.fell_back.locked().is_some() { &self.fallback } else { &self.primary }
    }
}

impl<P: KeyStore, F: KeyStore> KeyStore for FallbackStore<P, F> {
    fn load(&self) -> Result<Option<SecretBytes>> {
        match self.primary.load() {
            Ok(Some(secret)) => Ok(Some(secret)),
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
            Err(e) => Err(e),
        }
    }
    fn store(&self, secret: &[u8]) -> Result<()> {
        self.active().store(secret)
    }
    fn delete(&self) -> Result<()> {
        self.active().delete()
    }
}

/// The app's node key: the OS keychain entry, else `<data_dir>/node.key`.
pub fn load_app_identity(data_dir: &Path) -> Result<(NodeIdentity, KeySource)> {
    let store = FallbackStore::new(OsKeychain::for_node(), FileKeyStore::in_dir(data_dir));
    let identity = NodeIdentity::load_or_create(&store)?;
    Ok((identity, store.source()))
}
