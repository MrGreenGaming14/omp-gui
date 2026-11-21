//! Classifies peers by app version and shared protocol labels for the Peers list.
//!
//! Nodes talk over exact ALPN labels (`omp/<family>/<n>`): a family is shared
//! only when both sides announce the same label.

use semver::Version;
use serde::{Deserialize, Serialize};
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet};

/// Families a node must share with a peer to talk at all.
const REQUIRED_FAMILIES: [&str; 2] = ["control", "session"];

/// What this node knows about a peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    /// Node id (public key).
    pub node: String,
    pub name: Option<String>,
    /// The peer's app version, when it announced one.
    pub app_version: Option<Version>,
    /// ALPN labels the peer accepts.
    pub alpns: Vec<String>,
}

/// A peer's app version relative to this node's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Standing {
    Current,
    /// The peer runs an older version. Only the most significant differing
    /// component is counted: 1.4.2 vs 2.1.0 is `majors: 1`, 2.1.9 vs 2.3.0
    /// is `minors: 2`, 2.3.1 vs 2.3.4 is `patches: 3`. A prerelease of the
    /// same version is behind by zero in each.
    Behind { majors: u64, minors: u64, patches: u64 },
    Ahead,
    /// The peer didn't announce its version.
    Unknown,
}

/// One protocol family as both sides speak it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolStatus {
    pub family: String,
    /// The highest version with an exact label announced by both sides.
    pub shared: Option<u32>,
    /// The highest version this node announces, if any.
    pub local: Option<u32>,
    /// The highest version the peer announces, if any.
    pub peer: Option<u32>,
}

/// One row of the Peers list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerStanding {
    pub node: String,
    pub name: Option<String>,
    pub app_version: Option<Version>,
    pub standing: Standing,
    /// Every family either side speaks, sorted by family.
    pub protocols: Vec<ProtocolStatus>,
    /// True when every family this node speaks, and at least control and
    /// session, has the same label on both sides.
    pub can_talk: bool,
}

/// Splits an `omp/<family>/<n>` ALPN label into family and version.
pub fn parse_alpn(label: &str) -> Option<(&str, u32)> {
    let rest = label.strip_prefix("omp/")?;
    let (family, version) = rest.split_once('/')?;
    if family.is_empty() || version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((family, version.parse().ok()?))
}

/// Classifies `peers` against this node's app version and ALPNs. Sorted
/// for display: peers that can't talk first, then the most behind, then
/// by node id.
pub fn classify(
    local_version: &Version,
    local_alpns: &[impl AsRef<str>],
    peers: &[PeerInfo],
) -> Vec<PeerStanding> {
    let local = families(local_alpns.iter().map(AsRef::<str>::as_ref));
    let mut rows: Vec<PeerStanding> = peers
        .iter()
        .map(|peer| {
            let remote = families(peer.alpns.iter().map(String::as_str));
            let protocols = protocols(&local, &remote);
            let shares = |family: &str| {
                protocols.iter().any(|p| p.family == family && p.shared.is_some())
            };
            let can_talk = REQUIRED_FAMILIES.iter().all(|f| shares(f))
                && local.keys().all(|f| shares(f));
            PeerStanding {
                node: peer.node.clone(),
                name: peer.name.clone(),
                app_version: peer.app_version.clone(),
                standing: standing(local_version, peer.app_version.as_ref()),
                protocols,
                can_talk,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        a.can_talk
            .cmp(&b.can_talk)
            .then_with(|| sort_rank(&a.standing).cmp(&sort_rank(&b.standing)))
            .then_with(|| a.node.cmp(&b.node))
    });
    rows
}

fn families<'a>(labels: impl Iterator<Item = &'a str>) -> BTreeMap<&'a str, BTreeSet<(u32, &'a str)>> {
    let mut families = BTreeMap::<_, BTreeSet<_>>::new();
    for label in labels {
        if let Some((family, version)) = parse_alpn(label) {
            families.entry(family).or_default().insert((version, label));
        }
    }
    families
}

fn protocols(
    local: &BTreeMap<&str, BTreeSet<(u32, &str)>>,
    remote: &BTreeMap<&str, BTreeSet<(u32, &str)>>,
) -> Vec<ProtocolStatus> {
    let names: BTreeSet<&str> = local.keys().chain(remote.keys()).copied().collect();
    names
        .into_iter()
        .map(|family| {
            let (ours, theirs) = (local.get(family), remote.get(family));
            let shared = ours.zip(theirs).and_then(|(ours, theirs)| {
                ours.intersection(theirs).last().map(|(v, _)| *v)
            });
            ProtocolStatus {
                family: family.to_owned(),
                shared,
                local: ours.and_then(|versions| versions.last().map(|(v, _)| *v)),
                peer: theirs.and_then(|versions| versions.last().map(|(v, _)| *v)),
            }
        })
        .collect()
}

fn standing(local: &Version, peer: Option<&Version>) -> Standing {
    let Some(peer) = peer else {
        return Standing::Unknown;
    };
    match peer.cmp_precedence(local) {
        Ordering::Equal => Standing::Current,
        Ordering::Greater => Standing::Ahead,
        Ordering::Less => {
            // The peer has lower precedence, so its first differing
            // component is the smaller one and the subtractions can't wrap.
            let (majors, minors, patches) = if peer.major != local.major {
                (local.major - peer.major, 0, 0)
            } else if peer.minor != local.minor {
                (0, local.minor - peer.minor, 0)
            } else {
                (0, 0, local.patch - peer.patch)
            };
            Standing::Behind { majors, minors, patches }
        }
    }
}

/// Lower sorts first: the most behind, then unknown, current, ahead.
fn sort_rank(standing: &Standing) -> (u8, Reverse<(u64, u64, u64)>) {
    match standing {
        Standing::Behind { majors, minors, patches } => (0, Reverse((*majors, *minors, *patches))),
        Standing::Unknown => (1, Reverse((0, 0, 0))),
        Standing::Current => (2, Reverse((0, 0, 0))),
        Standing::Ahead => (3, Reverse((0, 0, 0))),
    }
}
