//! The node keypair, its public key and the holder proof.

use std::fmt;
use std::str::FromStr;

use biscuit_auth::{Algorithm, KeyPair, PrivateKey};
use data_encoding::{BASE32_NOPAD, Encoding, HEXLOWER, HEXLOWER_PERMISSIVE};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use zeroize::Zeroizing;

use crate::keystore::KeyStore;
use crate::{Result, TrustError};

/// Length of the secret seed, which is all that is stored.
pub const SECRET_KEY_LENGTH: usize = 32;

/// This node's ed25519 keypair: its network address and signing key. The
/// secret is wiped on drop and never printed.
#[derive(Clone)]
pub struct NodeIdentity(SigningKey);

impl NodeIdentity {
    /// Loads the key from `store`, creating and storing one on first run. A
    /// stored value of the wrong length is an error and is never replaced.
    pub fn load_or_create(store: &dyn KeyStore) -> Result<Self> {
        if let Some(secret) = store.load()? {
            return Ok(Self::from_secret_bytes(&seed_of(&secret)?));
        }
        let identity = Self::generate()?;
        store.store(identity.secret_bytes())?;
        Ok(identity)
    }

    /// A fresh key from the OS random source, stored nowhere.
    pub fn generate() -> Result<Self> {
        let mut seed = Zeroizing::new([0u8; SECRET_KEY_LENGTH]);
        getrandom::fill(seed.as_mut_slice()).map_err(|e| TrustError::Random(e.to_string()))?;
        Ok(Self::from_secret_bytes(&seed))
    }

    pub fn from_secret_bytes(seed: &[u8; SECRET_KEY_LENGTH]) -> Self { Self(SigningKey::from_bytes(seed)) }
    /// The seed; the network layer builds its iroh secret key from it.
    pub fn secret_bytes(&self) -> &[u8; SECRET_KEY_LENGTH] { self.0.as_bytes() }
    pub fn public_key(&self) -> PublicKey { PublicKey(self.0.verifying_key().to_bytes()) }
    /// Signs `message` (pure Ed25519, RFC 8032).
    pub fn sign(&self, message: &[u8]) -> [u8; 64] { self.0.sign(message).to_bytes() }
    /// The same key as Biscuit signs with.
    pub(crate) fn biscuit(&self) -> Result<KeyPair> {
        let private = PrivateKey::from_bytes(self.secret_bytes(), Algorithm::Ed25519).map_err(key_error)?;
        Ok(KeyPair::from(&private))
    }
}

impl fmt::Debug for NodeIdentity { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "NodeIdentity({})", self.public_key()) } }

pub(crate) fn seed_of(secret: &[u8]) -> Result<[u8; SECRET_KEY_LENGTH]> {
    secret.try_into().map_err(|_| TrustError::WrongLength { found: secret.len() })
}

/// A node's ed25519 public key: its endpoint id and grant identity. Prints as
/// iroh's endpoint id (64 lowercase hex); parses that or iroh's base32 form.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey(pub [u8; 32]);

impl PublicKey {
    pub fn as_bytes(&self) -> &[u8; 32] { &self.0 }
    /// The key from 64 hex characters, as stored; no curve check.
    pub fn from_hex(text: &str) -> Result<Self> { Self::decode(&HEXLOWER_PERMISSIVE, text) }
    fn decode(encoding: &Encoding, text: &str) -> Result<Self> {
        let bytes = encoding.decode(text.as_bytes()).ok().and_then(|b| b.try_into().ok());
        bytes.map(Self).ok_or_else(|| TrustError::Key(format!("not a public key: {text:?}")))
    }

    /// True when `signature` is this key's over `message`. Strict, as in Biscuit:
    /// weak keys and malleable signatures fail.
    pub fn verify(&self, message: &[u8], signature: &[u8; 64]) -> bool {
        let signature = ed25519_dalek::Signature::from_bytes(signature);
        VerifyingKey::from_bytes(&self.0).is_ok_and(|key| key.verify_strict(message, &signature).is_ok())
    }
}

impl fmt::Display for PublicKey { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&HEXLOWER.encode(&self.0)) } }

impl fmt::Debug for PublicKey { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "PublicKey({self})") } }

impl FromStr for PublicKey {
    type Err = TrustError;

    /// An endpoint id; fails unless the bytes are a curve point.
    fn from_str(text: &str) -> Result<Self> {
        let key = if text.len() == 64 { Self::from_hex(text)? } else { Self::decode(&BASE32_NOPAD, &text.to_ascii_uppercase())? };
        VerifyingKey::from_bytes(&key.0).map(|_| key).map_err(|_| TrustError::Key("not an ed25519 point".into()))
    }
}

// One key, four views: iroh's endpoint id, the session bus key and Biscuit's key.
impl From<iroh_base::PublicKey> for PublicKey { fn from(key: iroh_base::PublicKey) -> Self { Self(*key.as_bytes()) } }
impl From<loa_bus::NodeKey> for PublicKey { fn from(key: loa_bus::NodeKey) -> Self { Self(key.0) } }
impl From<PublicKey> for loa_bus::NodeKey { fn from(key: PublicKey) -> Self { Self(key.0) } }
impl TryFrom<PublicKey> for iroh_base::PublicKey {
    type Error = TrustError;
    fn try_from(key: PublicKey) -> Result<Self> { Self::from_bytes(&key.0).map_err(key_error) }
}
impl TryFrom<PublicKey> for biscuit_auth::PublicKey {
    type Error = TrustError;
    fn try_from(key: PublicKey) -> Result<Self> { Self::from_bytes(&key.0, Algorithm::Ed25519).map_err(key_error) }
}
impl TryFrom<&biscuit_auth::PublicKey> for PublicKey {
    type Error = TrustError;
    fn try_from(key: &biscuit_auth::PublicKey) -> Result<Self> { key.to_bytes().try_into().map(Self).map_err(|_| TrustError::Key("not an ed25519 key".into())) }
}

fn key_error(e: impl fmt::Display) -> TrustError { TrustError::Key(e.to_string()) }

/// A fresh nonce per connection the holder signs, so proofs can't be replayed.
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

fn proof_message(challenge: &Challenge, token: &[u8]) -> Vec<u8> { [b"loa-grant-proof/1\0".as_slice(), &challenge.0, token].concat() }

/// Signs `challenge` and `token`, binding both so the proof can't be reused.
pub fn prove(holder: &NodeIdentity, challenge: &Challenge, token: &[u8]) -> Proof { Proof(holder.sign(&proof_message(challenge, token))) }

pub(crate) fn proof_valid(holder: &PublicKey, challenge: &Challenge, token: &[u8], proof: &Proof) -> bool {
    holder.verify(&proof_message(challenge, token), &proof.0)
}
