//! Unattended runs: after a timeout nobody answered, a waiting decision gets
//! the default answer set for its kind. The policy is the `unattended` table
//! of the app's central config ([`UnattendedSettings`]): per kind a timeout
//! and an answer; a kind without an entry waits for someone. A decision that
//! carries its own `default` and `timeout_ms` keeps those.

use loa_ui_kit::theme::{SpaceRole as Sp, sp, TextRole as T, text_size as ts};
use crate::decision::{ApprovalChoice, Decision, PlanChoice};
use crate::form::Form;
use iced::alignment::Vertical;
use iced::widget::{column, pick_list, row, text, text_input, toggler};
use iced::{Element, Fill, Length};
use loa_contracts::harness::Answer;
use loa_config::{DecisionDefault, UnattendedSettings};
use loa_core::{DecisionKind, answer};
use serde_json::Value as Json;
use std::fmt;

/// Timeouts the settings offer, in seconds.
pub const TIMEOUTS: [u64; 6] = [30, 60, 120, 300, 900, 1800];

/// Timeout of a kind turned on while no other kind has one.
const DEFAULT_TIMEOUT: u64 = 300;

/// Form answer that submits the defaults the form declares.
const FORM_DEFAULTS: &str = "{}";

/// The unattended policy over the central config's `unattended` table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unattended(pub UnattendedSettings);

impl Unattended {
    /// Whether any kind is answered after a timeout.
    pub fn enabled(&self) -> bool {
        KINDS.into_iter().any(|kind| self.0.for_kind(kind).is_some())
    }

    /// Timeout in ms of decisions of `kind`, when the policy answers them.
    pub fn timeout_ms(&self, kind: DecisionKind) -> Option<i64> {
        self.0
            .for_kind(kind)
            .map(|d| i64::try_from(d.timeout_secs.saturating_mul(1000)).unwrap_or(i64::MAX))
    }

    /// The default answer this policy gives `decision`, recorded with `by`.
    /// `None` when its kind waits for someone or the configured answer
    /// doesn't fit the decision (a hand edited value).
    pub fn answer(&self, decision: &Decision, by: &str) -> Option<Answer> {
        let default = self.0.for_kind(decision.kind)?;
        let configured = default.answer.as_str();
        let waited = || Some(format!("Nobody answered within {}", Duration(default.timeout_secs)));
        let (value, reason) = match decision.kind {
            DecisionKind::Approval => {
                let choice = ApprovalChoice::from_answer(configured)?;
                (configured.to_owned(), (choice == ApprovalChoice::Deny).then(waited).flatten())
            }
            DecisionKind::Plan => {
                let choice = PlanChoice::from_answer(configured).filter(|c| *c != PlanChoice::Edit)?;
                (configured.to_owned(), (choice == PlanChoice::Reject).then(waited).flatten())
            }
            // A configured option label picks that option; anything else is
            // the typed answer.
            DecisionKind::Question => {
                let option = decision.options.iter().find(|o| o.eq_ignore_ascii_case(configured));
                (option.map_or(configured, String::as_str).to_owned(), None)
            }
            DecisionKind::Form => (form_answer(decision, configured)?, None),
        };
        Some(Answer {
            values: vec![value],
            reason: default.reason.clone().or(reason),
            by: by.to_owned(),
        })
    }

    /// Applies a settings edit.
    pub fn update(&mut self, message: Message) {
        match message {
            Message::Enabled(true) => {
                let timeout = self.shared_timeout();
                for (kind, answer) in [
                    (DecisionKind::Approval, answer::DENY),
                    (DecisionKind::Plan, answer::REJECT),
                ] {
                    let slot = slot(&mut self.0, kind);
                    if slot.is_none() {
                        *slot = Some(DecisionDefault {
                            timeout_secs: timeout,
                            answer: answer.to_owned(),
                            reason: None,
                        });
                    }
                }
            }
            Message::Enabled(false) => self.0 = UnattendedSettings::default(),
            Message::Answer(kind, None) => *slot(&mut self.0, kind) = None,
            Message::Answer(kind, Some(answer)) => {
                let timeout = self.shared_timeout();
                let slot = slot(&mut self.0, kind);
                match slot {
                    Some(default) if default.answer == answer => {}
                    Some(default) => {
                        default.answer = answer;
                        default.reason = None;
                    }
                    None => {
                        *slot = Some(DecisionDefault {
                            timeout_secs: timeout,
                            answer,
                            reason: None,
                        });
                    }
                }
            }
            Message::Timeout(kind, Duration(secs)) => {
                if let Some(default) = slot(&mut self.0, kind) {
                    default.timeout_secs = secs.max(1);
                }
            }
        }
    }

    /// The timeout a kind gets when it is turned on: that of another kind,
    /// else five minutes.
    fn shared_timeout(&self) -> u64 {
        KINDS
            .into_iter()
            .find_map(|kind| self.0.for_kind(kind).map(|d| d.timeout_secs))
            .unwrap_or(DEFAULT_TIMEOUT)
    }

    /// Settings section: on/off, then per kind the default answer and its
    /// timeout.
    pub fn view(&self) -> Element<'_, Message> {
        let mut section = column![
            row![
                text("Answer waiting prompts after a timeout").size(ts(T::Label)).width(Fill),
                toggler(self.enabled()).on_toggle(Message::Enabled).size(ts(T::Heading)),
            ]
            .spacing(sp(Sp::M))
            .align_y(Vertical::Center),
            text(
                "For unattended runs: a prompt nobody answers in time gets the \
                 default set for its kind, recorded as answered by timeout. \
                 A kind set to wait is never answered for you."
            )
            .size(ts(T::Caption))
            .style(loa_ui_kit::style::text(loa_ui_kit::Tone::Muted)),
        ]
        .spacing(sp(Sp::M));
        for kind in KINDS {
            section = section.push(self.kind_row(kind));
        }
        section.into()
    }

    fn kind_row(&self, kind: DecisionKind) -> Element<'_, Message> {
        let default = self.0.for_kind(kind);
        let control: Element<'_, Message> = if kind == DecisionKind::Question {
            text_input("Option or typed answer; empty waits", default.map_or("", |d| d.answer.as_str()))
                .on_input(|typed| Message::Answer(DecisionKind::Question, (!typed.trim().is_empty()).then_some(typed)))
                .size(ts(T::Label))
                .padding([sp(Sp::Xs), sp(Sp::S)])
                .style(loa_ui_kit::style::input)
                .width(Length::Fixed(CONTROL_WIDTH))
                .into()
        } else {
            let choices = choices(kind);
            let selected = default.map(|d| d.answer.as_str());
            let current = choices.iter().find(|c| c.answer.as_deref() == selected).cloned().unwrap_or(Choice {
                answer: selected.map(str::to_owned),
                label: "Value from the config file",
            });
            pick_list(choices, Some(current), move |choice: Choice| Message::Answer(kind, choice.answer))
                .text_size(13)
                .style(loa_ui_kit::style::pick)
                .width(Length::Fixed(CONTROL_WIDTH))
                .into()
        };
        let mut line = row![text(kind_label(kind)).size(ts(T::Label)).width(Fill), control]
            .spacing(sp(Sp::M))
            .align_y(Vertical::Center);
        if let Some(default) = default {
            let mut timeouts: Vec<Duration> = TIMEOUTS.map(Duration).to_vec();
            if !TIMEOUTS.contains(&default.timeout_secs) {
                timeouts.push(Duration(default.timeout_secs));
            }
            line = line.push(
                pick_list(timeouts, Some(Duration(default.timeout_secs)), move |d| Message::Timeout(kind, d))
                    .text_size(13)
                    .style(loa_ui_kit::style::pick)
                    .width(Length::Fixed(TIMEOUT_WIDTH)),
            );
        } else {
            line = line.push(iced::widget::space().width(Length::Fixed(TIMEOUT_WIDTH)));
        }
        line.into()
    }
}

const KINDS: [DecisionKind; 4] = [
    DecisionKind::Approval,
    DecisionKind::Question,
    DecisionKind::Plan,
    DecisionKind::Form,
];

const CONTROL_WIDTH: f32 = 240.0;
const TIMEOUT_WIDTH: f32 = 96.0;

fn slot(settings: &mut UnattendedSettings, kind: DecisionKind) -> &mut Option<DecisionDefault> {
    match kind {
        DecisionKind::Approval => &mut settings.approval,
        DecisionKind::Question => &mut settings.question,
        DecisionKind::Plan => &mut settings.plan,
        DecisionKind::Form => &mut settings.form,
    }
}

fn kind_label(kind: DecisionKind) -> &'static str {
    match kind {
        DecisionKind::Approval => "Approvals",
        DecisionKind::Question => "Questions",
        DecisionKind::Plan => "Plan reviews",
        DecisionKind::Form => "Forms",
    }
}

/// The answers the settings offer for `kind`, waiting first.
fn choices(kind: DecisionKind) -> Vec<Choice> {
    let wait = Choice {
        answer: None,
        label: "Wait for someone",
    };
    let answers: Vec<(&str, &'static str)> = match kind {
        DecisionKind::Approval => ApprovalChoice::ALL
            .into_iter()
            .rev()
            .map(|c| (c.answer(), c.label()))
            .collect(),
        DecisionKind::Plan => [PlanChoice::Reject, PlanChoice::Approve]
            .into_iter()
            .map(|c| (c.answer(), c.label()))
            .collect(),
        DecisionKind::Form => vec![(FORM_DEFAULTS, "Submit the form's defaults")],
        DecisionKind::Question => Vec::new(),
    };
    std::iter::once(wait)
        .chain(answers.into_iter().map(|(answer, label)| Choice {
            answer: Some(answer.to_owned()),
            label,
        }))
        .collect()
}

/// The form answer for a configured JSON object: the form's declared
/// defaults with the object's values over them.
fn form_answer(decision: &Decision, configured: &str) -> Option<String> {
    let Ok(Json::Object(overrides)) = serde_json::from_str::<Json>(configured) else {
        return None;
    };
    let mut values = decision
        .fields
        .as_deref()
        .and_then(|schema| Form::parse(schema).ok())
        .and_then(|mut form| form.submit())
        .and_then(|json| match serde_json::from_str::<Json>(&json) {
            Ok(Json::Object(defaults)) => Some(defaults),
            _ => None,
        })
        .unwrap_or_default();
    values.extend(overrides);
    Some(Json::Object(values).to_string())
}

/// A timeout shown as "30 s", "2 min".
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Duration(pub u64);

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            s if s >= 60 && s % 60 == 0 => write!(f, "{} min", s / 60),
            s => write!(f, "{s} s"),
        }
    }
}

/// A default answer with its settings label; `None` waits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub answer: Option<String>,
    label: &'static str,
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label)
    }
}

/// Settings edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// On: approvals are denied and plans rejected after five minutes (kinds
    /// already set keep their answers). Off: every kind waits.
    Enabled(bool),
    /// The default answer of a kind; `None` waits for someone.
    Answer(DecisionKind, Option<String>),
    Timeout(DecisionKind, Duration),
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{DecisionStatus, NodeId};

    fn decision(kind: DecisionKind) -> Decision {
        Decision {
            id: NodeId(7),
            kind,
            status: DecisionStatus::Waiting,
            title: None,
            body: String::new(),
            options: vec!["red".into(), "green".into()],
            multi: false,
            answer: Vec::new(),
            by: None,
            reason: None,
            fields: Some(
                r#"{"properties":{"env":{"enum":["dev","prod"],"default":"prod"},"n":{"type":"integer"}},"required":["env"]}"#
                    .into(),
            ),
            default: Vec::new(),
            timeout_ms: None,
            created: None,
            tool: None,
            harness: None,
        }
    }

    #[test]
    fn safe_defaults_deny_and_reject_while_questions_and_forms_wait() {
        let mut policy = Unattended::default();
        assert!(!policy.enabled());
        policy.update(Message::Enabled(true));
        assert!(policy.enabled());
        assert_eq!(policy.timeout_ms(DecisionKind::Approval), Some(300_000));
        let deny = policy.answer(&decision(DecisionKind::Approval), "timeout").unwrap();
        assert_eq!(deny.values, [answer::DENY]);
        assert_eq!(deny.reason.as_deref(), Some("Nobody answered within 5 min"));
        assert_eq!(deny.by, "timeout");
        let reject = policy.answer(&decision(DecisionKind::Plan), "t").unwrap();
        assert_eq!(reject.values, [answer::REJECT]);
        assert_eq!(policy.timeout_ms(DecisionKind::Question), None);
        assert!(policy.answer(&decision(DecisionKind::Question), "t").is_none());
        assert!(policy.answer(&decision(DecisionKind::Form), "t").is_none());
        policy.update(Message::Enabled(false));
        assert_eq!(policy, Unattended::default());
    }

    #[test]
    fn chosen_answers_and_timeouts_per_kind() {
        let mut policy = Unattended::default();
        policy.update(Message::Answer(DecisionKind::Approval, Some(answer::ALLOW_SESSION.into())));
        policy.update(Message::Timeout(DecisionKind::Approval, Duration(30)));
        policy.update(Message::Answer(DecisionKind::Question, Some("GREEN".into())));
        policy.update(Message::Answer(DecisionKind::Plan, Some(answer::APPROVE.into())));
        policy.update(Message::Answer(DecisionKind::Form, Some(r#"{"n":3}"#.into())));
        assert_eq!(policy.timeout_ms(DecisionKind::Approval), Some(30_000));
        // Kinds turned on later take the timeout already chosen.
        assert_eq!(policy.timeout_ms(DecisionKind::Form), Some(30_000));
        let approval = policy.answer(&decision(DecisionKind::Approval), "t").unwrap();
        assert_eq!((approval.values.as_slice(), approval.reason), ([answer::ALLOW_SESSION.to_owned()].as_slice(), None));
        assert_eq!(policy.answer(&decision(DecisionKind::Question), "t").unwrap().values, ["green"]);
        policy.update(Message::Answer(DecisionKind::Question, Some("blue".into())));
        assert_eq!(policy.answer(&decision(DecisionKind::Question), "t").unwrap().values, ["blue"], "typed");
        assert_eq!(policy.answer(&decision(DecisionKind::Plan), "t").unwrap().values, [answer::APPROVE]);
        assert_eq!(
            policy.answer(&decision(DecisionKind::Form), "t").unwrap().values,
            [r#"{"env":"prod","n":3}"#]
        );
        policy.update(Message::Answer(DecisionKind::Form, Some(FORM_DEFAULTS.into())));
        assert_eq!(policy.answer(&decision(DecisionKind::Form), "t").unwrap().values, [r#"{"env":"prod"}"#]);
        policy.update(Message::Answer(DecisionKind::Plan, None));
        assert_eq!(policy.timeout_ms(DecisionKind::Plan), None);
    }

    #[test]
    fn settings_are_the_central_config_table() {
        let mut policy = Unattended::default();
        policy.update(Message::Enabled(true));
        policy.update(Message::Answer(DecisionKind::Form, Some(FORM_DEFAULTS.into())));
        let mut config = loa_config::Config::default();
        config.unattended = policy.0.clone();
        config.validate().expect("the config accepts what the settings write");
        let dir = tempfile::tempdir().unwrap();
        let store = loa_config::ConfigStore::in_dir(dir.path());
        store.save(&config).unwrap();
        assert_eq!(Unattended(store.load().unwrap().unattended), policy);
    }

    #[test]
    fn hand_edited_answers_that_do_not_fit_are_not_applied() {
        let mut policy = Unattended::default();
        policy.0.approval = Some(DecisionDefault { timeout_secs: 5, answer: "yes".into(), reason: None });
        policy.0.plan = Some(DecisionDefault { timeout_secs: 5, answer: answer::EDIT.into(), reason: None });
        policy.0.form = Some(DecisionDefault { timeout_secs: 5, answer: "[1]".into(), reason: None });
        for kind in [DecisionKind::Approval, DecisionKind::Plan, DecisionKind::Form] {
            assert!(policy.answer(&decision(kind), "t").is_none(), "{kind:?}");
        }
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(Duration(30).to_string(), "30 s");
        assert_eq!(Duration(120).to_string(), "2 min");
        assert_eq!(Duration(90).to_string(), "90 s");
    }
}
