use loa_updater::peers::{PeerInfo, PeerStanding, Standing, classify, parse_alpn};
use semver::Version;

const LOCAL_ALPNS: [&str; 4] = ["omp/control/1", "omp/session/1", "omp/session/2", "omp/surface/1"];

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn peer(node: &str, version: Option<&str>, alpns: &[&str]) -> PeerInfo {
    PeerInfo {
        node: node.into(),
        name: Some(format!("{node} laptop")),
        app_version: version.map(v),
        alpns: alpns.iter().map(|s| s.to_string()).collect(),
    }
}

fn find<'a>(rows: &'a [PeerStanding], node: &str) -> &'a PeerStanding {
    rows.iter().find(|r| r.node == node).unwrap()
}

#[test]
fn alpn_labels_parse() {
    assert_eq!(parse_alpn("omp/session/2"), Some(("session", 2)));
    assert_eq!(parse_alpn("omp/control/10"), Some(("control", 10)));
    assert_eq!(parse_alpn("h3"), None);
    assert_eq!(parse_alpn("omp/session"), None);
    assert_eq!(parse_alpn("omp/session/x"), None);
    assert_eq!(parse_alpn("omp/session/+1"), None);
    assert_eq!(parse_alpn("omp//1"), None);
    assert_eq!(parse_alpn("other/session/1"), None);
}

#[test]
fn peer_behind_by_minor_still_talks_on_older_session() {
    let peers = [peer("b", Some("1.2.5"), &["omp/control/1", "omp/session/1", "omp/surface/1"])];
    let rows = classify(&v("1.4.0"), &LOCAL_ALPNS, &peers);
    let row = &rows[0];
    assert_eq!(row.standing, Standing::Behind { majors: 0, minors: 2, patches: 0 });
    assert!(row.can_talk);
    let session = row.protocols.iter().find(|p| p.family == "session").unwrap();
    assert_eq!(session.shared, Some(1));
    assert_eq!(session.local_max, Some(2));
    assert_eq!(session.peer_max, Some(1));
}

#[test]
fn ahead_current_and_unknown_versions() {
    let all = ["omp/control/1", "omp/session/2", "omp/surface/1"];
    let peers = [
        peer("ahead", Some("1.5.0"), &all),
        peer("same", Some("1.4.0"), &all),
        peer("anon", None, &all),
    ];
    let rows = classify(&v("1.4.0"), &LOCAL_ALPNS, &peers);
    assert_eq!(find(&rows, "ahead").standing, Standing::Ahead);
    assert_eq!(find(&rows, "same").standing, Standing::Current);
    assert_eq!(find(&rows, "anon").standing, Standing::Unknown);
    assert!(rows.iter().all(|r| r.can_talk));
    let session = find(&rows, "same").protocols.iter().find(|p| p.family == "session").unwrap();
    assert_eq!(session.shared, Some(2));
}

#[test]
fn no_shared_session_version_cannot_talk() {
    let peers = [
        peer("new", Some("3.0.0"), &["omp/control/1", "omp/session/3", "omp/surface/1"]),
        peer("nosurface", Some("1.4.0"), &["omp/control/1", "omp/session/2"]),
    ];
    let rows = classify(&v("1.4.0"), &LOCAL_ALPNS, &peers);
    let new = find(&rows, "new");
    assert!(!new.can_talk);
    let session = new.protocols.iter().find(|p| p.family == "session").unwrap();
    assert_eq!(session.shared, None);
    assert_eq!(session.peer_max, Some(3));
    assert!(!find(&rows, "nosurface").can_talk);
}

#[test]
fn sorted_cannot_talk_then_most_behind_then_node() {
    let all = ["omp/control/1", "omp/session/1", "omp/surface/1"];
    let peers = [
        peer("z-current", Some("2.3.4"), &all),
        peer("y-patch", Some("2.3.1"), &all),
        peer("x-major", Some("1.9.9"), &all),
        peer("w-mute", Some("2.3.4"), &["omp/control/1"]),
        peer("v-minor", Some("2.1.0"), &all),
        peer("u-unknown", None, &all),
        peer("a-current", Some("2.3.4"), &all),
        peer("t-ahead", Some("3.0.0"), &all),
    ];
    let rows = classify(&v("2.3.4"), &LOCAL_ALPNS, &peers);
    let order: Vec<&str> = rows.iter().map(|r| r.node.as_str()).collect();
    assert_eq!(order, [
        "w-mute",
        "x-major",
        "v-minor",
        "y-patch",
        "u-unknown",
        "a-current",
        "z-current",
        "t-ahead",
    ]);
    assert_eq!(find(&rows, "x-major").standing, Standing::Behind {
        majors: 1,
        minors: 0,
        patches: 0
    });
    assert_eq!(find(&rows, "y-patch").standing, Standing::Behind {
        majors: 0,
        minors: 0,
        patches: 3
    });
}
