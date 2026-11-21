//! `known_hosts` checks (#97).
//!
//! Reads one or more OpenSSH `known_hosts` files on every check (so edits made
//! outside the app apply at once). Supports hashed host names (`|1|salt|hash`),
//! comma separated patterns with wildcards and negation, `[host]:port` names,
//! and the `@cert-authority` and `@revoked` markers. New keys are appended to
//! one writable file only after the user accepted them.

use loa_util::LockExt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use russh::keys::ssh_key::certificate::CertType;
use russh::keys::ssh_key::public::KeyData;
use russh::keys::{Certificate, HashAlg, PublicKey, PublicKeyOrCertificate};
use sha1::Sha1;

use crate::error::{Error, Result};
use crate::ssh_config::{DEFAULT_PORT, wildcard_match};

/// The name `known_hosts` uses for a host: `host` on port 22, else `[host]:port`.
pub fn host_name(hostname: &str, port: u16) -> String {
    if port == DEFAULT_PORT {
        hostname.to_string()
    } else {
        format!("[{hostname}]:{port}")
    }
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
    /// A matching key (or a valid certificate from a trusted CA) is on file.
    Trusted,
    /// The key is listed under `@revoked`. Never connect, never prompt.
    Revoked { entry: KnownKey },
    /// Nothing is on file for this host.
    Unknown,
    /// Keys are on file for this host but none matches the presented one.
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
        KnownKey {
            file: self.file.clone(),
            line: self.line,
            key_type: self.key.algorithm().as_str().to_string(),
            fingerprint: fingerprint(self.key.key_data()),
        }
    }

    fn matches_host(&self, name: &str) -> bool {
        if let Some(hashed) = self.hosts.strip_prefix("|1|") {
            return hashed_matches(hashed, name);
        }
        let mut matched = false;
        for pattern in self.hosts.split(',') {
            let (negated, pattern) = match pattern.strip_prefix('!') {
                Some(p) => (true, p),
                None => (false, pattern),
            };
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

fn hashed_matches(hashed: &str, name: &str) -> bool {
    let Some((salt, hash)) = hashed.split_once('|') else {
        return false;
    };
    let (Ok(salt), Ok(hash)) = (STANDARD.decode(salt), STANDARD.decode(hash)) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(&salt) else {
        return false;
    };
    mac.update(name.as_bytes());
    mac.verify_slice(&hash).is_ok()
}

/// The set of `known_hosts` files a connection is checked against.
#[derive(Debug)]
pub struct KnownHosts {
    files: Vec<PathBuf>,
    write_to: PathBuf,
    write_lock: std::sync::Mutex<()>,
}

impl KnownHosts {
    /// `files` are read in order; accepted keys are appended to `write_to`
    /// (normally `~/.ssh/known_hosts`, which should also be in `files`).
    pub fn new(files: Vec<PathBuf>, write_to: PathBuf) -> Self {
        Self {
            files,
            write_to,
            write_lock: std::sync::Mutex::new(()),
        }
    }

    /// Checks the key `hostname:port` presented.
    pub fn check(
        &self,
        hostname: &str,
        port: u16,
        presented: &PublicKeyOrCertificate,
    ) -> Result<HostKeyStatus> {
        let name = host_name(hostname, port).to_ascii_lowercase();
        let entries = self.load()?;
        let entries: Vec<&Entry> = entries.iter().filter(|e| e.matches_host(&name)).collect();
        match presented {
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                Ok(check_key(&entries, key.key_data()))
            }
            PublicKeyOrCertificate::Certificate(cert) => {
                Ok(check_certificate(&entries, cert, hostname))
            }
        }
    }

    /// Appends `hostname:port key` to the writable file.
    pub fn add(&self, hostname: &str, port: u16, key: &KeyData) -> Result<()> {
        let path = &self.write_to;
        let fail = |message: String| Error::KnownHosts {
            path: path.clone(),
            message,
        };
        let line = PublicKey::new(key.clone(), "")
            .to_openssh()
            .map_err(|e| fail(e.to_string()))?;
        // Serialises writers in this process; the file is opened in append
        // mode so concurrent OpenSSH writers interleave whole lines.
        let _guard = self
            .write_lock
            .locked();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| fail(e.to_string()))?;
        }
        let needs_newline = match std::fs::read(path) {
            Ok(bytes) => bytes.last().is_some_and(|&b| b != b'\n'),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(fail(e.to_string())),
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| fail(e.to_string()))?;
        let mut out = String::with_capacity(line.len() + hostname.len() + 16);
        if needs_newline {
            out.push('\n');
        }
        out.push_str(&host_name(hostname, port));
        out.push(' ');
        out.push_str(line.trim_end());
        out.push('\n');
        file.write_all(out.as_bytes())
            .map_err(|e| fail(e.to_string()))
    }

    fn load(&self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for path in &self.files {
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(Error::Io {
                        path: path.clone(),
                        source,
                    });
                }
            };
            parse_into(&text, path, &mut entries);
        }
        Ok(entries)
    }
}

/// Parses `known_hosts` text. Malformed lines are skipped, as OpenSSH does.
fn parse_into(text: &str, file: &Path, out: &mut Vec<Entry>) {
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (marker, rest) = match line.strip_prefix('@') {
            Some(rest) => {
                let (m, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                let marker = match m {
                    "cert-authority" => Marker::CertAuthority,
                    "revoked" => Marker::Revoked,
                    _ => continue,
                };
                (marker, rest.trim_start())
            }
            None => (Marker::None, line),
        };
        let Some((hosts, key)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(key) = PublicKey::from_openssh(key.trim()) else {
            continue;
        };
        out.push(Entry {
            marker,
            hosts: hosts.to_string(),
            key,
            file: file.to_path_buf(),
            line: index + 1,
        });
    }
}

fn check_key(entries: &[&Entry], key: &KeyData) -> HostKeyStatus {
    if let Some(revoked) = entries
        .iter()
        .find(|e| e.marker == Marker::Revoked && e.key.key_data() == key)
    {
        return HostKeyStatus::Revoked {
            entry: revoked.known(),
        };
    }
    let plain: Vec<&&Entry> = entries.iter().filter(|e| e.marker == Marker::None).collect();
    if plain.iter().any(|e| e.key.key_data() == key) {
        return HostKeyStatus::Trusted;
    }
    if plain.is_empty() {
        HostKeyStatus::Unknown
    } else {
        HostKeyStatus::Changed {
            previous: plain.iter().map(|e| e.known()).collect(),
        }
    }
}

fn check_certificate(entries: &[&Entry], cert: &Certificate, hostname: &str) -> HostKeyStatus {
    let ca = cert.signature_key();
    if let Some(revoked) = entries
        .iter()
        .find(|e| e.marker == Marker::Revoked && e.key.key_data() == ca)
    {
        return HostKeyStatus::Revoked {
            entry: revoked.known(),
        };
    }
    let trusted_ca = entries
        .iter()
        .any(|e| e.marker == Marker::CertAuthority && e.key.key_data() == ca);
    if trusted_ca {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let principal_ok = cert.valid_principals().is_empty()
            || cert.valid_principals().iter().any(|p| p == hostname);
        let ca_fp = ca.fingerprint(HashAlg::Sha256);
        if cert.cert_type() == CertType::Host
            && principal_ok
            && cert.validate_at(now, [&ca_fp]).is_ok()
        {
            return HostKeyStatus::Trusted;
        }
    }
    // Not vouched for by a CA on file: judge the certified key itself.
    check_key(entries, cert.public_key())
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::{Algorithm, PrivateKey};

    fn key() -> PrivateKey {
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
    }

    fn line(k: &PrivateKey) -> String {
        k.public_key().to_openssh().unwrap()
    }

    fn presented(k: &PrivateKey) -> PublicKeyOrCertificate {
        k.public_key().clone().into()
    }

    fn hashed(name: &str, salt: &[u8]) -> String {
        let mut mac = Hmac::<Sha1>::new_from_slice(salt).unwrap();
        mac.update(name.as_bytes());
        format!(
            "|1|{}|{}",
            STANDARD.encode(salt),
            STANDARD.encode(mac.finalize().into_bytes())
        )
    }

    #[test]
    fn hashed_and_ported_entries_match_only_their_host() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (key(), key());
        let file = dir.path().join("known_hosts");
        std::fs::write(
            &file,
            format!(
                "{} {}\n[box.lan]:2222 {}\n",
                hashed("web.example", b"0123456789abcdefghij"),
                line(&a),
                line(&b)
            ),
        )
        .unwrap();
        let kh = KnownHosts::new(vec![file.clone()], file);
        assert_eq!(kh.check("web.example", 22, &presented(&a)).unwrap(), HostKeyStatus::Trusted);
        assert_eq!(kh.check("other.example", 22, &presented(&a)).unwrap(), HostKeyStatus::Unknown);
        assert_eq!(kh.check("box.lan", 2222, &presented(&b)).unwrap(), HostKeyStatus::Trusted);
        assert_eq!(kh.check("box.lan", 22, &presented(&b)).unwrap(), HostKeyStatus::Unknown);
    }

    #[test]
    fn changed_key_lists_previous_and_revoked_wins() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new, bad) = (key(), key(), key());
        let file = dir.path().join("kh");
        let global = dir.path().join("global");
        std::fs::write(&file, format!("*.lan,!secret.lan {}\n", line(&old))).unwrap();
        std::fs::write(&global, format!("@revoked * {}\n", line(&bad))).unwrap();
        let kh = KnownHosts::new(vec![file.clone(), global], file.clone());
        match kh.check("db.lan", 22, &presented(&new)).unwrap() {
            HostKeyStatus::Changed { previous } => {
                assert_eq!(previous.len(), 1);
                assert_eq!(previous[0].line, 1);
                assert_eq!(previous[0].fingerprint, fingerprint(old.public_key().key_data()));
            }
            other => panic!("expected Changed, got {other:?}"),
        }
        assert_eq!(kh.check("secret.lan", 22, &presented(&new)).unwrap(), HostKeyStatus::Unknown);
        assert!(matches!(
            kh.check("db.lan", 22, &presented(&bad)).unwrap(),
            HostKeyStatus::Revoked { .. }
        ));
    }

    #[test]
    fn added_key_is_trusted_next_time_and_file_keeps_old_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub/known_hosts");
        let k = key();
        let kh = KnownHosts::new(vec![file.clone()], file.clone());
        assert_eq!(kh.check("10.0.0.5", 2200, &presented(&k)).unwrap(), HostKeyStatus::Unknown);
        kh.add("10.0.0.5", 2200, k.public_key().key_data()).unwrap();
        assert_eq!(kh.check("10.0.0.5", 2200, &presented(&k)).unwrap(), HostKeyStatus::Trusted);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("[10.0.0.5]:2200 ssh-ed25519 "), "{text}");
        // Appending to a file without a trailing newline keeps lines separate.
        std::fs::write(&file, "# no newline").unwrap();
        kh.add("h", 22, k.public_key().key_data()).unwrap();
        assert_eq!(kh.check("h", 22, &presented(&k)).unwrap(), HostKeyStatus::Trusted);
    }

    #[test]
    fn host_certificate_from_listed_ca_is_trusted() {
        use russh::keys::ssh_key::certificate::Builder;
        let dir = tempfile::tempdir().unwrap();
        let (ca, host, rogue_ca) = (key(), key(), key());
        let file = dir.path().join("kh");
        std::fs::write(&file, format!("@cert-authority *.lan {}\n", line(&ca))).unwrap();
        let kh = KnownHosts::new(vec![file.clone()], file);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let cert = |signer: &PrivateKey, principal: &str| {
            let mut b = Builder::new_with_random_nonce(
                &mut rand::rng(),
                host.public_key().key_data().clone(),
                now - 60,
                now + 3600,
            )
            .unwrap();
            b.cert_type(CertType::Host).unwrap();
            b.valid_principal(principal).unwrap();
            PublicKeyOrCertificate::Certificate(b.sign(signer).unwrap())
        };
        assert_eq!(kh.check("db.lan", 22, &cert(&ca, "db.lan")).unwrap(), HostKeyStatus::Trusted);
        assert_eq!(
            kh.check("db.lan", 22, &cert(&ca, "web.lan")).unwrap(),
            HostKeyStatus::Unknown,
            "principal must name the host"
        );
        assert_eq!(
            kh.check("db.lan", 22, &cert(&rogue_ca, "db.lan")).unwrap(),
            HostKeyStatus::Unknown
        );
    }
}
