//! Where the node's secret seed lives between runs.

use std::fmt;
use std::sync::Mutex;

use zeroize::Zeroizing;

use crate::{IdentityError, Result};

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
        self.secret.lock().unwrap_or_else(|e| e.into_inner())
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
    IdentityError::Store(e.to_string())
}
