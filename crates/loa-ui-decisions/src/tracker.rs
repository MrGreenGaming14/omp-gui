//! Waiting decisions across every open session: badge counts, system
//! notifications while the app is in the background, and unattended
//! timeouts. Pure: time is passed in, and notices are returned for the app
//! to raise through [`DesktopNotifier`].

use crate::decision::Decision;
use crate::unattended::Unattended;
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

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("system notification failed: {0}")]
pub struct NotifyError(pub String);

/// Desktop notifications through `notify-rust` (D-Bus on Linux, pure Rust).
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

    /// Raises `notice`. Resolves after one round trip to the notification
    /// daemon, so the UI thread never waits on it.
    pub fn notify(
        &self,
        notice: &Notice,
    ) -> impl Future<Output = Result<(), NotifyError>> + Send + use<> {
        let mut notification = notify_rust::Notification::new();
        notification
            .appname(&self.app_name)
            .summary(&notice.heading())
            .body(&notice.summary);
        async move {
            #[cfg(not(windows))]
            let shown = notification.show_async().await;
            // `show_async` does not exist on Windows: run the blocking
            // `show` on its own thread so the UI never waits on it.
            #[cfg(windows)]
            let shown = {
                let (sender, receiver) = iced::futures::channel::oneshot::channel();
                std::thread::spawn(move || {
                    let _ = sender.send(notification.show());
                });
                receiver.await.map_err(|e| NotifyError(e.to_string()))?
            };
            shown.map(drop).map_err(|e| NotifyError(e.to_string()))
        }
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
    decision: Decision,
    /// Unix ms the wait started: `created`, else when first seen.
    since: i64,
    /// The decision's own valid default with its deadline. Wins over the
    /// unattended policy.
    own: Option<(i64, Answer)>,
    fired: bool,
}

#[derive(Debug, Default)]
struct Session {
    waiting: BTreeMap<NodeId, Waiting>,
}

/// Tracks the waiting decisions of every session the app has open.
#[derive(Debug, Default)]
pub struct Tracker {
    background: bool,
    policy: Unattended,
    sessions: HashMap<String, Session>,
}

impl Tracker {
    pub fn new(policy: Unattended) -> Self {
        Self {
            policy,
            ..Self::default()
        }
    }

    /// Whether the app window is in the background. Notices are only
    /// returned while it is.
    pub fn set_background(&mut self, background: bool) {
        self.background = background;
    }

    pub fn background(&self) -> bool {
        self.background
    }

    pub fn policy(&self) -> &Unattended {
        &self.policy
    }

    /// Replaces the unattended policy. Decisions already waiting follow the
    /// new timeout, counted from when they started.
    pub fn set_policy(&mut self, policy: Unattended) {
        self.policy = policy;
    }

    /// Reads the waiting decisions of the session in `dom` (keyed by
    /// `<session id>`) at `now_ms`. Returns one notice per decision seen
    /// waiting for the first time while the app is in the background.
    pub fn observe(&mut self, dom: &Dom, now_ms: i64) -> Vec<Notice> {
        let Some(root) = dom.root() else {
            return Vec::new();
        };
        let Some(session_id) = root.str_attr(attr::ID) else {
            return Vec::new();
        };
        let session_title = root.str_attr(attr::TITLE);
        let session = self.sessions.entry(session_id.to_owned()).or_default();
        let mut still = BTreeMap::new();
        let mut notices = Vec::new();
        for decision in Decision::all(dom).filter(Decision::is_waiting) {
            let id = decision.id;
            let waiting = match session.waiting.remove(&id) {
                Some(known) => known,
                None => {
                    if self.background {
                        notices.push(Notice {
                            session: session_id.to_owned(),
                            session_title: session_title.map(str::to_owned),
                            decision: id,
                            kind: decision.kind,
                            summary: summary(&decision),
                        });
                    }
                    let since = decision.created.unwrap_or(now_ms);
                    let own = decision
                        .timeout_ms
                        .zip(decision.default_answer(TIMEOUT_BY))
                        .map(|(t, answer)| (since.saturating_add(t), answer));
                    Waiting {
                        decision,
                        since,
                        own,
                        fired: false,
                    }
                }
            };
            still.insert(id, waiting);
        }
        session.waiting = still;
        notices
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

    fn deadline(&self, waiting: &Waiting) -> Option<i64> {
        match &waiting.own {
            Some((deadline, _)) => Some(*deadline),
            None => self
                .policy
                .timeout_ms(waiting.decision.kind)
                .map(|t| waiting.since.saturating_add(t)),
        }
    }

    /// Default answers whose timeout has passed at `now_ms`. Each decision
    /// is returned once.
    pub fn tick(&mut self, now_ms: i64) -> Vec<Expired> {
        let mut due = Vec::new();
        for (session, state) in &self.sessions {
            for (id, waiting) in &state.waiting {
                if !waiting.fired && self.deadline(waiting).is_some_and(|d| d <= now_ms) {
                    due.push((session.clone(), *id));
                }
            }
        }
        due.sort();
        let mut expired = Vec::with_capacity(due.len());
        for (session, id) in due {
            let Some(waiting) = self
                .sessions
                .get_mut(&session)
                .and_then(|s| s.waiting.get_mut(&id))
            else {
                continue;
            };
            waiting.fired = true;
            // A hand edited default that doesn't fit the decision leaves it
            // waiting for someone.
            let answer = match &waiting.own {
                Some((_, answer)) => answer.clone(),
                None => match self.policy.answer(&waiting.decision, TIMEOUT_BY) {
                    Some(answer) => answer,
                    None => continue,
                },
            };
            expired.push(Expired {
                session,
                decision: id,
                answer,
            });
        }
        expired
    }

    /// The earliest pending timeout, so the app can schedule the next
    /// [`tick`](Self::tick).
    pub fn next_deadline(&self) -> Option<i64> {
        self.sessions
            .values()
            .flat_map(|s| s.waiting.values())
            .filter(|w| !w.fired)
            .filter_map(|w| self.deadline(w))
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
    use loa_config::{DecisionDefault, UnattendedSettings};
    use loa_core::{Draft, ElementKind, Value, answer};

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
        let mut tracker = Tracker::default();
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
        tracker.observe(&a, 10);
        tracker.observe(&b, 10);
        assert_eq!(
            (tracker.badge("a"), tracker.badge("b"), tracker.total()),
            (2, 1, 3)
        );

        a.commit(Draft::set(first, attr::STATUS, "answered"), 20)
            .unwrap();
        tracker.observe(&a, 20);
        assert_eq!(tracker.badge("a"), 1);
        let mut badges: Vec<_> = tracker.badges().collect();
        badges.sort();
        assert_eq!(badges, [("a", 1), ("b", 1)]);

        tracker.forget("b");
        assert_eq!(tracker.total(), 1);
    }

    #[test]
    fn notifies_once_per_new_decision_only_in_background() {
        let mut tracker = Tracker::default();
        let (mut dom, turn) = session("s");
        decision(
            &mut dom,
            turn,
            vec![(attr::KIND, "question".into())],
            "Seen in the foreground?",
        );
        assert!(tracker.observe(&dom, 0).is_empty());

        tracker.set_background(true);
        assert!(
            tracker.observe(&dom, 1).is_empty(),
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
        let notices = tracker.observe(&dom, 2);
        assert!(tracker.observe(&dom, 3).is_empty());
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].decision, approval);
        assert_eq!(notices[0].summary, "bash: cargo publish");
        assert_eq!(notices[0].heading(), "Fix the build needs approval");
    }

    #[test]
    fn unattended_timeout_applies_the_default_once() {
        let mut tracker = Tracker::default();
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
        // A timeout without a default never fires while the policy is off.
        decision(
            &mut dom,
            turn,
            vec![
                (attr::KIND, "plan".into()),
                (attr::TIMEOUT_MS, 10i64.into()),
            ],
            "# Plan",
        );
        tracker.observe(&dom, 4_000);
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
    fn the_policy_answers_decisions_without_their_own_default() {
        let after = |timeout_secs: u64, answer: &str| {
            Some(DecisionDefault { timeout_secs, answer: answer.to_owned(), reason: None })
        };
        let mut tracker = Tracker::new(Unattended(UnattendedSettings {
            approval: after(60, answer::ALLOW_ONCE),
            question: after(60, "RED"),
            ..UnattendedSettings::default()
        }));
        let (mut dom, turn) = session("s");
        let approval = decision(
            &mut dom,
            turn,
            vec![(attr::KIND, "approval".into())],
            "ls",
        );
        let question = decision(
            &mut dom,
            turn,
            vec![
                (attr::KIND, "question".into()),
                (attr::OPTIONS, list(&["red", "green"])),
            ],
            "Color?",
        );
        tracker.observe(&dom, 1_000);
        assert_eq!(tracker.next_deadline(), Some(61_000));
        assert!(tracker.tick(60_999).is_empty());
        let fired = tracker.tick(61_000);
        let answers: Vec<_> = fired
            .iter()
            .map(|e| (e.decision, e.answer.values.clone(), e.answer.by.as_str()))
            .collect();
        assert_eq!(
            answers,
            [
                (approval, vec![answer::ALLOW_ONCE.to_owned()], TIMEOUT_BY),
                (question, vec!["red".to_owned()], TIMEOUT_BY),
            ]
        );

        // A plan default set later applies to decisions already waiting.
        let late = decision(&mut dom, turn, vec![(attr::KIND, "plan".into())], "# P");
        tracker.observe(&dom, 70_000);
        tracker.set_policy(Unattended(UnattendedSettings {
            plan: after(30, answer::REJECT),
            ..UnattendedSettings::default()
        }));
        assert_eq!(tracker.next_deadline(), Some(100_000));
        let fired = tracker.tick(100_000);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].decision, late);
        assert_eq!(fired[0].answer.values, [answer::REJECT]);

        tracker.set_policy(Unattended::default());
        decision(&mut dom, turn, vec![(attr::KIND, "approval".into())], "pwd");
        tracker.observe(&dom, 200_000);
        assert_eq!(tracker.next_deadline(), None);
        assert!(tracker.tick(i64::MAX).is_empty(), "policy off: nothing fires");
    }

    #[test]
    fn defaults_that_do_not_fit_the_kind_never_fire() {
        let mut tracker = Tracker::default();
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
        tracker.observe(&dom, 0);
        assert!(tracker.tick(10).is_empty());
        assert_eq!(tracker.badge("s"), 3);
    }
}
