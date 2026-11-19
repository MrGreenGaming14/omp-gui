//! The node keypair, its public key and signatures.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use data_encoding::{BASE32_NOPAD, HEXLOWER, HEXLOWER_PERMISSIVE};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use zeroize::Zeroizing;

use crate::store::KeyStore;
use crate::{IdentityError, Result};

/// Length of the secret seed, which is all that is stored.
pub const SECRET_KEY_LENGTH: usize = ed25519_dalek::SECRET_KEY_LENGTH;
const PUBLIC_KEY_LENGTH: usize = ed25519_dalek::PUBLIC_KEY_LENGTH;
/// Prefix of a public key in Biscuit's text form.
const BISCUIT_PREFIX: &str = "ed25519/";

/// This node's ed25519 keypair: its network address and its signing key.
///
/// The secret is wiped from memory on drop and never printed.
#[derive(Clone)]
pub struct NodeIdentity {
    signing: SigningKey,
}

impl NodeIdentity {
    /// Loads the node key from `store`, generating and storing a new one on first run.
    ///
    /// A stored value of the wrong length is an error; it is never replaced, since
    /// a new key would give the node a new address and void every grant.
    pub fn load_or_create(store: &dyn KeyStore) -> Result<Self> {
        if let Some(secret) = store.load()? {
            let seed: &[u8; SECRET_KEY_LENGTH] =
                secret
                    .as_slice()
                    .try_into()
                    .map_err(|_| IdentityError::WrongLength {
                        found: secret.len(),
                    })?;
            return Ok(Self::from_secret_bytes(seed));
        }
        let identity = Self::generate()?;
        store.store(identity.secret_bytes())?;
        Ok(identity)
    }

    /// A fresh key from the operating system random source, not stored anywhere.
    pub fn generate() -> Result<Self> {
        let mut seed = Zeroizing::new([0u8; SECRET_KEY_LENGTH]);
        getrandom::fill(seed.as_mut_slice()).map_err(IdentityError::Random)?;
        Ok(Self::from_secret_bytes(&seed))
    }

    /// The identity whose secret seed is `seed`.
    pub fn from_secret_bytes(seed: &[u8; SECRET_KEY_LENGTH]) -> Self {
        Self {
            signing: SigningKey::from_bytes(seed),
        }
    }

    /// The 32 byte secret seed. The network layer builds the iroh secret key
    /// from it (`iroh::SecretKey::from_bytes`) and the grants layer the Biscuit
    /// keypair (`PrivateKey::from_bytes(.., Algorithm::Ed25519)`).
    pub fn secret_bytes(&self) -> &[u8; SECRET_KEY_LENGTH] {
        self.signing.as_bytes()
    }

    /// The public half: the endpoint id peers dial and the key grants are bound to.
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.signing.verifying_key())
    }

    /// Signs `message` with the node key (pure Ed25519, RFC 8032).
    pub fn sign(&self, message: &[u8]) -> Signature {
        Signature(self.signing.sign(message).to_bytes())
    }
}

impl fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeIdentity")
            .field("public_key", &self.public_key())
            .finish_non_exhaustive()
    }
}

/// A node's ed25519 public key.
///
/// `Display` prints the iroh endpoint id form (64 lowercase hex characters).
/// `FromStr` accepts the endpoint id form and the Biscuit form.
#[derive(Clone, Copy)]
pub struct PublicKey(VerifyingKey);

impl PublicKey {
    /// The key from its 32 compressed bytes; fails when they are not a curve point.
    pub fn from_bytes(bytes: &[u8; PUBLIC_KEY_LENGTH]) -> Result<Self> {
        VerifyingKey::from_bytes(bytes)
            .map(Self)
            .map_err(|_| IdentityError::InvalidPublicKey)
    }

    /// The 32 compressed bytes, as used by iroh and Biscuit.
    pub fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LENGTH] {
        self.0.as_bytes()
    }

    /// Checks that `signature` was made over `message` by this key's secret.
    ///
    /// Uses strict verification, which rejects weak keys and malleable
    /// signatures, the same check Biscuit applies.
    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<()> {
        let signature = ed25519_dalek::Signature::from_bytes(&signature.0);
        self.0
            .verify_strict(message, &signature)
            .map_err(|_| IdentityError::BadSignature)
    }

    /// The form iroh prints for an endpoint id: 64 lowercase hex characters.
    pub fn to_endpoint_id_string(&self) -> String {
        HEXLOWER.encode(self.as_bytes())
    }

    /// Parses what iroh accepts for an endpoint id: 64 lowercase hex characters,
    /// or 52 characters of unpadded base32 in either case.
    pub fn parse_endpoint_id(text: &str) -> Result<Self> {
        let mut bytes = [0u8; PUBLIC_KEY_LENGTH];
        if text.len() == PUBLIC_KEY_LENGTH * 2 {
            decode_exact(&HEXLOWER, text.as_bytes(), &mut bytes, "bad hex")?;
        } else {
            let upper = text.to_ascii_uppercase();
            decode_exact(&BASE32_NOPAD, upper.as_bytes(), &mut bytes, "bad base32")?;
        }
        Self::from_bytes(&bytes)
    }

    /// The form Biscuit prints for a public key: `ed25519/` and 64 lowercase hex characters.
    pub fn to_biscuit_string(&self) -> String {
        let mut text = String::with_capacity(BISCUIT_PREFIX.len() + PUBLIC_KEY_LENGTH * 2);
        text.push_str(BISCUIT_PREFIX);
        HEXLOWER.encode_append(self.as_bytes(), &mut text);
        text
    }

    /// Parses Biscuit's text form: `ed25519/` and 64 hex characters in either case.
    pub fn parse_biscuit(text: &str) -> Result<Self> {
        let hex = text
            .strip_prefix(BISCUIT_PREFIX)
            .ok_or(IdentityError::ParseKey("missing ed25519/ prefix"))?;
        let mut bytes = [0u8; PUBLIC_KEY_LENGTH];
        decode_exact(&HEXLOWER_PERMISSIVE, hex.as_bytes(), &mut bytes, "bad hex")?;
        Self::from_bytes(&bytes)
    }
}

/// Decodes `input` into exactly `out.len()` bytes.
fn decode_exact(
    encoding: &data_encoding::Encoding,
    input: &[u8],
    out: &mut [u8],
    what: &'static str,
) -> Result<()> {
    if encoding.decode_len(input.len()) != Ok(out.len()) {
        return Err(IdentityError::ParseKey("wrong length"));
    }
    match encoding.decode_mut(input, out) {
        Ok(len) if len == out.len() => Ok(()),
        Ok(_) => Err(IdentityError::ParseKey("wrong length")),
        Err(_) => Err(IdentityError::ParseKey(what)),
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = [0u8; PUBLIC_KEY_LENGTH * 2];
        HEXLOWER.encode_mut(self.as_bytes(), &mut buf);
        // HEXLOWER only writes ASCII characters.
        f.write_str(std::str::from_utf8(&buf).map_err(|_| fmt::Error)?)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({self})")
    }
}

impl FromStr for PublicKey {
    type Err = IdentityError;

    fn from_str(text: &str) -> Result<Self> {
        if text.starts_with(BISCUIT_PREFIX) {
            Self::parse_biscuit(text)
        } else {
            Self::parse_endpoint_id(text)
        }
    }
}

impl PartialEq for PublicKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for PublicKey {}

impl Hash for PublicKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_bytes().hash(state);
    }
}

impl PartialOrd for PublicKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PublicKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

/// A 64 byte ed25519 signature.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature([u8; Signature::LENGTH]);

impl Signature {
    /// Length in bytes.
    pub const LENGTH: usize = ed25519_dalek::SIGNATURE_LENGTH;

    /// A signature from its wire bytes.
    pub fn from_bytes(bytes: &[u8; Self::LENGTH]) -> Self {
        Self(*bytes)
    }

    /// The wire bytes, as iroh and Biscuit carry them.
    pub fn to_bytes(&self) -> [u8; Self::LENGTH] {
        self.0
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature({})", HEXLOWER.encode(&self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryKeyStore;

    #[test]
    fn first_run_creates_and_later_runs_load_the_same_key() {
        let store = MemoryKeyStore::new();
        let first = NodeIdentity::load_or_create(&store).unwrap();
        let stored = store.load().unwrap().unwrap();
        assert_eq!(stored.as_slice(), first.secret_bytes());
        let second = NodeIdentity::load_or_create(&store).unwrap();
        assert_eq!(first.public_key(), second.public_key());
    }

    #[test]
    fn two_nodes_get_different_keys() {
        let a = NodeIdentity::load_or_create(&MemoryKeyStore::new()).unwrap();
        let b = NodeIdentity::load_or_create(&MemoryKeyStore::new()).unwrap();
        assert_ne!(a.public_key(), b.public_key());
    }

    #[test]
    fn stored_value_of_wrong_length_is_an_error_and_left_in_place() {
        let bad = [7u8; 31];
        let store = MemoryKeyStore::with_secret(&bad);
        let err = NodeIdentity::load_or_create(&store).unwrap_err();
        assert!(matches!(err, IdentityError::WrongLength { found: 31 }));
        assert_eq!(store.load().unwrap().unwrap().as_slice(), bad);

        let store = MemoryKeyStore::with_secret(&[7u8; 64]);
        assert!(matches!(
            NodeIdentity::load_or_create(&store),
            Err(IdentityError::WrongLength { found: 64 })
        ));
    }

    #[test]
    fn store_failure_is_reported_not_papered_over() {
        struct Broken;
        impl KeyStore for Broken {
            fn load(&self) -> Result<Option<crate::SecretBytes>> {
                Err(IdentityError::Store("locked".into()))
            }
            fn store(&self, _: &[u8]) -> Result<()> {
                unreachable!("must not write after a failed read")
            }
        }
        assert!(matches!(
            NodeIdentity::load_or_create(&Broken),
            Err(IdentityError::Store(_))
        ));
    }

    #[test]
    fn rfc8032_test_vector_1() {
        let seed: [u8; 32] = HEXLOWER
            .decode(b"9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
            .unwrap()
            .try_into()
            .unwrap();
        let id = NodeIdentity::from_secret_bytes(&seed);
        assert_eq!(
            id.public_key().to_endpoint_id_string(),
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
        );
        assert_eq!(
            HEXLOWER.encode(&id.sign(b"").to_bytes()),
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
             5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        );
    }

    #[test]
    fn signature_verifies_only_for_same_key_and_message() {
        let id = NodeIdentity::generate().unwrap();
        let other = NodeIdentity::generate().unwrap();
        let sig = id.sign(b"grant");
        id.public_key().verify(b"grant", &sig).unwrap();
        assert!(matches!(
            id.public_key().verify(b"grant!", &sig),
            Err(IdentityError::BadSignature)
        ));
        assert!(other.public_key().verify(b"grant", &sig).is_err());
        let mut tampered = sig.to_bytes();
        tampered[0] ^= 1;
        assert!(
            id.public_key()
                .verify(b"grant", &Signature::from_bytes(&tampered))
                .is_err()
        );
    }

    #[test]
    fn text_forms_round_trip() {
        let key = NodeIdentity::generate().unwrap().public_key();
        let endpoint = key.to_endpoint_id_string();
        assert_eq!(endpoint, key.to_string());
        assert_eq!(PublicKey::parse_endpoint_id(&endpoint).unwrap(), key);
        assert_eq!(endpoint.parse::<PublicKey>().unwrap(), key);

        let base32 = BASE32_NOPAD.encode(key.as_bytes());
        assert_eq!(PublicKey::parse_endpoint_id(&base32).unwrap(), key);
        assert_eq!(
            PublicKey::parse_endpoint_id(&base32.to_ascii_lowercase()).unwrap(),
            key
        );

        let biscuit = key.to_biscuit_string();
        assert_eq!(biscuit, format!("ed25519/{endpoint}"));
        assert_eq!(PublicKey::parse_biscuit(&biscuit).unwrap(), key);
        assert_eq!(biscuit.parse::<PublicKey>().unwrap(), key);
        assert_eq!(
            PublicKey::parse_biscuit(&biscuit.to_ascii_uppercase().replace("ED25519", "ed25519"))
                .unwrap(),
            key
        );
    }

    #[test]
    fn malformed_key_text_is_rejected() {
        let key = NodeIdentity::generate().unwrap().public_key();
        let endpoint = key.to_endpoint_id_string();
        for bad in [
            "",
            &endpoint[..63],
            &format!("{endpoint}0"),
            &endpoint.replace(|c: char| c.is_ascii_digit(), "g"),
            &endpoint.to_ascii_uppercase(),
            &format!("secp256r1/{endpoint}"),
            &format!("ed25519/{}", &endpoint[..62]),
            "ed25519/",
        ] {
            assert!(bad.parse::<PublicKey>().is_err(), "accepted {bad:?}");
        }
        assert!(PublicKey::parse_biscuit(&endpoint).is_err());
    }

    #[test]
    fn bytes_off_the_curve_are_not_a_public_key() {
        // y = 2 has no matching x on edwards25519.
        let mut bytes = [0u8; 32];
        bytes[0] = 2;
        assert!(matches!(
            PublicKey::from_bytes(&bytes),
            Err(IdentityError::InvalidPublicKey)
        ));
    }

    #[test]
    fn debug_never_shows_the_secret() {
        let id = NodeIdentity::generate().unwrap();
        let secret_hex = HEXLOWER.encode(id.secret_bytes());
        let printed = format!(
            "{id:?} {:?}",
            MemoryKeyStore::with_secret(id.secret_bytes())
        );
        assert!(!printed.contains(&format!("{:?}", id.secret_bytes())));
        assert!(!printed.contains(&secret_hex));
        assert!(printed.contains(&id.public_key().to_string()));
    }
}
