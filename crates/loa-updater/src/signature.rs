//! Ed25519 signatures over the SHA-256 digest of release artifacts.
//!
//! A release ships `<artifact>.sig` beside the artifact: the 64-byte
//! signature as lowercase hex text, optionally followed by a newline. The
//! signed message is the 32-byte SHA-256 digest of the artifact.

use loa_util::fs::write_atomic;
use crate::layout::Staged;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum SignatureError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{artifact} has no signature")]
    Unsigned { artifact: PathBuf },
    #[error("signature file {path} is malformed: {reason}")]
    Malformed { path: PathBuf, reason: String },
    #[error("invalid public key: {0}")]
    InvalidKey(String),
    #[error("{artifact} is not signed by a trusted key")]
    Untrusted { artifact: PathBuf },
    #[error("{artifact} has sha256 {actual}, manifest expects {expected}")]
    DigestMismatch { artifact: PathBuf, expected: String, actual: String },
}

pub type Result<T, E = SignatureError> = std::result::Result<T, E>;

/// SHA-256 of the file at `path`, read in chunks.
pub fn artifact_digest(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

/// Signs an artifact digest.
pub fn sign_digest(key: &SigningKey, digest: &[u8; 32]) -> Signature {
    key.sign(digest)
}

/// Path of the detached signature for `artifact`: `<artifact>.sig`.
pub fn signature_path(artifact: &Path) -> PathBuf {
    let mut name = artifact.as_os_str().to_owned();
    name.push(".sig");
    PathBuf::from(name)
}

/// Signs `artifact` and writes `<artifact>.sig` beside it. Used by release
/// tooling. Returns the signature path.
pub fn sign_artifact(key: &SigningKey, artifact: &Path) -> Result<PathBuf> {
    let digest = artifact_digest(artifact)
        .map_err(|source| SignatureError::Io { path: artifact.to_owned(), source })?;
    let signature = sign_digest(key, &digest);
    let path = signature_path(artifact);
    let text = format!("{}\n", hex::encode(signature.to_bytes()));
    write_atomic(&path, text.as_bytes(), None)
        .map_err(|source| SignatureError::Io { path: path.clone(), source })?;
    Ok(path)
}

/// Checks that `signature` is a valid signature of `artifact` by one of
/// the `trusted` keys.
pub fn verify_artifact(artifact: &Path, signature: &Path, trusted: &[VerifyingKey]) -> Result<()> {
    let digest = artifact_digest(artifact)
        .map_err(|source| SignatureError::Io { path: artifact.to_owned(), source })?;
    verify_digest(artifact, &digest, signature, trusted)
}

/// Re-hashes the staged artifact, checks it against the manifest's sha256,
/// and verifies its detached signature against the `trusted` keys.
pub fn verify_staged(staged: &Staged, trusted: &[VerifyingKey]) -> Result<()> {
    let artifact = staged.artifact();
    let Some(signature) = staged.signature() else {
        return Err(SignatureError::Unsigned { artifact });
    };
    let digest = artifact_digest(&artifact)
        .map_err(|source| SignatureError::Io { path: artifact.clone(), source })?;
    let actual = hex::encode(digest);
    let expected = staged.manifest.sha256.to_ascii_lowercase();
    if actual != expected {
        return Err(SignatureError::DigestMismatch { artifact, expected, actual });
    }
    verify_digest(&artifact, &digest, &signature, trusted)
}

/// Parses a 32-byte Ed25519 public key from hex.
pub fn parse_public_key(key_hex: &str) -> Result<VerifyingKey> {
    let bytes =
        hex::decode(key_hex.trim()).map_err(|e| SignatureError::InvalidKey(e.to_string()))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|b: Vec<u8>| SignatureError::InvalidKey(format!("{} bytes, expected 32", b.len())))?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| SignatureError::InvalidKey(e.to_string()))
}

fn verify_digest(
    artifact: &Path,
    digest: &[u8; 32],
    signature: &Path,
    trusted: &[VerifyingKey],
) -> Result<()> {
    let signature = read_signature(artifact, signature)?;
    if trusted.iter().any(|key| key.verify_strict(digest, &signature).is_ok()) {
        Ok(())
    } else {
        Err(SignatureError::Untrusted { artifact: artifact.to_owned() })
    }
}

fn read_signature(artifact: &Path, path: &Path) -> Result<Signature> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(SignatureError::Unsigned { artifact: artifact.to_owned() });
        }
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            return Err(SignatureError::Malformed {
                path: path.to_owned(),
                reason: "not text".into(),
            });
        }
        Err(source) => return Err(SignatureError::Io { path: path.to_owned(), source }),
    };
    let malformed = |reason: String| SignatureError::Malformed { path: path.to_owned(), reason };
    let hex_text = text.strip_suffix('\n').unwrap_or(&text);
    let hex_text = hex_text.strip_suffix('\r').unwrap_or(hex_text);
    let bytes = hex::decode(hex_text).map_err(|e| malformed(e.to_string()))?;
    let bytes: [u8; 64] = bytes
        .try_into()
        .map_err(|b: Vec<u8>| malformed(format!("{} bytes, expected 64", b.len())))?;
    Ok(Signature::from_bytes(&bytes))
}
