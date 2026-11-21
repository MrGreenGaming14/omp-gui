//! The session screen's projections: the unattended policy between the central config and
//! `loa_view::session::decisions`.

use loa_settings::app::{DecisionDefault, UnattendedSettings};
use loa_view::session::decisions::{DecisionDefaultView, UnattendedView};

pub fn unattended_to_view(settings: &UnattendedSettings) -> UnattendedView {
    let view = |d: &Option<DecisionDefault>| d.as_ref().map(|d| DecisionDefaultView { timeout_secs: d.timeout_secs, answer: d.answer.clone(), reason: d.reason.clone() });
    UnattendedView { approval: view(&settings.approval), question: view(&settings.question), plan: view(&settings.plan), form: view(&settings.form) }
}

pub fn unattended_from_view(view: UnattendedView) -> UnattendedSettings {
    let settings = |d: Option<DecisionDefaultView>| d.map(|d| DecisionDefault { timeout_secs: d.timeout_secs, answer: d.answer, reason: d.reason });
    UnattendedSettings { approval: settings(view.approval), question: settings(view.question), plan: settings(view.plan), form: settings(view.form) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unattended_policy_round_trips() {
        let default = |timeout_secs, answer: &str, reason: Option<&str>| Some(DecisionDefault { timeout_secs, answer: answer.into(), reason: reason.map(str::to_owned) });
        let settings = UnattendedSettings {
            approval: default(300, "deny", Some("nobody around")),
            question: default(60, "green", None),
            plan: None,
            form: default(1800, r#"{"env":"prod"}"#, None),
        };
        assert_eq!(unattended_from_view(unattended_to_view(&settings)), settings);
        assert_eq!(unattended_from_view(unattended_to_view(&UnattendedSettings::default())), UnattendedSettings::default());
    }
}
