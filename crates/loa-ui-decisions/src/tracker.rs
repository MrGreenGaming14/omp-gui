//! Waiting decisions across every open session: badge counts, system
//! notifications while the app is in the background, and unattended
//! timeouts. Pure: time is passed in and notifications go through the
//! [`Notifier`] port.

use crate::decision::Decision;
use loa_adapter::Answer;
use loa_core::{DecisionKind, Dom, NodeId, attr};
use std::collections::{BTreeMap, HashMap};

/// `by` of an answer applied because an unattended timeout passed.
pub const TIMEOUT_BY: &str = "timeout";

/// A system notification about a session that needs input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// `<session id>`.
    pub session: String,
    /// `<session title>`, when set.
    pub session_title: Option<String>,
    pub decision: NodeId,
    pub kind: DecisionKind,
    /// One line about what is asked: tool and command, question, title.
    pub summary: String,
}

impl Notice {
    /// Notification heading.
    pub fn heading(&self) -> String {
        let session = self.session_title.as_deref().unwrap_or("A session");
        let what = match self.kind {
            DecisionKind::Approval => "needs approval",
            DecisionKind::Question => "has a question",
            DecisionKind::Plan => "has a plan to review",
            DecisionKind::Form => "needs a form filled in",
        };
        format!("{session} {what}")
    }
}

#[derive(Debug, thiserror::Error)]
#[error("system notification failed: {0}")]
pub struct NotifyError(pub String);

/// Raises system notifications. The app passes [`DesktopNotifier`]; tests
/// pass a recorder.
pub trait Notifier {
    fn notify(&self, notice: &Notice) -> Result<(), NotifyError>;
}

/// Desktop notifications through `notify-rust` (D-Bus on Linux, pure Rust).
/// Each call blocks for one round trip to the notification daemon.
#[derive(Clone, Debug)]
pub struct DesktopNotifier {
    app_name: String,
}

impl DesktopNotifier {
    pub fn new(app_name: impl Into<String>) -> Self {
        Self {
            app_name: app_name.into(),
        }
    }
}

impl Notifier for DesktopNotifier {
    fn notify(&self, notice: &Notice) -> Result<(), NotifyError> {
        notify_rust::Notification::new()
            .appname(&self.app_name)
            .summary(&notice.heading())
            .body(&notice.summary)
            .show()
            .map(drop)
            .map_err(|e| NotifyError(e.to_string()))
    }
}

/// An unattended timeout that passed: hand `answer` to the session's
/// `HarnessSession::answer` for `decision`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expired {
    pub session: String,
    pub decision: NodeId,
    pub answer: Answer,
}

#[derive(Debug)]
struct Waiting {
    /// Unix ms when the default answer applies.
    deadline: Option<i64>,
    default: Option<Answer>,
    fired: bool,
}

#[derive(Debug, Default)]
struct Session {
    waiting: BTreeMap<NodeId, Waiting>,
}

/// Tracks the waiting decisions of every session the app has open.
pub struct Tracker<N> {
    notifier: N,
    background: bool,
    sessions: HashMap<String, Session>,
}

impl<N: Notifier> Tracker<N> {
    pub fn new(notifier: N) -> Self {
        Self {
            notifier,
            background: false,
            sessions: HashMap::new(),
        }
    }

    /// Whether the app window is in the background. Notifications are only
    /// raised while it is.
    pub fn set_background(&mut self, background: bool) {
        self.background = background;
    }

    /// Reads the waiting decisions of the session in `dom` (keyed by
    /// `<session id>`) at `now_ms`. Every decision seen waiting for the first
    /// time while the app is in the background raises one notification.
    /// The state is fully updated even when a notification fails; the first
    /// failure is returned.
    pub fn observe(&mut self, dom: &Dom, now_ms: i64) -> Result<(), NotifyError> {
        let Some(root) = dom.root() else {
            return Ok(());
        };
        let Some(session_id) = root.str_attr(attr::ID) else {
            return Ok(());
        };
        let session_title = root.str_attr(attr::TITLE);
        let session = self.sessions.entry(session_id.to_owned()).or_default();
        let mut still = BTreeMap::new();
        let mut result = Ok(());
        for decision in Decision::all(dom).filter(Decision::is_waiting) {
            let id = decision.id;
            let waiting = match session.waiting.remove(&id) {
                Some(known) => known,
                None => {
                    if self.background {
                        let notice = Notice {
                            session: session_id.to_owned(),
                            session_title: session_title.map(str::to_owned),
                            decision: id,
                            kind: decision.kind,
                            summary: summary(&decision),
                        };
                        if let Err(e) = self.notifier.notify(&notice)
                            && result.is_ok()
                        {
                            result = Err(e);
                        }
                    }
                    let default = decision.default_answer(TIMEOUT_BY);
                    let deadline = decision
                        .timeout_ms
                        .filter(|_| default.is_some())
                        .map(|t| decision.created.unwrap_or(now_ms).saturating_add(t));
                    Waiting {
                        deadline,
                        default,
                        fired: false,
                    }
                }
            };
            still.insert(id, waiting);
        }
        session.waiting = still;
        result
    }

    /// Drops a closed session.
    pub fn forget(&mut self, session: &str) {
        self.sessions.remove(session);
    }

    /// Number of waiting decisions in `session`, for its sidebar and top bar
    /// badge.
    pub fn badge(&self, session: &str) -> usize {
        self.sessions.get(session).map_or(0, |s| s.waiting.len())
    }

    /// Sessions with at least one waiting decision and their counts.
    pub fn badges(&self) -> impl Iterator<Item = (&str, usize)> {
        self.sessions
            .iter()
            .filter(|(_, s)| !s.waiting.is_empty())
            .map(|(id, s)| (id.as_str(), s.waiting.len()))
    }

    /// Waiting decisions across every session.
    pub fn total(&self) -> usize {
        self.sessions.values().map(|s| s.waiting.len()).sum()
    }

    /// Default answers whose timeout has passed at `now_ms`. Each decision
    /// is returned once.
    pub fn tick(&mut self, now_ms: i64) -> Vec<Expired> {
        let mut expired = Vec::new();
        for (session, state) in &mut self.sessions {
            for (id, waiting) in &mut state.waiting {
                if waiting.fired || waiting.deadline.is_none_or(|d| d > now_ms) {
                    continue;
                }
                if let Some(answer) = waiting.default.clone() {
                    waiting.fired = true;
                    expired.push(Expired {
                        session: session.clone(),
                        decision: *id,
                        answer,
                    });
                }
            }
        }
        expired.sort_by(|a, b| (&a.session, a.decision).cmp(&(&b.session, b.decision)));
        expired
    }

    /// The earliest pending timeout, so the app can schedule the next
    /// [`tick`](Self::tick).
    pub fn next_deadline(&self) -> Option<i64> {
        self.sessions
            .values()
            .flat_map(|s| s.waiting.values())
            .filter(|w| !w.fired)
            .filter_map(|w| w.deadline)
            .min()
    }
}

fn summary(decision: &Decision) -> String {
    let first_line = decision
        .body
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let lead = match (decision.kind, &decision.tool, &decision.title) {
        (DecisionKind::Approval, Some(tool), _) => Some(tool.as_str()),
        (_, _, Some(title)) => Some(title.as_str()),
        _ => None,
    };
    match lead {
        Some(lead) if first_line.is_empty() => lead.to_owned(),
        Some(lead) => format!("{lead}: {first_line}"),
        None => first_line.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{Draft, ElementKind, Value, answer};
    use std::cell::RefCell;

    #[derive(Default)]
    struct Recorder(RefCell<Vec<Notice>>);

    impl Notifier for &Recorder {
        fn notify(&self, notice: &Notice) -> Result<(), NotifyError> {
            self.0.borrow_mut().push(notice.clone());
            Ok(())
        }
    }

    fn session(id: &str) -> (Dom, NodeId) {
        let mut dom = Dom::new();
        dom.commit(
            Draft::insert(
                NodeId::DOCUMENT,
                ElementKind::Session,
                [
                    (attr::ID, Value::from(id)),
                    (attr::HARNESS, "omp".into()),
                    (attr::NODE, "node-a".into()),
                    (attr::CREATED, 0i64.into()),
                    (attr::TITLE, "Fix the build".into()),
                ],
                "",
            ),
            0,
        )
        .unwrap();
        let root = dom.root().unwrap().id;
        let turn = dom
            .commit(
                Draft::insert(
                    root,
                    ElementKind::Turn,
                    [(attr::STATUS, Value::from("waiting"))],
                    "",
                ),
                0,
            )
            .unwrap();
        (dom, turn.node)
    }

    fn decision(dom: &mut Dom, turn: NodeId, attrs: Vec<(&str, Value)>, body: &str) -> NodeId {
        dom.commit(Draft::insert(turn, ElementKind::Decision, attrs, body), 0)
            .unwrap()
            .node
    }

    fn list(items: &[&str]) -> Value {
        Value::List(items.iter().map(|s| Value::from(*s)).collect())
    }

    #[test]
    fn badges_count_waiting_decisions_per_session() {
        let recorder = Recorder::default();
        let mut tracker = Tracker::new(&recorder);
        let (mut a, turn_a) = session("a");
        let (mut b, turn_b) = session("b");
        let first = decision(
            &mut a,
            turn_a,
            vec![(attr::KIND, "approval".into())],
            "rm -rf build",
        );
        decision(
            &mut a,
            turn_a,
            vec![(attr::KIND, "question".into())],
            "Which branch?",
        );
        decision(&mut b, turn_b, vec![(attr::KIND, "plan".into())], "# Plan");
        tracker.observe(&a, 10).unwrap();
        tracker.observe(&b, 10).unwrap();
        assert_eq!(
            (tracker.badge("a"), tracker.badge("b"), tracker.total()),
            (2, 1, 3)
        );

        a.commit(Draft::set(first, attr::STATUS, "answered"), 20)
            .unwrap();
        tracker.observe(&a, 20).unwrap();
        assert_eq!(tracker.badge("a"), 1);
        let mut badges: Vec<_> = tracker.badges().collect();
        badges.sort();
        assert_eq!(badges, [("a", 1), ("b", 1)]);

        tracker.forget("b");
        assert_eq!(tracker.total(), 1);
    }

    #[test]
    fn notifies_once_per_new_decision_only_in_background() {
        let recorder = Recorder::default();
        let mut tracker = Tracker::new(&recorder);
        let (mut dom, turn) = session("s");
        decision(
            &mut dom,
            turn,
            vec![(attr::KIND, "question".into())],
            "Seen in the foreground?",
        );
        tracker.observe(&dom, 0).unwrap();
        assert!(recorder.0.borrow().is_empty());

        tracker.set_background(true);
        tracker.observe(&dom, 1).unwrap();
        assert!(
            recorder.0.borrow().is_empty(),
            "an already known decision does not notify again"
        );

        let call = dom
            .commit(
                Draft::insert(
                    turn,
                    ElementKind::Call,
                    [(attr::TOOL, Value::from("bash"))],
                    "",
                ),
                2,
            )
            .unwrap()
            .node;
        let approval = dom
            .commit(
                Draft::insert(
                    call,
                    ElementKind::Decision,
                    [(attr::KIND, Value::from("approval"))],
                    "cargo publish\n",
                ),
                2,
            )
            .unwrap()
            .node;
        tracker.observe(&dom, 2).unwrap();
        tracker.observe(&dom, 3).unwrap();
        let notices = recorder.0.borrow();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].decision, approval);
        assert_eq!(notices[0].summary, "bash: cargo publish");
        assert_eq!(notices[0].heading(), "Fix the build needs approval");
    }

    #[test]
    fn unattended_timeout_applies_the_default_once() {
        let recorder = Recorder::default();
        let mut tracker = Tracker::new(&recorder);
        let (mut dom, turn) = session("s");
        let approval = decision(
            &mut dom,
            turn,
            vec![
                (attr::KIND, "approval".into()),
                (attr::DEFAULT, list(&[answer::DENY])),
                (attr::TIMEOUT_MS, 5_000i64.into()),
                (attr::CREATED, 1_000i64.into()),
            ],
            "rm -rf /tmp/x",
        );
        // No `created`: the clock starts when the tracker first sees it.
        let question = decision(
            &mut dom,
            turn,
            vec![
                (attr::KIND, "question".into()),
                (attr::OPTIONS, list(&["yes", "no"])),
                (attr::DEFAULT, list(&["no"])),
                (attr::TIMEOUT_MS, 1_000i64.into()),
            ],
            "Continue?",
        );
        // A timeout without a default never fires.
        decision(
            &mut dom,
            turn,
            vec![
                (attr::KIND, "plan".into()),
                (attr::TIMEOUT_MS, 10i64.into()),
            ],
            "# Plan",
        );
        tracker.observe(&dom, 4_000).unwrap();
        assert_eq!(tracker.next_deadline(), Some(5_000));
        assert!(tracker.tick(4_999).is_empty());

        let fired = tracker.tick(5_000);
        assert_eq!(
            fired,
            [Expired {
                session: "s".into(),
                decision: question,
                answer: Answer {
                    values: vec!["no".into()],
                    reason: None,
                    by: TIMEOUT_BY.into()
                },
            }]
        );
        assert_eq!(tracker.next_deadline(), Some(6_000));
        let fired = tracker.tick(7_000);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].decision, approval);
        assert_eq!(fired[0].answer.values, [answer::DENY]);
        assert_eq!(fired[0].answer.by, TIMEOUT_BY);
        assert!(
            tracker.tick(100_000).is_empty(),
            "each default is applied once"
        );
        assert_eq!(tracker.next_deadline(), None);
    }

    #[test]
    fn defaults_that_do_not_fit_the_kind_never_fire() {
        let recorder = Recorder::default();
        let mut tracker = Tracker::new(&recorder);
        let (mut dom, turn) = session("s");
        let timeout = (attr::TIMEOUT_MS, Value::from(1i64));
        decision(
            &mut dom,
            turn,
            vec![
                (attr::KIND, "approval".into()),
                (attr::DEFAULT, list(&["yes"])),
                timeout.clone(),
            ],
            "ls",
        );
        decision(
            &mut dom,
            turn,
            vec![
                (attr::KIND, "plan".into()),
                (attr::DEFAULT, list(&[answer::EDIT])),
                timeout.clone(),
            ],
            "# P",
        );
        decision(
            &mut dom,
            turn,
            vec![
                (attr::KIND, "form".into()),
                (attr::DEFAULT, list(&["[1]"])),
                timeout,
            ],
            "",
        );
        tracker.observe(&dom, 0).unwrap();
        assert!(tracker.tick(10).is_empty());
        assert_eq!(tracker.badge("s"), 3);
    }
}
