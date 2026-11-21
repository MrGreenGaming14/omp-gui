//! `known_hosts` checks: hashed names, wildcard and negated patterns, `[host]:port`,
//! `@cert-authority` and `@revoked`. Files are re-read on every check; accepted keys are appended.

use loa_util::LockExt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use loa_util::ssh::wildcard_match;
use russh::keys::ssh_key::certificate::CertType;
use russh::keys::ssh_key::public::KeyData;
use russh::keys::{Certificate, HashAlg, PublicKey, PublicKeyOrCertificate};
use sha1::Sha1;

use crate::error::{Error, Result};

/// The name `known_hosts` uses for a host: `host` on port 22, else `[host]:port`.
pub fn host_name(hostname: &str, port: u16) -> String {
    if port == 22 { hostname.to_owned() } else { format!("[{hostname}]:{port}") }
}

/// `SHA256:…` fingerprint of a key, as `ssh-keygen -l` prints it.
pub fn fingerprint(key: &KeyData) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// A key already on file for a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownKey {
    pub file: PathBuf,
    pub line: usize,
    pub key_type: String,
    pub fingerprint: String,
}

/// Outcome of checking a presented host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyStatus {
    Trusted,
    /// Listed under `@revoked`: never connect, never prompt.
    Revoked { entry: KnownKey },
    Unknown,
    /// Keys are on file for this host but none matches.
    Changed { previous: Vec<KnownKey> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    None,
    CertAuthority,
    Revoked,
}

struct Entry {
    marker: Marker,
    hosts: String,
    key: PublicKey,
    file: PathBuf,
    line: usize,
}

impl Entry {
    fn known(&self) -> KnownKey {
        let (key_type, fingerprint) = (self.key.algorithm().as_str().to_owned(), fingerprint(self.key.key_data()));
        KnownKey { file: self.file.clone(), line: self.line, key_type, fingerprint }
    }

    fn matches_host(&self, name: &str) -> bool {
        if let Some(hashed) = self.hosts.strip_prefix("|1|") {
            return hashed_matches(hashed, name).unwrap_or(false);
        }
        let mut matched = false;
        for pattern in self.hosts.split(',') {
            let (negated, pattern) = pattern.strip_prefix('!').map_or((false, pattern), |p| (true, p));
            if wildcard_match(&pattern.to_ascii_lowercase(), name) {
                if negated {
                    return false;
                }
                matched = true;
            }
        }
        matched
    }
}

fn hashed_matches(hashed: &str, name: &str) -> Option<bool> {
    let (salt, hash) = hashed.split_once('|')?;
    let mut mac = Hmac::<Sha1>::new_from_slice(&STANDARD.decode(salt).ok()?).ok()?;
    mac.update(name.as_bytes());
    Some(mac.verify_slice(&STANDARD.decode(hash).ok()?).is_ok())
}

/// The `known_hosts` files a connection is checked against.
#[derive(Debug)]
pub struct KnownHosts {
    files: Vec<PathBuf>,
    write_to: PathBuf,
    write_lock: Mutex<()>,
}

impl KnownHosts {
    /// `files` are read in order; accepted keys are appended to `write_to`.
    pub fn new(files: Vec<PathBuf>, write_to: PathBuf) -> Self {
        Self { files, write_to, write_lock: Mutex::new(()) }
    }

    /// Checks the key `hostname:port` presented.
    pub fn check(&self, hostname: &str, port: u16, presented: &PublicKeyOrCertificate) -> Result<HostKeyStatus> {
        let name = host_name(hostname, port).to_ascii_lowercase();
        let entries = self.load()?;
        let entries: Vec<&Entry> = entries.iter().filter(|e| e.matches_host(&name)).collect();
        Ok(match presented {
            PublicKeyOrCertificate::PublicKey { key, .. } => check_key(&entries, key.key_data()),
            PublicKeyOrCertificate::Certificate(cert) => check_certificate(&entries, cert, hostname),
        })
    }

    /// Appends `hostname:port key` to the writable file.
    pub fn add(&self, hostname: &str, port: u16, key: &KeyData) -> Result<()> {
        let fail = |message: String| Error::KnownHosts { path: self.write_to.clone(), message };
        let line = PublicKey::new(key.clone(), "").to_openssh().map_err(|e| fail(e.to_string()))?;
        let _guard = self.write_lock.locked();
        let path = &self.write_to;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| fail(e.to_string()))?;
        }
        let needs_newline = match std::fs::read(path) {
            Ok(bytes) => bytes.last().is_some_and(|&b| b != b'\n'),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(fail(e.to_string())),
        };
        let text = format!("{}{} {}\n", if needs_newline { "\n" } else { "" }, host_name(hostname, port), line.trim_end());
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| fail(e.to_string()))?;
        file.write_all(text.as_bytes()).map_err(|e| fail(e.to_string()))
    }

    fn load(&self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for path in &self.files {
            match std::fs::read_to_string(path) {
                Ok(text) => parse_into(&text, path, &mut entries),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(Error::Io { path: path.clone(), source }),
            }
        }
        Ok(entries)
    }
}

/// Parses `known_hosts` text; malformed lines are skipped, as OpenSSH does.
fn parse_into(text: &str, file: &Path, out: &mut Vec<Entry>) {
    for (index, line) in text.lines().map(str::trim).enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (marker, rest) = match line.strip_prefix('@').map(|r| r.split_once(char::is_whitespace).unwrap_or((r, ""))) {
            Some(("cert-authority", rest)) => (Marker::CertAuthority, rest.trim_start()),
            Some(("revoked", rest)) => (Marker::Revoked, rest.trim_start()),
            Some(_) => continue,
            None => (Marker::None, line),
        };
        let Some((hosts, key)) = rest.split_once(char::is_whitespace) else { continue };
        let Ok(key) = PublicKey::from_openssh(key.trim()) else { continue };
        out.push(Entry { marker, hosts: hosts.to_owned(), key, file: file.to_path_buf(), line: index + 1 });
    }
}

fn revoked(entries: &[&Entry], key: &KeyData) -> Option<HostKeyStatus> {
    let entry = entries.iter().find(|e| e.marker == Marker::Revoked && e.key.key_data() == key)?;
    Some(HostKeyStatus::Revoked { entry: entry.known() })
}

fn check_key(entries: &[&Entry], key: &KeyData) -> HostKeyStatus {
    if let Some(status) = revoked(entries, key) {
        return status;
    }
    let plain: Vec<&&Entry> = entries.iter().filter(|e| e.marker == Marker::None).collect();
    if plain.iter().any(|e| e.key.key_data() == key) {
        HostKeyStatus::Trusted
    } else if plain.is_empty() {
        HostKeyStatus::Unknown
    } else {
        HostKeyStatus::Changed { previous: plain.iter().map(|e| e.known()).collect() }
    }
}

fn check_certificate(entries: &[&Entry], cert: &Certificate, hostname: &str) -> HostKeyStatus {
    let ca = cert.signature_key();
    if let Some(status) = revoked(entries, ca) {
        return status;
    }
    if entries.iter().any(|e| e.marker == Marker::CertAuthority && e.key.key_data() == ca) {
        let principal_ok = cert.valid_principals().is_empty() || cert.valid_principals().iter().any(|p| p == hostname);
        let valid = cert.validate_at(loa_util::time::now_secs(), [&ca.fingerprint(HashAlg::Sha256)]).is_ok();
        if cert.cert_type() == CertType::Host && principal_ok && valid {
            return HostKeyStatus::Trusted;
        }
    }
    // Not vouched for by a CA on file: judge the certified key itself.
    check_key(entries, cert.public_key())
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::ssh_key::certificate::Builder;
    use russh::keys::{Algorithm, PrivateKey};

    fn key() -> PrivateKey {
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
    }

    fn line(k: &PrivateKey) -> String {
        k.public_key().to_openssh().unwrap()
    }

    #[test]
    fn statuses_for_hashed_ported_negated_revoked_and_certified_entries() {
        let dir = tempfile::tempdir().unwrap();
        let [a, b, old, bad, ca, rogue] = [(); 6].map(|()| key());
        let mut mac = Hmac::<Sha1>::new_from_slice(b"0123456789abcdefghij").unwrap();
        mac.update(b"web.example");
        let hashed = format!("|1|{}|{}", STANDARD.encode(b"0123456789abcdefghij"), STANDARD.encode(mac.finalize().into_bytes()));
        let (file, global) = (dir.path().join("sub/kh"), dir.path().join("global"));
        let text = format!("{hashed} {}\n[box.lan]:2222 {}\n*.lan,!secret.lan {}\n@cert-authority *.ca {}\n", line(&a), line(&b), line(&old), line(&ca));
        std::fs::write(&global, format!("@revoked * {}\n", line(&bad))).unwrap();
        let kh = KnownHosts::new(vec![file.clone(), global], file.clone());
        assert_eq!(kh.check("10.0.0.5", 2200, &a.public_key().clone().into()).unwrap(), HostKeyStatus::Unknown);
        kh.add("10.0.0.5", 2200, a.public_key().key_data()).unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().starts_with("[10.0.0.5]:2200 ssh-ed25519 "));
        std::fs::write(&file, format!("{text}# no newline")).unwrap();
        kh.add("h", 22, a.public_key().key_data()).unwrap();
        let cert = |signer: &PrivateKey, principal: &str| {
            let now = loa_util::time::now_secs();
            let mut builder = Builder::new_with_random_nonce(&mut rand::rng(), b.public_key().key_data().clone(), now - 60, now + 3600).unwrap();
            builder.cert_type(CertType::Host).unwrap();
            builder.valid_principal(principal).unwrap();
            PublicKeyOrCertificate::Certificate(builder.sign(signer).unwrap())
        };
        let changed = HostKeyStatus::Changed { previous: vec![KnownKey { file: file.clone(), line: 3, key_type: "ssh-ed25519".into(), fingerprint: fingerprint(old.public_key().key_data()) }] };
        let cases = [
            ("web.example", 22, a.public_key().clone().into(), HostKeyStatus::Trusted),
            ("other.example", 22, a.public_key().clone().into(), HostKeyStatus::Unknown),
            ("h", 22, a.public_key().clone().into(), HostKeyStatus::Trusted),
            ("box.lan", 2222, b.public_key().clone().into(), HostKeyStatus::Trusted),
            ("box.lan", 22, b.public_key().clone().into(), changed.clone()),
            ("db.lan", 22, b.public_key().clone().into(), changed),
            ("secret.lan", 22, b.public_key().clone().into(), HostKeyStatus::Unknown),
            ("db.ca", 22, cert(&ca, "db.ca"), HostKeyStatus::Trusted),
            ("db.ca", 22, cert(&ca, "web.ca"), HostKeyStatus::Unknown),
            ("db.ca", 22, cert(&rogue, "db.ca"), HostKeyStatus::Unknown),
        ];
        for (host, port, presented, want) in cases {
            assert_eq!(kh.check(host, port, &presented).unwrap(), want, "{host}:{port}");
        }
        assert!(matches!(kh.check("db.lan", 22, &bad.public_key().clone().into()).unwrap(), HostKeyStatus::Revoked { .. }));
    }
}
