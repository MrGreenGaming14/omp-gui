//! The updater's products and peer standings as [`loa_view::settings::updates`] views, and the
//! answer to a held update back as the updater's choice.

use loa_settings::updates::{
    CheckResult, GateCheck, GateReport, HoldChoice, InstalledStatus, PeerStanding, ProductKind, ProductStatus, SignatureStatus, Standing, Verdict,
};
use loa_view::settings::updates as view;

fn report_to_view(r: &GateReport) -> view::GateReport {
    let check = |c: &CheckResult| view::CheckResult {
        check: match c.check {
            GateCheck::ToolSchemas => view::GateCheck::ToolSchemas,
            GateCheck::SessionFormat => view::GateCheck::SessionFormat,
            GateCheck::ConfigSchema => view::GateCheck::ConfigSchema,
            GateCheck::RpcProtocol => view::GateCheck::RpcProtocol,
            GateCheck::ContractTests => view::GateCheck::ContractTests,
            GateCheck::Changelog => view::GateCheck::Changelog,
        },
        verdict: match c.verdict {
            Verdict::Pass => view::Verdict::Pass,
            Verdict::Harmful => view::Verdict::Harmful,
        },
        details: c.details.clone(),
    };
    view::GateReport { from: r.from.to_string(), to: r.to.to_string(), checks: r.checks.iter().map(check).collect() }
}

pub fn product_to_view(p: &ProductStatus) -> view::Product {
    let version = |v: &semver::Version| v.to_string();
    view::Product {
        id: p.id.clone(),
        label: p.label.clone(),
        kind: match &p.kind {
            ProductKind::App => view::ProductKind::App,
            ProductKind::Omp => view::ProductKind::Omp,
            ProductKind::Harness(harness) => view::ProductKind::Harness(harness.clone()),
        },
        source: p.source.clone(),
        current: p.current.as_ref().map(version),
        installed: match &p.installed {
            InstalledStatus::Unknown => view::Installed::Unknown,
            InstalledStatus::Running => view::Installed::Running,
            InstalledStatus::Managed(path) => view::Installed::Managed(path.display().to_string()),
            InstalledStatus::System(path) => view::Installed::System(path.display().to_string()),
            InstalledStatus::Missing(reason) => view::Installed::Missing(reason.clone()),
        },
        offer: p.offer.as_ref().map(|release| version(&release.version)),
        newest: p.newest.as_ref().map(version),
        changelog: p.changelog.clone(),
        last_check_unix: p.last_check_unix,
        staged: p.staged.iter().map(version).collect(),
        active: p.active.as_ref().map(|a| view::Active { current: version(&a.current), previous: a.previous.as_ref().map(version) }),
        report: p.report.as_ref().map(report_to_view),
        held: p.held.as_ref().map(|h| view::HeldUpdate { version: version(&h.version), report: report_to_view(&h.report) }),
        pending_swap: p.pending_swap.as_ref().map(version),
        pin: p.pin.as_ref().map(version),
        waiting_past: p.waiting_past.as_ref().map(version),
        signature: p.signature.as_ref().map(|s| match s {
            SignatureStatus::Verified => view::SignatureStatus::Verified,
            SignatureStatus::Unsigned => view::SignatureStatus::Unsigned,
            SignatureStatus::NoTrustedKeys => view::SignatureStatus::NoTrustedKeys,
            SignatureStatus::Rejected(reason) => view::SignatureStatus::Rejected(reason.clone()),
        }),
        restart_required: p.restart_required,
        error: p.error.clone(),
    }
}

pub fn peer_to_view(p: &PeerStanding) -> view::PeerStanding {
    view::PeerStanding {
        node: p.node.clone(),
        name: p.name.clone(),
        app_version: p.app_version.as_ref().map(ToString::to_string),
        standing: match p.standing {
            Standing::Current => view::Standing::Current,
            Standing::Behind { majors, minors, patches } => view::Standing::Behind { majors, minors, patches },
            Standing::Ahead => view::Standing::Ahead,
            Standing::Unknown => view::Standing::Unknown,
        },
        protocols: p.protocols.iter().map(|s| view::ProtocolStatus { family: s.family.clone(), shared: s.shared }).collect(),
        can_talk: p.can_talk,
    }
}

pub fn hold_choice_from_view(choice: view::HoldChoice) -> HoldChoice {
    match choice {
        view::HoldChoice::UpdateAnyway => HoldChoice::UpdateAnyway,
        view::HoldChoice::PinCurrent => HoldChoice::PinCurrent,
        view::HoldChoice::Wait => HoldChoice::Wait,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_choices_map_one_to_one() {
        let pairs = [(view::HoldChoice::UpdateAnyway, HoldChoice::UpdateAnyway), (view::HoldChoice::PinCurrent, HoldChoice::PinCurrent), (view::HoldChoice::Wait, HoldChoice::Wait)];
        for (shown, choice) in pairs {
            assert_eq!(hold_choice_from_view(shown), choice);
        }
    }

    #[test]
    fn products_keep_what_the_screen_shows() {
        let v = |s: &str| semver::Version::parse(s).unwrap();
        let report = GateReport { product: "omp".into(), from: v("19.0.0"), to: v("19.1.0"), checks: vec![CheckResult::harmful(GateCheck::ToolSchemas, vec!["- edit: tool removed".into()])] };
        let product = ProductStatus {
            id: "omp".into(), label: "omp".into(), kind: ProductKind::Omp, source: "GitHub can1357/oh-my-pi".into(), current: Some(v("19.0.0")),
            installed: InstalledStatus::Managed("/opt/omp".into()), offer: None, newest: Some(v("19.1.0")), changelog: "- faster".into(), last_check_unix: Some(5),
            staged: vec![v("19.1.0")], active: None, report: Some(report.clone()), held: None, pending_swap: None, pin: Some(v("19.0.0")), waiting_past: None,
            signature: Some(SignatureStatus::Rejected("bad key".into())), restart_required: true, error: Some("feed down".into()),
        };
        let shown = product_to_view(&product);
        assert_eq!((shown.current.as_deref(), shown.staged, shown.pin.as_deref()), (Some("19.0.0"), vec!["19.1.0".to_owned()], Some("19.0.0")));
        assert_eq!(shown.installed, view::Installed::Managed("/opt/omp".into()));
        let shown_report = shown.report.unwrap();
        assert_eq!((shown_report.to.as_str(), shown_report.harmful().count()), ("19.1.0", 1));
    }
}
