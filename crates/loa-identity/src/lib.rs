//! Node identity: one ed25519 keypair in the OS keychain.
//!
//! The same key is the node's network address (the iroh endpoint id) and its
//! signing key (the Biscuit grant root and holder key). [`NodeIdentity`] owns the
//! secret, [`PublicKey`] is what peers see, and [`KeyStore`] is where the secret
//! seed lives between runs.

mod key;
mod store;

pub use key::{NodeIdentity, PublicKey, SECRET_KEY_LENGTH, Signature};
pub use store::{KeyStore, MemoryKeyStore, OsKeychain, SecretBytes};

/// Everything that can go wrong loading, storing, parsing or verifying a node key.
///
/// No variant carries secret bytes, so errors are safe to log.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// The key store could not be read or written.
    #[error("key store: {0}")]
    Store(String),
    /// The key store holds a value that is not a 32 byte ed25519 seed. It is left
    /// untouched so the user can recover it; a new key would change the node's address.
    #[error("stored node key has {found} bytes, expected {SECRET_KEY_LENGTH}")]
    WrongLength {
        /// Length of the stored value.
        found: usize,
    },
    /// The operating system random source failed while generating a key.
    #[error("os random source: {0}")]
    Random(getrandom::Error),
    /// Text is not a public key in the iroh endpoint id or Biscuit form.
    #[error("invalid public key text: {0}")]
    ParseKey(&'static str),
    /// 32 bytes that do not encode a point on the ed25519 curve.
    #[error("bytes are not a valid ed25519 public key")]
    InvalidPublicKey,
    /// A signature did not verify under the given public key and message.
    #[error("signature does not verify")]
    BadSignature,
}

/// Result type of this crate.
pub type Result<T, E = IdentityError> = std::result::Result<T, E>;
