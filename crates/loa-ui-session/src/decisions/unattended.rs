//! Unattended runs: a waiting decision nobody answers within its kind's timeout gets the
//! default answer set for that kind. The policy is the central config's `unattended` table; a
//! kind without an entry waits, and a decision carrying its own default keeps it.

use super::card::kind_for;
use super::model::Decision;
use loa_config::{DecisionDefault, UnattendedSettings};
use loa_contracts::harness::Answer;
use loa_core::{DecisionKind, answer};
use std::fmt;

/// Timeouts the settings offer, in seconds.
pub const TIMEOUTS: [u64; 6] = [30, 60, 120, 300, 900, 1800];
/// Timeout of a kind turned on while no other kind has one.
const DEFAULT_TIMEOUT: u64 = 300;
pub(crate) const KINDS: [DecisionKind; 4] = [DecisionKind::Approval, DecisionKind::Question, DecisionKind::Plan, DecisionKind::Form];

/// The unattended policy over the central config's `unattended` table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unattended(pub UnattendedSettings);

/// Settings edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// On: approvals are denied and plans rejected after five minutes (kinds already set keep
    /// their answers). Off: every kind waits.
    Enabled(bool),
    /// The default answer of a kind; `None` waits for someone.
    Answer(DecisionKind, Option<String>),
    Timeout(DecisionKind, Duration),
}

/// A timeout shown as "30 s", "2 min".
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Duration(pub u64);

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { if self.0 >= 60 && self.0.is_multiple_of(60) { write!(f, "{} min", self.0 / 60) } else { write!(f, "{} s", self.0) } }
}

impl Unattended {
    /// Whether any kind is answered after a timeout.
    pub fn enabled(&self) -> bool { KINDS.into_iter().any(|kind| self.0.for_kind(kind).is_some()) }
    /// Timeout in ms of decisions of `kind`, when the policy answers them.
    pub fn timeout_ms(&self, kind: DecisionKind) -> Option<i64> { self.0.for_kind(kind).map(|d| i64::try_from(d.timeout_secs.saturating_mul(1000)).unwrap_or(i64::MAX)) }

    /// The default answer this policy gives `decision`, recorded with `by`; `None` when its kind
    /// waits or the configured answer doesn't fit (a hand-edited value).
    pub fn answer(&self, decision: &Decision, by: &str) -> Option<Answer> {
        let default = self.0.for_kind(decision.kind)?;
        let (value, refuses) = kind_for(decision.kind).policy(decision, &default.answer)?;
        let waited = refuses.then(|| format!("Nobody answered within {}", Duration(default.timeout_secs)));
        Some(Answer { values: vec![value], reason: default.reason.clone().or(waited), by: by.to_owned() })
    }

    pub fn update(&mut self, message: Message) {
        let timeout = KINDS.into_iter().find_map(|kind| self.0.for_kind(kind).map(|d| d.timeout_secs)).unwrap_or(DEFAULT_TIMEOUT);
        let fresh = |answer: &str| DecisionDefault { timeout_secs: timeout, answer: answer.to_owned(), reason: None };
        match message {
            Message::Enabled(true) => {
                for (kind, answer) in [(DecisionKind::Approval, answer::DENY), (DecisionKind::Plan, answer::REJECT)] {
                    slot(&mut self.0, kind).get_or_insert_with(|| fresh(answer));
                }
            }
            Message::Enabled(false) => self.0 = UnattendedSettings::default(),
            Message::Answer(kind, None) => *slot(&mut self.0, kind) = None,
            Message::Answer(kind, Some(answer)) => match slot(&mut self.0, kind) {
                Some(default) if default.answer == answer => {}
                Some(default) => (default.answer, default.reason) = (answer, None),
                empty => *empty = Some(fresh(&answer)),
            },
            Message::Timeout(kind, Duration(secs)) => {
                if let Some(default) = slot(&mut self.0, kind) {
                    default.timeout_secs = secs.max(1);
                }
            }
        }
    }
}

fn slot(settings: &mut UnattendedSettings, kind: DecisionKind) -> &mut Option<DecisionDefault> {
    match kind {
        DecisionKind::Approval => &mut settings.approval,
        DecisionKind::Question => &mut settings.question,
        DecisionKind::Plan => &mut settings.plan,
        DecisionKind::Form => &mut settings.form,
    }
}
