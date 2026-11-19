//! Versioned protocol labels (`omp/<name>/<version>`) and version selection.

use crate::error::{NetError, Result};
use std::fmt;

/// Protocol name of node control traffic: discovery, presence, grants, revocations.
pub const CONTROL: &str = "omp/control";
/// Protocol name of session traffic: prompts, responses, session updates, files.
pub const SESSION: &str = "omp/session";
/// Protocol name of surface frames and input.
pub const SURFACE: &str = "omp/surface";

pub const CONTROL_1: &[u8] = b"omp/control/1";
pub const SESSION_1: &[u8] = b"omp/session/1";
pub const SURFACE_1: &[u8] = b"omp/surface/1";

/// One protocol at one version, e.g. `omp/session/1`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Alpn {
    /// Protocol name without the version, e.g. `omp/session`.
    pub protocol: String,
    pub version: u32,
}

impl Alpn {
    pub fn new(protocol: impl Into<String>, version: u32) -> Self {
        Self { protocol: protocol.into(), version }
    }

    /// The wire label, `<protocol>/<version>`.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.to_string().into_bytes()
    }

    /// Parses a wire label. The version is the decimal after the last `/`.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes).map_err(|_| NetError::InvalidAlpn(String::from_utf8_lossy(bytes).into()))?;
        let invalid = || NetError::InvalidAlpn(text.to_owned());
        let (protocol, version) = text.rsplit_once('/').ok_or_else(invalid)?;
        if protocol.is_empty() || version.starts_with('0') || !version.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        let version = version.parse().map_err(|_| invalid())?;
        Ok(Self::new(protocol, version))
    }
}

impl fmt::Display for Alpn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.protocol, self.version)
    }
}

/// Wire labels a dialer offers for `protocol`: every version it supports,
/// highest first.
pub fn offer(protocol: &str, versions: &[u32]) -> Vec<Vec<u8>> {
    let mut versions = versions.to_vec();
    versions.sort_unstable_by(|a, b| b.cmp(a));
    versions.dedup();
    versions.into_iter().map(|v| Alpn::new(protocol, v).to_bytes()).collect()
}

/// Orders a listener's labels so that TLS ALPN negotiation, which picks the
/// first of the server's labels the client offers, lands on the highest
/// version both sides share.
pub fn server_order(alpns: impl IntoIterator<Item = Alpn>) -> Vec<Vec<u8>> {
    let mut alpns: Vec<Alpn> = alpns.into_iter().collect();
    alpns.sort_by(|a, b| a.protocol.cmp(&b.protocol).then(b.version.cmp(&a.version)));
    alpns.dedup();
    alpns.iter().map(Alpn::to_bytes).collect()
}

/// The highest version in both lists, if any.
pub fn highest_shared(ours: &[u32], theirs: &[u32]) -> Option<u32> {
    ours.iter().filter(|v| theirs.contains(v)).max().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_parse_back_and_reject_garbage() {
        assert_eq!(Alpn::parse(SESSION_1).unwrap(), Alpn::new(SESSION, 1));
        assert_eq!(Alpn::parse(b"omp/session/12").unwrap().version, 12);
        for bad in [&b"omp/session"[..], b"omp/session/", b"/1", b"omp/session/01", b"omp/session/x", b"\xff/1"] {
            assert!(Alpn::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn server_order_puts_highest_version_first_per_protocol() {
        let order = server_order([Alpn::new(SESSION, 1), Alpn::new(CONTROL, 1), Alpn::new(SESSION, 2)]);
        assert_eq!(order, vec![CONTROL_1.to_vec(), b"omp/session/2".to_vec(), SESSION_1.to_vec()]);
        assert_eq!(highest_shared(&[1, 2, 3], &[3, 2, 4]), Some(3));
        assert_eq!(highest_shared(&[1], &[2]), None);
    }
}
