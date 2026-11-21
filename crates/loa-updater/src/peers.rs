//! Classifies peers by app version and shared protocol labels for the Peers list.
//!
//! Nodes talk over exact ALPN labels (`omp/<family>/<n>`): a family is shared
//! only when both sides announce the same label.

use loa_settings::updates::{PeerInfo, PeerStanding, ProtocolStatus, Standing};
use semver::Version;
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet};

/// Families a node must share with a peer to talk at all.
const REQUIRED_FAMILIES: [&str; 2] = ["control", "session"];

/// Splits an `omp/<family>/<n>` label into family and version.
pub fn parse_alpn(label: &str) -> Option<(&str, u32)> {
    let (family, version) = label.strip_prefix("omp/")?.split_once('/')?;
    let digits = !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit());
    (!family.is_empty() && digits).then_some(())?;
    Some((family, version.parse().ok()?))
}

/// Classifies `peers`; sorted with peers that can't talk first, then the most behind, then by node.
pub fn classify(local_version: &Version, local_alpns: &[impl AsRef<str>], peers: &[PeerInfo]) -> Vec<PeerStanding> {
    let local = families(local_alpns.iter().map(AsRef::as_ref));
    let mut rows: Vec<PeerStanding> = peers
        .iter()
        .map(|peer| {
            let remote = families(peer.alpns.iter().map(String::as_str));
            let protocols = protocols(&local, &remote);
            let shares = |family: &str| protocols.iter().any(|p| p.family == family && p.shared.is_some());
            let can_talk = REQUIRED_FAMILIES.iter().all(|f| shares(f)) && local.keys().all(|f| shares(f));
            let standing = standing(local_version, peer.app_version.as_ref());
            PeerStanding { node: peer.node.clone(), name: peer.name.clone(), app_version: peer.app_version.clone(), standing, protocols, can_talk }
        })
        .collect();
    rows.sort_by(|a, b| (a.can_talk, rank(&a.standing), &a.node).cmp(&(b.can_talk, rank(&b.standing), &b.node)));
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
    let Some(peer) = peer else { return Standing::Unknown };
    match peer.cmp_precedence(local) {
        Ordering::Equal => Standing::Current,
        Ordering::Greater => Standing::Ahead,
        // The first differing component of the peer is the smaller one, so nothing wraps.
        Ordering::Less if peer.major != local.major => Standing::Behind { majors: local.major - peer.major, minors: 0, patches: 0 },
        Ordering::Less if peer.minor != local.minor => Standing::Behind { majors: 0, minors: local.minor - peer.minor, patches: 0 },
        Ordering::Less => Standing::Behind { majors: 0, minors: 0, patches: local.patch - peer.patch },
    }
}

/// Lower sorts first: the most behind, then unknown, current, ahead.
fn rank(standing: &Standing) -> (u8, Reverse<(u64, u64, u64)>) {
    match standing {
        Standing::Behind { majors, minors, patches } => (0, Reverse((*majors, *minors, *patches))),
        Standing::Unknown => (1, Reverse((0, 0, 0))),
        Standing::Current => (2, Reverse((0, 0, 0))),
        Standing::Ahead => (3, Reverse((0, 0, 0))),
    }
}
