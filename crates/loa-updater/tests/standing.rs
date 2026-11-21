//! Hold decisions over the persisted state and peer standing by version and shared ALPNs.

use loa_updater::decision::{HoldChoice, apply_choice, offer_allowed, record};
use loa_updater::gates::{CheckResult, GateCheck, GateReport};
use loa_updater::peers::{PeerInfo, Standing, classify, parse_alpn};
use loa_updater::state::UpdaterState;
use semver::Version;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn report(to: &str, clean: bool) -> GateReport {
    let check = if clean { CheckResult::pass } else { CheckResult::harmful };
    GateReport { product: "omp".into(), from: v("1.0.0"), to: v(to), checks: vec![check(GateCheck::Changelog, vec![])] }
}

#[test]
fn decisions() {
    let mut state = UpdaterState::default();
    assert!(record(&mut state, &report("1.1.0", true), 5));
    assert_eq!(state.pending_swap.get("omp"), Some(&v("1.1.0")));
    // A later harmful report of the same version withdraws the pending swap.
    assert!(!record(&mut state, &report("1.1.0", false), 6));
    assert_eq!((state.pending_swap.get("omp"), state.held["omp"].held_at_unix), (None, 6));

    // Choice -> (swap now, pin, wait), starting from an old pin at 0.9.0.
    let cases = [
        (HoldChoice::UpdateAnyway, Some(v("1.1.0")), None, None),
        (HoldChoice::PinCurrent, None, Some(v("1.0.0")), None),
        (HoldChoice::Wait, None, Some(v("0.9.0")), Some(v("1.1.0"))),
    ];
    for (choice, swap, pin, wait) in cases {
        let mut state = state.clone();
        state.pins.insert("omp".into(), v("0.9.0"));
        assert_eq!(apply_choice(&mut state, "omp", &v("1.0.0"), choice), swap, "{choice:?}");
        assert_eq!((state.pins.get("omp").cloned(), state.waits.get("omp").cloned()), (pin, wait), "{choice:?}");
        assert!(state.held.is_empty() && state.pending_swap.get("omp").cloned() == swap, "{choice:?}");
    }
    assert_eq!(apply_choice(&mut UpdaterState::default(), "omp", &v("1.0.0"), HoldChoice::Wait), None, "nothing held");

    state.waits.insert("omp".into(), v("1.1.0"));
    assert!(!offer_allowed(&state, "omp", &v("1.1.0")) && offer_allowed(&state, "omp", &v("1.2.0")));
    state.pins.insert("omp".into(), v("1.0.0"));
    assert!(!offer_allowed(&state, "omp", &v("1.2.0")));
}

#[test]
fn alpn_labels_parse() {
    assert_eq!((parse_alpn("omp/session/2"), parse_alpn("omp/control/10")), (Some(("session", 2)), Some(("control", 10))));
    for bad in ["h3", "omp/session", "omp/session/x", "omp/session/+1", "omp//1", "other/session/1"] {
        assert_eq!(parse_alpn(bad), None, "{bad}");
    }
}

#[test]
fn peers_sort_by_standing_and_shared_protocols() {
    const LOCAL: [&str; 4] = ["omp/control/1", "omp/session/1", "omp/session/2", "omp/surface/1"];
    let all = ["omp/control/1", "omp/session/1", "omp/surface/1"];
    let peer = |node: &str, version: Option<&str>, alpns: &[&str]| PeerInfo {
        node: node.into(),
        name: None,
        app_version: version.map(v),
        alpns: alpns.iter().map(|s| s.to_string()).collect(),
    };
    let peers = [
        peer("z-current", Some("2.3.4"), &all),
        peer("y-patch", Some("2.3.1"), &all),
        peer("x-major", Some("1.9.9"), &all),
        peer("w-mute", Some("2.3.4"), &["omp/control/1"]),
        peer("s-newer", Some("3.0.0"), &["omp/control/1", "omp/session/3", "omp/surface/1"]),
        peer("v-minor", Some("2.1.0"), &all),
        peer("u-unknown", None, &all),
        peer("t-ahead", Some("3.0.0"), &all),
    ];
    let rows = classify(&v("2.3.4"), &LOCAL, &peers);
    let order: Vec<(&str, bool)> = rows.iter().map(|r| (r.node.as_str(), r.can_talk)).collect();
    assert_eq!(order, [
        ("w-mute", false),
        ("s-newer", false),
        ("x-major", true),
        ("v-minor", true),
        ("y-patch", true),
        ("u-unknown", true),
        ("z-current", true),
        ("t-ahead", true)
    ]);
    let standings: Vec<&Standing> = rows[2..5].iter().map(|r| &r.standing).collect();
    assert_eq!(standings, [
        &Standing::Behind { majors: 1, minors: 0, patches: 0 },
        &Standing::Behind { majors: 0, minors: 2, patches: 0 },
        &Standing::Behind { majors: 0, minors: 0, patches: 3 }
    ]);
    let session = rows[2].protocols.iter().find(|p| p.family == "session").unwrap();
    assert_eq!((session.shared, session.local_max, session.peer_max), (Some(1), Some(2), Some(1)));
    let newer = rows[1].protocols.iter().find(|p| p.family == "session").unwrap();
    assert_eq!((newer.shared, newer.peer_max), (None, Some(3)));
}
