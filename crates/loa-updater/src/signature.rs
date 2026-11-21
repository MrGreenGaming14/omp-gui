//! Ed25519 signatures over the SHA-256 digest of release artifacts. A release ships
//! `<artifact>.sig`: the 64-byte signature as hex text, optionally newline-terminated.

use crate::stage::Staged;
use crate::{Error, Result, io_at};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;

/// SHA-256 of the file at `path`.
pub fn artifact_digest(path: &Path) -> io::Result<[u8; 32]> {
    let (mut file, mut hasher, mut buf) = (std::fs::File::open(path)?, Sha256::new(), vec![0u8; 64 * 1024]);
    loop {
        match io::Read::read(&mut file, &mut buf)? {
            0 => return Ok(hasher.finalize().into()),
            n => hasher.update(&buf[..n]),
        }
    }
}

/// Re-hashes the staged artifact against its manifest and checks its signature by one of `trusted`.
pub fn verify_staged(staged: &Staged, trusted: &[VerifyingKey]) -> Result<()> {
    let artifact = staged.artifact();
    let signature = staged.signature().ok_or_else(|| Error::Unsigned(artifact.clone()))?;
    let digest = artifact_digest(&artifact).map_err(io_at(&artifact))?;
    let (actual, expected) = (hex::encode(digest), staged.manifest.sha256.to_ascii_lowercase());
    if actual != expected {
        return Err(Error::Mismatch { what: "sha256", expected, actual });
    }
    let text = match std::fs::read_to_string(&signature) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Error::Unsigned(artifact)),
        other => other.map_err(io_at(&signature))?,
    };
    let bytes = hex::decode(text.trim_end_matches(['\n', '\r'])).map_err(|e| Error::Signature(format!("malformed signature: {e}")))?;
    let bytes: [u8; 64] = bytes.try_into().map_err(|b: Vec<u8>| Error::Signature(format!("signature has {} bytes, expected 64", b.len())))?;
    let signature = Signature::from_bytes(&bytes);
    if trusted.iter().any(|key| key.verify_strict(&digest, &signature).is_ok()) {
        Ok(())
    } else {
        Err(Error::Signature(format!("{} is not signed by a trusted key", artifact.display())))
    }
}

/// A 32-byte Ed25519 public key from hex.
pub fn parse_public_key(key_hex: &str) -> Result<VerifyingKey> {
    let invalid = |e: String| Error::Signature(format!("invalid public key: {e}"));
    let bytes: [u8; 32] = hex::decode(key_hex.trim())
        .map_err(|e| invalid(e.to_string()))?
        .try_into()
        .map_err(|b: Vec<u8>| invalid(format!("{} bytes, expected 32", b.len())))?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| invalid(e.to_string()))
}
