//! Where the node's secret seed lives between runs.

use loa_util::LockExt;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use zeroize::Zeroizing;

use crate::{IdentityError, NodeIdentity, Result, SECRET_KEY_LENGTH};

/// Secret bytes read from a [`KeyStore`]; wiped from memory when dropped.
pub type SecretBytes = Zeroizing<Vec<u8>>;

/// Storage for exactly one secret: this node's ed25519 seed.
///
/// Implementations store the bytes verbatim and must not log them.
pub trait KeyStore: Send + Sync {
    /// Returns the stored secret, or `None` when nothing has been stored yet.
    fn load(&self) -> Result<Option<SecretBytes>>;
    /// Stores `secret`, replacing any previous value.
    fn store(&self, secret: &[u8]) -> Result<()>;
}

/// In-process key store for tests and ephemeral nodes. Nothing touches disk.
#[derive(Default)]
pub struct MemoryKeyStore {
    secret: Mutex<Option<SecretBytes>>,
}

impl MemoryKeyStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that already holds `secret`, of any length.
    pub fn with_secret(secret: &[u8]) -> Self {
        Self {
            secret: Mutex::new(Some(Zeroizing::new(secret.to_vec()))),
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<SecretBytes>> {
        // A poisoned lock only means another thread panicked mid-access; the
        // slot is a plain value that is always in a consistent state.
        self.secret.locked()
    }
}

impl KeyStore for MemoryKeyStore {
    fn load(&self) -> Result<Option<SecretBytes>> {
        Ok(self.slot().clone())
    }

    fn store(&self, secret: &[u8]) -> Result<()> {
        *self.slot() = Some(Zeroizing::new(secret.to_vec()));
        Ok(())
    }
}

impl fmt::Debug for MemoryKeyStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryKeyStore")
            .field("occupied", &self.slot().is_some())
            .finish()
    }
}

/// The OS credential store: Secret Service on Linux and BSD, Keychain Services on
/// macOS, Credential Manager on Windows. One entry is addressed by service and
/// account name.
pub struct OsKeychain {
    service: String,
    account: String,
    entry: keyring::Entry,
}

impl OsKeychain {
    /// Service name of the app's node key entry.
    pub const SERVICE: &str = "loa";
    /// Account name of the app's node key entry.
    pub const ACCOUNT: &str = "node-key";

    /// The entry that holds this app's node key. This is the one constructor
    /// that reaches the user's real keychain under the app's fixed names.
    pub fn for_node() -> Result<Self> {
        Self::new(Self::SERVICE, Self::ACCOUNT)
    }

    /// An entry under an explicit service and account name.
    ///
    /// The first call selects the platform credential store for the process.
    pub fn new(service: &str, account: &str) -> Result<Self> {
        let entry = keyring::Entry::new(service, account).map_err(store_error)?;
        Ok(Self {
            service: service.to_owned(),
            account: account.to_owned(),
            entry,
        })
    }

    /// Removes the entry. Removing an entry that does not exist succeeds.
    pub fn delete(&self) -> Result<()> {
        match self.entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(store_error(e)),
        }
    }
}

impl KeyStore for OsKeychain {
    fn load(&self) -> Result<Option<SecretBytes>> {
        match self.entry.get_secret() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(store_error(e)),
        }
    }

    fn store(&self, secret: &[u8]) -> Result<()> {
        self.entry.set_secret(secret).map_err(store_error)
    }
}

impl fmt::Debug for OsKeychain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsKeychain")
            .field("service", &self.service)
            .field("account", &self.account)
            .finish()
    }
}

/// Keeps only the keychain error's `Display` text: some variants carry the raw
/// stored bytes, which must not reach logs through `Debug`.
fn store_error(e: keyring::Error) -> IdentityError {
    match e {
        keyring::Error::NoDefaultStore => IdentityError::Unavailable("no platform credential store".into()),
        keyring::Error::NoStorageAccess(_) | keyring::Error::PlatformFailure(_) => {
            IdentityError::Unavailable(e.to_string())
        }
        e => IdentityError::Store(e.to_string()),
    }
}

/// A key file under the app's data directory, readable only by the user (mode
/// 0600 on Unix). The fallback for machines without a usable OS keychain:
/// headless sessions, containers, or a locked credential store.
#[derive(Debug, Clone)]
pub struct FileKeyStore {
    path: PathBuf,
}

impl FileKeyStore {
    /// File name of the node key inside the data directory.
    pub const FILE_NAME: &str = "node.key";

    /// The node key file `dir/node.key`.
    pub fn in_dir(dir: &Path) -> Self {
        Self { path: dir.join(Self::FILE_NAME) }
    }

    /// A key file at an explicit path.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Removes the key file. Removing a missing file succeeds.
    pub fn delete(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(file_error(&self.path, &e)),
        }
    }
}

impl KeyStore for FileKeyStore {
    fn load(&self) -> Result<Option<SecretBytes>> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(file_error(&self.path, &e)),
        }
    }

    fn store(&self, secret: &[u8]) -> Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir).map_err(|e| file_error(dir, &e))?;
        // Written to a private temporary file and renamed into place, so a
        // crash never leaves a truncated key behind.
        let tmp = self.path.with_extension("key.tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp).map_err(|e| file_error(&tmp, &e))?;
        io::Write::write_all(&mut file, secret).map_err(|e| file_error(&tmp, &e))?;
        file.sync_all().map_err(|e| file_error(&tmp, &e))?;
        drop(file);
        fs::rename(&tmp, &self.path).map_err(|e| file_error(&self.path, &e))
    }
}

fn file_error(path: &Path, e: &io::Error) -> IdentityError {
    IdentityError::Store(format!("{}: {e}", path.display()))
}

/// Where the loaded node key lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// The OS credential store.
    Keychain,
    /// The key file, because the keychain could not be used; carries the reason.
    File { path: PathBuf, keychain_error: String },
    /// Held in memory only: neither store could be used, or the key was
    /// handed in (tests). A new key, and address, next time.
    Memory { reason: String },
}

/// Loads the node key from `keychain`, falling back to `file` only when the
/// keychain itself is unusable ([`IdentityError::Unavailable`]).
///
/// A key left in the file by an earlier fallback run moves into the keychain
/// once the keychain works, so the node keeps its address; the file is then
/// removed. A keychain that works but holds a malformed value is an error, not
/// a reason to switch stores.
pub fn load_node_identity(keychain: &dyn KeyStore, file: &FileKeyStore) -> Result<(NodeIdentity, KeySource)> {
    match keychain.load() {
        Ok(Some(_)) => Ok((NodeIdentity::load_or_create(keychain)?, KeySource::Keychain)),
        Ok(None) => {
            if let Some(secret) = file.load()? {
                let seed: &[u8; SECRET_KEY_LENGTH] =
                    secret.as_slice().try_into().map_err(|_| IdentityError::WrongLength { found: secret.len() })?;
                keychain.store(seed)?;
                file.delete()?;
                return Ok((NodeIdentity::from_secret_bytes(seed), KeySource::Keychain));
            }
            Ok((NodeIdentity::load_or_create(keychain)?, KeySource::Keychain))
        }
        Err(IdentityError::Unavailable(reason)) => from_file(file, reason),
        Err(e) => Err(e),
    }
}

/// The app's node key: the [`OsKeychain::for_node`] entry, or the key file in
/// `data_dir` when this machine has no usable keychain (see [`load_node_identity`]).
pub fn load_app_identity(data_dir: &Path) -> Result<(NodeIdentity, KeySource)> {
    load_with(OsKeychain::for_node(), &FileKeyStore::in_dir(data_dir))
}

fn load_with<K: KeyStore>(keychain: Result<K>, file: &FileKeyStore) -> Result<(NodeIdentity, KeySource)> {
    match keychain {
        Ok(keychain) => load_node_identity(&keychain, file),
        Err(IdentityError::Unavailable(reason)) => from_file(file, reason),
        Err(e) => Err(e),
    }
}

fn from_file(file: &FileKeyStore, keychain_error: String) -> Result<(NodeIdentity, KeySource)> {
    let identity = NodeIdentity::load_or_create(file)?;
    Ok((identity, KeySource::File { path: file.path().to_owned(), keychain_error }))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoKeychain;
    impl KeyStore for NoKeychain {
        fn load(&self) -> Result<Option<SecretBytes>> {
            Err(IdentityError::Unavailable("no secret service".into()))
        }
        fn store(&self, _: &[u8]) -> Result<()> {
            Err(IdentityError::Unavailable("no secret service".into()))
        }
    }

    #[test]
    fn file_store_round_trips_privately() {
        let dir = std::env::temp_dir().join(format!("loa-identity-file-{}", std::process::id()));
        let file = FileKeyStore::in_dir(&dir.join("nested"));
        assert!(file.load().unwrap().is_none());
        let created = NodeIdentity::load_or_create(&file).unwrap();
        let loaded = NodeIdentity::load_or_create(&FileKeyStore::in_dir(&dir.join("nested"))).unwrap();
        assert_eq!(created.public_key(), loaded.public_key());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(file.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unusable_keychain_falls_back_to_the_file_and_keeps_the_key() {
        let dir = std::env::temp_dir().join(format!("loa-identity-fallback-{}", std::process::id()));
        let file = FileKeyStore::in_dir(&dir);
        let (first, source) = load_node_identity(&NoKeychain, &file).unwrap();
        assert!(matches!(&source, KeySource::File { keychain_error, .. } if keychain_error.contains("secret service")));
        let (again, _) = load_node_identity(&NoKeychain, &file).unwrap();
        assert_eq!(first.public_key(), again.public_key());

        // Once a keychain works, the file key moves into it and the address stays.
        let keychain = MemoryKeyStore::new();
        let (moved, source) = load_node_identity(&keychain, &file).unwrap();
        assert_eq!(source, KeySource::Keychain);
        assert_eq!(moved.public_key(), first.public_key());
        assert!(file.load().unwrap().is_none());
        let (kept, _) = load_node_identity(&keychain, &file).unwrap();
        assert_eq!(kept.public_key(), first.public_key());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_keychain_errors_do_not_switch_stores() {
        let keychain = MemoryKeyStore::with_secret(&[1, 2, 3]);
        let dir = std::env::temp_dir().join(format!("loa-identity-bad-{}", std::process::id()));
        let file = FileKeyStore::in_dir(&dir);
        assert!(matches!(load_node_identity(&keychain, &file), Err(IdentityError::WrongLength { found: 3 })));
        assert!(file.load().unwrap().is_none());
    }

    #[test]
    fn a_keychain_that_cannot_open_uses_the_file() {
        let dir = std::env::temp_dir().join(format!("loa-identity-noopen-{}", std::process::id()));
        let file = FileKeyStore::in_dir(&dir);
        let none: Result<MemoryKeyStore> = Err(IdentityError::Unavailable("no platform credential store".into()));
        let (first, source) = load_with(none, &file).unwrap();
        assert_eq!(source, KeySource::File { path: file.path().to_owned(), keychain_error: "no platform credential store".into() });
        let again: Result<MemoryKeyStore> = Err(IdentityError::Unavailable("still none".into()));
        assert_eq!(load_with(again, &file).unwrap().0.public_key(), first.public_key());
        let broken: Result<MemoryKeyStore> = Err(IdentityError::Store("denied".into()));
        assert!(load_with(broken, &file).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
