//! The node keypair, its public key and the holder proof.

use std::fmt;
use std::str::FromStr;

use biscuit_auth::{Algorithm, KeyPair, PrivateKey};
use data_encoding::{BASE32_NOPAD, HEXLOWER, HEXLOWER_PERMISSIVE};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use zeroize::Zeroizing;

use crate::keystore::KeyStore;
use crate::{Result, TrustError};

/// Length of the secret seed, which is all that is stored.
pub const SECRET_KEY_LENGTH: usize = 32;

/// This node's ed25519 keypair: its network address and its signing key.
/// The secret is wiped from memory on drop and never printed.
#[derive(Clone)]
pub struct NodeIdentity {
    signing: SigningKey,
}

impl NodeIdentity {
    /// Loads the key from `store`, generating and storing one on first run. A
    /// stored value of the wrong length is an error and is never replaced.
    pub fn load_or_create(store: &dyn KeyStore) -> Result<Self> {
        if let Some(secret) = store.load()? {
            return Ok(Self::from_secret_bytes(&seed_of(&secret)?));
        }
        let identity = Self::generate()?;
        store.store(identity.secret_bytes())?;
        Ok(identity)
    }

    /// A fresh key from the operating system random source, not stored anywhere.
    pub fn generate() -> Result<Self> {
        let mut seed = Zeroizing::new([0u8; SECRET_KEY_LENGTH]);
        getrandom::fill(seed.as_mut_slice()).map_err(|e| TrustError::Random(e.to_string()))?;
        Ok(Self::from_secret_bytes(&seed))
    }

    pub fn from_secret_bytes(seed: &[u8; SECRET_KEY_LENGTH]) -> Self {
        Self { signing: SigningKey::from_bytes(seed) }
    }

    /// The 32 byte seed; the network layer builds its iroh secret key from it.
    pub fn secret_bytes(&self) -> &[u8; SECRET_KEY_LENGTH] {
        self.signing.as_bytes()
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.signing.verifying_key().to_bytes())
    }

    /// Signs `message` (pure Ed25519, RFC 8032).
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing.sign(message).to_bytes()
    }

    /// The same key as Biscuit signs with.
    pub(crate) fn biscuit(&self) -> Result<KeyPair> {
        let private = PrivateKey::from_bytes(self.secret_bytes(), Algorithm::Ed25519).map_err(|e| TrustError::Key(e.to_string()))?;
        Ok(KeyPair::from(&private))
    }
}

/// The 32 byte seed in `secret`, or [`TrustError::WrongLength`].
pub(crate) fn seed_of(secret: &[u8]) -> Result<[u8; SECRET_KEY_LENGTH]> {
    secret.try_into().map_err(|_| TrustError::WrongLength { found: secret.len() })
}

impl fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeIdentity").field("public_key", &self.public_key()).finish_non_exhaustive()
    }
}

/// A node's ed25519 public key: its endpoint id and grant identity.
///
/// `Display` prints the iroh endpoint id form (64 lowercase hex characters);
/// `FromStr` also takes iroh's 52 character base32 form.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey(pub [u8; 32]);

impl PublicKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn verifying(&self) -> Option<VerifyingKey> {
        VerifyingKey::from_bytes(&self.0).ok()
    }

    /// True when `signature` was made over `message` by this key's secret.
    /// Strict: weak keys and malleable signatures fail, as in Biscuit.
    pub fn verify(&self, message: &[u8], signature: &[u8; 64]) -> bool {
        let signature = ed25519_dalek::Signature::from_bytes(signature);
        self.verifying().is_some_and(|key| key.verify_strict(message, &signature).is_ok())
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&HEXLOWER.encode(&self.0))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({self})")
    }
}

impl FromStr for PublicKey {
    type Err = TrustError;

    /// Parses an endpoint id; fails unless the bytes are a curve point.
    fn from_str(text: &str) -> Result<Self> {
        let key = match text.len() {
            64 => Self::from_hex(text)?,
            _ => Self::decode(&BASE32_NOPAD, &text.to_ascii_uppercase())?,
        };
        key.verifying().map(|_| key).ok_or_else(|| TrustError::Key("not an ed25519 point".into()))
    }
}

impl PublicKey {
    /// The key from 64 hex characters, as stored and as [`Display`](fmt::Display) prints it.
    pub fn from_hex(text: &str) -> Result<Self> {
        Self::decode(&HEXLOWER_PERMISSIVE, text)
    }

    fn decode(encoding: &data_encoding::Encoding, text: &str) -> Result<Self> {
        let bytes = encoding.decode(text.as_bytes()).ok().and_then(|b| b.try_into().ok());
        bytes.map(Self).ok_or_else(|| TrustError::Key(format!("not a public key: {text:?}")))
    }
}

impl From<iroh_base::PublicKey> for PublicKey {
    fn from(key: iroh_base::PublicKey) -> Self {
        Self(*key.as_bytes())
    }
}

impl TryFrom<PublicKey> for iroh_base::PublicKey {
    type Error = TrustError;
    fn try_from(key: PublicKey) -> Result<Self> {
        Self::from_bytes(&key.0).map_err(|e| TrustError::Key(e.to_string()))
    }
}

impl From<loa_bus::NodeKey> for PublicKey {
    fn from(key: loa_bus::NodeKey) -> Self {
        Self(key.0)
    }
}

impl From<PublicKey> for loa_bus::NodeKey {
    fn from(key: PublicKey) -> Self {
        Self(key.0)
    }
}

impl TryFrom<PublicKey> for biscuit_auth::PublicKey {
    type Error = TrustError;
    fn try_from(key: PublicKey) -> Result<Self> {
        Self::from_bytes(&key.0, Algorithm::Ed25519).map_err(|e| TrustError::Key(e.to_string()))
    }
}

impl TryFrom<&biscuit_auth::PublicKey> for PublicKey {
    type Error = TrustError;
    fn try_from(key: &biscuit_auth::PublicKey) -> Result<Self> {
        key.to_bytes().try_into().map(Self).map_err(|_| TrustError::Key("not an ed25519 key".into()))
    }
}

/// A random nonce the verifier sends the holder before accepting a token.
/// Fresh per connection, so a recorded proof can't be replayed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Challenge(pub [u8; 32]);

impl Challenge {
    pub fn random() -> Result<Self> {
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce).map_err(|e| TrustError::Random(e.to_string()))?;
        Ok(Self(nonce))
    }
}

/// The holder's signature over a challenge and the token it presents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof(pub [u8; 64]);

const PROOF_DOMAIN: &[u8] = b"loa-grant-proof/1\0";

fn proof_message(challenge: &Challenge, token: &[u8]) -> Vec<u8> {
    [PROOF_DOMAIN, &challenge.0, token].concat()
}

/// Signs `challenge` and `token` with the holder's key. The proof binds both,
/// so it can't be reused for another token or another connection.
pub fn prove(holder: &NodeIdentity, challenge: &Challenge, token: &[u8]) -> Proof {
    Proof(holder.sign(&proof_message(challenge, token)))
}

pub(crate) fn proof_valid(holder: &PublicKey, challenge: &Challenge, token: &[u8], proof: &Proof) -> bool {
    holder.verify(&proof_message(challenge, token), &proof.0)
}
