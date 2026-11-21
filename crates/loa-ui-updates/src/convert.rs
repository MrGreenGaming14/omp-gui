//! Fills the screen's plain [`Data`] from the updater service's snapshot.

use crate::model::{Active, Check, Choice, Held, Installed, Kind, Peer, Product, Report, Signature, Standing};
use crate::Data;
use loa_updater::decision::HoldChoice;
use loa_updater::gates::{GateReport, Verdict};
use loa_updater::peers::{self, PeerStanding};
use loa_updater::{InstalledStatus, Overview, ProductKind, ProductStatus, SignatureStatus};

impl Data {
    /// The screen's data from the updater's [`Overview`], the classified
    /// peers, whether a turn runs and the current Unix time.
    pub fn from_updater(overview: &Overview, peers: &[PeerStanding], turn_running: bool, now_unix: u64) -> Self {
        Self {
            products: overview.products.iter().map(product).collect(),
            github_requests_left: overview.github_rate_limit.map(|limit| limit.remaining),
            peers: peers.iter().map(peer).collect(),
            turn_running,
            now_unix,
        }
    }
}

impl From<Choice> for HoldChoice {
    fn from(choice: Choice) -> Self {
        match choice {
            Choice::UpdateAnyway => Self::UpdateAnyway,
            Choice::PinCurrent => Self::PinCurrent,
            Choice::Wait => Self::Wait,
        }
    }
}

fn product(p: &ProductStatus) -> Product {
    let string = |v: &semver::Version| v.to_string();
    Product {
        id: p.id.clone(),
        label: p.label.clone(),
        kind: match p.kind {
            ProductKind::App => Kind::App,
            ProductKind::Omp => Kind::Omp,
            ProductKind::Harness(_) => Kind::Harness,
        },
        source: p.source.clone(),
        current: p.current.as_ref().map(string),
        installed: match &p.installed {
            InstalledStatus::Unknown => Installed::Unknown,
            InstalledStatus::Running => Installed::Running,
            InstalledStatus::Managed(path) => Installed::Managed(path.display().to_string()),
            InstalledStatus::System(path) => Installed::System(path.display().to_string()),
            InstalledStatus::Missing(reason) => Installed::Missing(reason.clone()),
        },
        offer: p.offer.as_ref().map(|release| release.version.to_string()),
        newest: p.newest.as_ref().map(string),
        changelog: p.changelog.clone(),
        last_check_unix: p.last_check_unix,
        staged: p.staged.iter().map(string).collect(),
        active: p.active.as_ref().map(|active| Active {
            current: active.current.to_string(),
            previous: active.previous.as_ref().map(string),
        }),
        report: p.report.as_ref().map(report),
        held: p.held.as_ref().map(|held| Held { version: held.version.to_string(), harmful: held.report.harmful().count() }),
        pending_swap: p.pending_swap.as_ref().map(string),
        pin: p.pin.as_ref().map(string),
        waiting_past: p.waiting_past.as_ref().map(string),
        signature: p.signature.as_ref().map(|status| match status {
            SignatureStatus::Verified => Signature::Verified,
            SignatureStatus::Unsigned => Signature::Unsigned,
            SignatureStatus::NoTrustedKeys => Signature::NoTrustedKeys,
            SignatureStatus::Rejected(reason) => Signature::Rejected(reason.clone()),
        }),
        restart_required: p.restart_required,
        error: p.error.clone(),
    }
}

fn report(report: &GateReport) -> Report {
    Report {
        from: report.from.to_string(),
        to: report.to.to_string(),
        checks: report
            .checks
            .iter()
            .map(|c| Check { label: c.check.label().to_owned(), pass: c.verdict == Verdict::Pass, details: c.details.clone() })
            .collect(),
    }
}

fn peer(p: &PeerStanding) -> Peer {
    Peer {
        node: p.node.clone(),
        name: p.name.clone(),
        version: p.app_version.as_ref().map(ToString::to_string),
        standing: match p.standing {
            peers::Standing::Current => Standing::Current,
            peers::Standing::Behind { majors, minors, patches } => Standing::Behind { majors, minors, patches },
            peers::Standing::Ahead => Standing::Ahead,
            peers::Standing::Unknown => Standing::Unknown,
        },
        protocols: p.protocols.iter().map(|s| (s.family.clone(), s.shared)).collect(),
        can_talk: p.can_talk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_updater::gates::{CheckResult, GateCheck};
    use loa_updater::peers::{PeerInfo, classify};
    use loa_updater::state::HeldUpdate;
    use loa_updater::model::Active as UpdaterActive;
    use semver::Version;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn overview_and_peers_become_screen_data() {
        let report = GateReport {
            product: "omp".into(),
            from: v("19.0.0"),
            to: v("19.1.0"),
            checks: vec![
                CheckResult::harmful(GateCheck::ToolSchemas, vec!["- edit: tool removed".into()]),
                CheckResult::pass(GateCheck::RpcProtocol, vec!["negotiated: 2".into()]),
            ],
        };
        let status = ProductStatus {
            id: "omp".into(),
            label: "omp".into(),
            kind: ProductKind::Omp,
            source: "npm @oh-my-pi/pi-coding-agent".into(),
            current: Some(v("19.0.0")),
            installed: InstalledStatus::System("/usr/bin/omp".into()),
            offer: None,
            newest: Some(v("19.1.0")),
            changelog: String::new(),
            last_check_unix: Some(10),
            staged: vec![v("19.1.0")],
            active: Some(UpdaterActive { current: v("19.0.0"), previous: Some(v("18.9.0")) }),
            report: Some(report.clone()),
            held: Some(HeldUpdate { version: v("19.1.0"), report, held_at_unix: 5 }),
            pending_swap: None,
            pin: None,
            waiting_past: None,
            signature: Some(SignatureStatus::Rejected("bad key".into())),
            restart_required: false,
            error: None,
        };
        let overview = Overview { products: vec![status], github_rate_limit: None };
        let peers = classify(
            &v("0.2.0"),
            &["omp/control/1", "omp/session/2"],
            &[PeerInfo {
                node: "k3y".into(),
                name: None,
                app_version: Some(v("0.1.0")),
                alpns: vec!["omp/control/1".into(), "omp/session/2".into()],
            }],
        );
        let data = Data::from_updater(&overview, &peers, true, 100);

        let omp = &data.products[0];
        assert_eq!(omp.kind, Kind::Omp);
        assert_eq!(omp.installed, Installed::System("/usr/bin/omp".into()));
        assert_eq!(omp.held, Some(Held { version: "19.1.0".into(), harmful: 1 }));
        assert_eq!(omp.active, Some(Active { current: "19.0.0".into(), previous: Some("18.9.0".into()) }));
        let checks = &omp.report.as_ref().unwrap().checks;
        assert_eq!(checks[0], Check { label: "Tool schemas".into(), pass: false, details: vec!["- edit: tool removed".into()] });
        assert!(checks[1].pass);
        assert_eq!(omp.signature, Some(Signature::Rejected("bad key".into())));
        assert_eq!(data.peers[0].standing, Standing::Behind { majors: 0, minors: 1, patches: 0 });
        assert!(data.peers[0].can_talk);
        assert!(data.turn_running);
        assert_eq!(HoldChoice::from(Choice::Wait), HoldChoice::Wait);
    }
}
