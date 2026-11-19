use ed25519_dalek::SigningKey;
use loa_updater::layout::{ARTIFACT_DIR, MANIFEST_FILE, PAYLOAD_DIR};
use loa_updater::signature::{
    SignatureError, artifact_digest, parse_public_key, sign_artifact, verify_artifact,
    verify_staged,
};
use loa_updater::{Staged, StagedManifest};
use semver::Version;
use std::fs;
use std::path::Path;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn write_artifact(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("app-1.2.0.tar.gz");
    fs::write(&path, b"release bytes of the app, version 1.2.0").unwrap();
    path
}

fn stage(root: &Path, key: &SigningKey) -> Staged {
    let dir = root.join("versions/app/1.2.0");
    fs::create_dir_all(dir.join(PAYLOAD_DIR)).unwrap();
    fs::create_dir_all(dir.join(ARTIFACT_DIR)).unwrap();
    let artifact = write_artifact(&dir.join(ARTIFACT_DIR));
    sign_artifact(key, &artifact).unwrap();
    let manifest = StagedManifest {
        product: "app".into(),
        version: Version::new(1, 2, 0),
        artifact_name: "app-1.2.0.tar.gz".into(),
        sha256: hex::encode(artifact_digest(&artifact).unwrap()),
        size: fs::metadata(&artifact).unwrap().len(),
        signature_name: Some("app-1.2.0.tar.gz.sig".into()),
    };
    fs::write(dir.join(MANIFEST_FILE), serde_json::to_vec(&manifest).unwrap()).unwrap();
    Staged { dir, manifest }
}

#[test]
fn signed_artifact_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = write_artifact(dir.path());
    let sig = sign_artifact(&key(1), &artifact).unwrap();
    assert_eq!(sig, dir.path().join("app-1.2.0.tar.gz.sig"));
    let text = fs::read_to_string(&sig).unwrap();
    assert_eq!(text.trim_end().len(), 128);
    assert!(text.trim_end().bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));

    // Another trusted key listed first doesn't stop the matching one.
    let trusted = [key(2).verifying_key(), key(1).verifying_key()];
    verify_artifact(&artifact, &sig, &trusted).unwrap();
}

#[test]
fn signature_without_trailing_newline_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = write_artifact(dir.path());
    let sig = sign_artifact(&key(1), &artifact).unwrap();
    let text = fs::read_to_string(&sig).unwrap();
    fs::write(&sig, text.trim_end()).unwrap();
    verify_artifact(&artifact, &sig, &[key(1).verifying_key()]).unwrap();
}

#[test]
fn tampered_artifact_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = write_artifact(dir.path());
    let sig = sign_artifact(&key(1), &artifact).unwrap();
    let mut bytes = fs::read(&artifact).unwrap();
    bytes[3] ^= 0x01;
    fs::write(&artifact, bytes).unwrap();
    let err = verify_artifact(&artifact, &sig, &[key(1).verifying_key()]).unwrap_err();
    assert!(matches!(err, SignatureError::Untrusted { .. }), "{err}");
}

#[test]
fn untrusted_key_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = write_artifact(dir.path());
    let sig = sign_artifact(&key(9), &artifact).unwrap();
    let err = verify_artifact(&artifact, &sig, &[key(1).verifying_key()]).unwrap_err();
    assert!(matches!(err, SignatureError::Untrusted { .. }), "{err}");
}

#[test]
fn missing_signature_is_unsigned() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = write_artifact(dir.path());
    let err = verify_artifact(&artifact, &dir.path().join("app-1.2.0.tar.gz.sig"), &[
        key(1).verifying_key(),
    ])
    .unwrap_err();
    assert!(matches!(err, SignatureError::Unsigned { .. }), "{err}");

    let root = tempfile::tempdir().unwrap();
    let mut staged = stage(root.path(), &key(1));
    staged.manifest.signature_name = None;
    let err = verify_staged(&staged, &[key(1).verifying_key()]).unwrap_err();
    assert!(matches!(err, SignatureError::Unsigned { .. }), "{err}");
}

#[test]
fn staged_artifact_verifies_and_mismatch_is_caught() {
    let root = tempfile::tempdir().unwrap();
    let staged = stage(root.path(), &key(1));
    verify_staged(&staged, &[key(1).verifying_key()]).unwrap();

    // Replace the artifact and re-sign it: the signature is valid, but the
    // bytes are not what was staged.
    fs::write(staged.artifact(), b"something else entirely").unwrap();
    sign_artifact(&key(1), &staged.artifact()).unwrap();
    let err = verify_staged(&staged, &[key(1).verifying_key()]).unwrap_err();
    assert!(matches!(err, SignatureError::DigestMismatch { .. }), "{err}");
}

#[test]
fn public_key_parses_from_hex() {
    let vk = key(1).verifying_key();
    assert_eq!(parse_public_key(&hex::encode(vk.to_bytes())).unwrap(), vk);
    assert!(matches!(parse_public_key("abcd"), Err(SignatureError::InvalidKey(_))));
    assert!(matches!(parse_public_key("not hex"), Err(SignatureError::InvalidKey(_))));
}
