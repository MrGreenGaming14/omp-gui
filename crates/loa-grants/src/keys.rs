//! Node keys as grants use them, and the holder proof.

use crate::{GrantError, Result};
use biscuit_auth::{Algorithm, KeyPair, PrivateKey, PublicKey};
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};

/// Public ed25519 key of a node: its network address and grant identity.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodePublicKey(pub [u8; 32]);

impl NodePublicKey {
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(text: &str) -> Result<Self> {
        let mut out = [0u8; 32];
        hex::decode_to_slice(text, &mut out).map_err(|e| GrantError::Key(e.to_string()))?;
        Ok(Self(out))
    }

    pub(crate) fn biscuit(&self) -> Result<PublicKey> {
        PublicKey::from_bytes(&self.0, Algorithm::Ed25519).map_err(|e| GrantError::Key(e.to_string()))
    }

    pub(crate) fn from_biscuit(key: &PublicKey) -> Option<Self> {
        key.to_bytes().try_into().ok().map(Self)
    }
}

impl std::fmt::Display for NodePublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// A node's ed25519 signing key. The app loads the secret from the OS
/// keychain (loa-identity) and hands it in; this crate never stores it.
pub struct NodeKey {
    pair: KeyPair,
    signing: SigningKey,
    public: NodePublicKey,
}

impl NodeKey {
    /// Builds the key from its 32 byte ed25519 secret.
    pub fn from_secret(secret: &[u8; 32]) -> Result<Self> {
        let private = PrivateKey::from_bytes(secret, Algorithm::Ed25519).map_err(|e| GrantError::Key(e.to_string()))?;
        let signing = SigningKey::from_bytes(secret);
        let public = NodePublicKey(signing.verifying_key().to_bytes());
        Ok(Self { pair: KeyPair::from(&private), signing, public })
    }

    /// A fresh random key, for tests and new nodes.
    pub fn generate() -> Result<Self> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| GrantError::Random(e.to_string()))?;
        Self::from_secret(&secret)
    }

    pub fn public(&self) -> NodePublicKey {
        self.public
    }

    pub(crate) fn pair(&self) -> &KeyPair {
        &self.pair
    }

    pub(crate) fn private(&self) -> PrivateKey {
        self.pair.private()
    }
}

impl std::fmt::Debug for NodeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeKey").field("public", &self.public).finish_non_exhaustive()
    }
}

/// A random nonce the verifier sends the holder before accepting a token.
/// Fresh per connection, so a recorded proof can't be replayed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Challenge(pub [u8; 32]);

impl Challenge {
    pub fn random() -> Result<Self> {
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce).map_err(|e| GrantError::Random(e.to_string()))?;
        Ok(Self(nonce))
    }
}

/// The holder's ed25519 signature over a challenge and the token it presents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof(pub [u8; 64]);

const PROOF_DOMAIN: &[u8] = b"loa-grant-proof/1\0";

fn proof_message(challenge: &Challenge, token: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(PROOF_DOMAIN.len() + challenge.0.len() + token.len());
    message.extend_from_slice(PROOF_DOMAIN);
    message.extend_from_slice(&challenge.0);
    message.extend_from_slice(token);
    message
}

/// Signs `challenge` and `token` with the holder's key. The proof binds both,
/// so it can't be reused for another token or another connection.
pub fn prove(holder: &NodeKey, challenge: &Challenge, token: &[u8]) -> Proof {
    Proof(holder.signing.sign(&proof_message(challenge, token)).to_bytes())
}

/// True when `proof` is `holder`'s signature over `challenge` and `token`.
pub(crate) fn proof_valid(holder: &NodePublicKey, challenge: &Challenge, token: &[u8], proof: &Proof) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(&holder.0) else { return false };
    let signature = ed25519_dalek::Signature::from_bytes(&proof.0);
    key.verify_strict(&proof_message(challenge, token), &signature).is_ok()
}
