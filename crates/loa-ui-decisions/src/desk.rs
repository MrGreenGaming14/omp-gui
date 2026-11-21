//! The decisions of every open session behind one component: cards per
//! session, waiting badges, system notifications while the window is in the
//! background, unattended timeouts and their settings.
//!
//! The app calls [`Desk::sync`] with a session's DOM after every patch and
//! raises the returned notices with [`Desk::notify`], forwards
//! [`Message`]s, passes every [`Event::Answer`] to that session's
//! `HarnessSession::answer` (and [`Desk::answer_failed`] when it errs),
//! persists [`Event::PolicyChanged`], and places [`Desk::view_card`] where
//! the chat shows a `<decision>`.

use crate::tracker::{DesktopNotifier, Notice, NotifyError, Tracker};
use crate::unattended::{self, Unattended};
use crate::{Shortcut, State, Viewer, shortcut};
use iced::{Element, Subscription, Task, Theme, event, keyboard, time, window};
use loa_adapter::Answer;
use loa_core::{Dom, NodeId, attr};
use std::collections::HashMap;
use std::time::Duration;

/// How often pending unattended timeouts are checked.
const TICK: Duration = Duration::from_millis(500);

/// Unix ms now.
pub use loa_util::time::now_ms;

#[derive(Clone, Debug)]
pub enum Message {
    /// A card message of one session.
    Card(String, crate::Message),
    /// Number key, Enter or Esc for the active session's focused card.
    Shortcut(Shortcut),
    /// The app window gained (`true`) or lost focus.
    Focused(bool),
    /// Check unattended timeouts at this unix ms.
    Tick(i64),
    Notified(Result<(), NotifyError>),
    Settings(unattended::Message),
}

/// What the app must act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Pass to `HarnessSession::answer(decision, answer)` of `session`.
    Answer {
        session: String,
        decision: NodeId,
        answer: Answer,
    },
    OpenLink(String),
    /// The unattended settings changed: persist them.
    PolicyChanged(Unattended),
    /// Raising a system notification failed.
    NotifyFailed(String),
}

pub struct Desk {
    /// Who answers here: the local node, with drive access.
    local: String,
    /// The local node's human name, shown on its answers instead of `local`.
    local_name: Option<String>,
    theme: Theme,
    sessions: HashMap<String, State>,
    active: Option<String>,
    tracker: Tracker,
    notifier: DesktopNotifier,
}

impl Desk {
    /// `by` is recorded on every answer given here.
    pub fn new(by: impl Into<String>, theme: &Theme, policy: Unattended, notifier: DesktopNotifier) -> Self {
        Self {
            local: by.into(),
            local_name: None,
            theme: theme.clone(),
            sessions: HashMap::new(),
            active: None,
            tracker: Tracker::new(policy),
            notifier,
        }
    }

    /// The session shown in the center: it takes the keyboard shortcuts.
    pub fn set_active(&mut self, session: Option<String>) {
        self.active = session;
    }

    pub fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        for state in self.sessions.values_mut() {
            state.set_theme(theme);
        }
    }

    /// The local node's human name (its host name): cards show it on
    /// answers this node gave instead of its key.
    pub fn set_local_name(&mut self, name: impl Into<String>) {
        let name = name.into();
        for state in self.sessions.values_mut() {
            state.set_local_name(Some(name.clone()));
        }
        self.local_name = Some(name);
    }

    /// A peer's session the local node may only watch: its cards show but
    /// can't be answered.
    pub fn set_view_only(&mut self, session: &str, view_only: bool) {
        let viewer = self.viewer(view_only);
        self.state(session).set_viewer(viewer);
    }

    fn viewer(&self, view_only: bool) -> Viewer {
        Viewer {
            by: self.local.clone(),
            view_only,
        }
    }

    fn state(&mut self, session: &str) -> &mut State {
        if !self.sessions.contains_key(session) {
            let mut state = State::new(self.viewer(false), &self.theme);
            state.set_local_name(self.local_name.clone());
            self.sessions.insert(session.to_owned(), state);
        }
        self.sessions.get_mut(session).expect("inserted above")
    }

    /// Brings the cards and badges of the session in `dom` in line with it
    /// at `now_ms`. Returns the notices to raise with [`notify`](Self::notify).
    pub fn sync(&mut self, dom: &Dom, now_ms: i64) -> Vec<Notice> {
        let Some(session) = dom.root().and_then(|r| r.str_attr(attr::ID)) else {
            return Vec::new();
        };
        self.state(session).sync(dom);
        self.tracker.observe(dom, now_ms)
    }

    /// Raises system notifications off the UI thread.
    pub fn notify(&self, notices: Vec<Notice>) -> Task<Message> {
        Task::batch(
            notices
                .iter()
                .map(|n| Task::perform(self.notifier.notify(n), Message::Notified)),
        )
    }

    /// Drops a closed session.
    pub fn forget(&mut self, session: &str) {
        self.sessions.remove(session);
        self.tracker.forget(session);
        if self.active.as_deref() == Some(session) {
            self.active = None;
        }
    }

    /// Reopens a card whose answer the harness refused.
    pub fn answer_failed(&mut self, session: &str, decision: NodeId) {
        if let Some(state) = self.sessions.get_mut(session) {
            state.answer_failed(decision);
        }
    }

    pub fn policy(&self) -> &Unattended {
        self.tracker.policy()
    }

    pub fn update(&mut self, message: Message) -> Vec<Event> {
        match message {
            Message::Card(session, message) => self.card(session, message),
            Message::Shortcut(shortcut) => match self.active.clone() {
                Some(session) => self.card(session, crate::Message::Shortcut(shortcut)),
                None => Vec::new(),
            },
            Message::Focused(focused) => {
                self.tracker.set_background(!focused);
                Vec::new()
            }
            Message::Tick(now) => self
                .tracker
                .tick(now)
                .into_iter()
                .map(|expired| {
                    if let Some(state) = self.sessions.get_mut(&expired.session) {
                        state.answering(expired.decision);
                    }
                    Event::Answer {
                        session: expired.session,
                        decision: expired.decision,
                        answer: expired.answer,
                    }
                })
                .collect(),
            Message::Notified(result) => result
                .err()
                .map(|e| Event::NotifyFailed(e.to_string()))
                .into_iter()
                .collect(),
            Message::Settings(edit) => {
                let mut policy = self.tracker.policy().clone();
                policy.update(edit);
                if policy == *self.tracker.policy() {
                    return Vec::new();
                }
                self.tracker.set_policy(policy.clone());
                vec![Event::PolicyChanged(policy)]
            }
        }
    }

    fn card(&mut self, session: String, message: crate::Message) -> Vec<Event> {
        let Some(state) = self.sessions.get_mut(&session) else {
            return Vec::new();
        };
        // Card messages never start work of their own.
        let (_, event) = state.update(message);
        match event {
            Some(crate::Event::Answer { decision, answer }) => vec![Event::Answer {
                session,
                decision,
                answer,
            }],
            Some(crate::Event::OpenLink(url)) => vec![Event::OpenLink(url)],
            None => Vec::new(),
        }
    }

    /// The card of one decision, for the chat to place inline.
    pub fn view_card(&self, session: &str, decision: NodeId) -> Option<Element<'_, Message>> {
        let owned = session.to_owned();
        self.sessions
            .get(session)?
            .view_card(decision)
            .map(move |card| card.map(move |m| Message::Card(owned.clone(), m)))
    }

    /// The card that takes shortcuts in the active session.
    pub fn focused(&self) -> Option<NodeId> {
        self.sessions.get(self.active.as_deref()?)?.focused()
    }

    /// Waiting decisions of `session`, for its sidebar row and the top bar.
    pub fn badge(&self, session: &str) -> usize {
        self.tracker.badge(session)
    }

    /// Waiting decisions across every session.
    pub fn total(&self) -> usize {
        self.tracker.total()
    }

    /// The unattended settings section.
    pub fn settings_view(&self) -> Element<'_, Message> {
        self.tracker.policy().view().map(Message::Settings)
    }

    /// Window focus always; number keys and Enter while the active session
    /// has a card to answer; a clock while an unattended timeout is pending.
    /// Esc is left to the app, which closes its palette or screen first and
    /// sends [`Shortcut::Dismiss`] when nothing else takes it.
    pub fn subscription(&self) -> Subscription<Message> {
        let focus = event::listen_with(|event, _, _| match event {
            iced::Event::Window(window::Event::Focused) => Some(Message::Focused(true)),
            iced::Event::Window(window::Event::Unfocused) => Some(Message::Focused(false)),
            _ => None,
        });
        let keys = if self.focused().is_some() {
            keyboard::listen().filter_map(|event| {
                shortcut(&event)
                    .filter(|key| *key != Shortcut::Dismiss)
                    .map(Message::Shortcut)
            })
        } else {
            Subscription::none()
        };
        let clock = if self.tracker.next_deadline().is_some() {
            time::every(TICK).map(|_| Message::Tick(now_ms()))
        } else {
            Subscription::none()
        };
        Subscription::batch([focus, keys, clock])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unattended::Duration as Secs;
    use iced_test::simulator;
    use loa_core::{DecisionKind, Draft, ElementKind, Value, answer};

    fn session(id: &str) -> (Dom, NodeId) {
        let mut dom = Dom::new();
        let root = dom
            .commit(
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
            .unwrap()
            .node;
        let turn = dom
            .commit(Draft::insert(root, ElementKind::Turn, [(attr::STATUS, Value::from("waiting"))], ""), 0)
            .unwrap()
            .node;
        (dom, turn)
    }

    fn approval(dom: &mut Dom, turn: NodeId) -> NodeId {
        dom.commit(
            Draft::insert(turn, ElementKind::Decision, [(attr::KIND, Value::from("approval"))], "cargo publish"),
            0,
        )
        .unwrap()
        .node
    }

    fn desk() -> Desk {
        Desk::new("node-me", &Theme::Dark, Unattended::default(), DesktopNotifier::new("loa"))
    }

    #[test]
    fn card_answers_name_their_session_and_who_answered() {
        let mut desk = desk();
        let (mut a, turn) = session("a");
        let id = approval(&mut a, turn);
        desk.sync(&a, 0);
        assert_eq!((desk.badge("a"), desk.total()), (1, 1));
        let events = desk.update(Message::Card("a".into(), crate::Message::Choose(id, 1)));
        assert_eq!(
            events,
            [Event::Answer {
                session: "a".into(),
                decision: id,
                answer: Answer { values: vec![answer::ALLOW_SESSION.into()], reason: None, by: "node-me".into() },
            }]
        );
        // Locked until the DOM records it; a refusal reopens the card.
        assert!(desk.update(Message::Card("a".into(), crate::Message::Choose(id, 0))).is_empty());
        desk.answer_failed("a", id);
        assert_eq!(desk.update(Message::Card("a".into(), crate::Message::Choose(id, 2))).len(), 1);

        a.commit(Draft::set(id, attr::STATUS, "answered"), 1).unwrap();
        desk.sync(&a, 1);
        assert_eq!(desk.total(), 0);
    }

    #[test]
    fn shortcuts_go_to_the_active_session_only() {
        let mut desk = desk();
        let (mut a, turn_a) = session("a");
        let (mut b, turn_b) = session("b");
        approval(&mut a, turn_a);
        let in_b = approval(&mut b, turn_b);
        desk.sync(&a, 0);
        desk.sync(&b, 0);
        assert!(desk.update(Message::Shortcut(Shortcut::Dismiss)).is_empty(), "no active session");
        desk.set_active(Some("b".into()));
        assert_eq!(desk.focused(), Some(in_b));
        desk.update(Message::Shortcut(Shortcut::Pick(0)));
        let events = desk.update(Message::Shortcut(Shortcut::Confirm));
        assert!(matches!(
            events.as_slice(),
            [Event::Answer { session, decision, answer }]
                if session == "b" && *decision == in_b && answer.values == [answer::ALLOW_ONCE]
        ));
    }

    #[test]
    fn notices_only_while_the_window_is_unfocused() {
        let mut desk = desk();
        let (mut a, turn) = session("a");
        approval(&mut a, turn);
        assert!(desk.sync(&a, 0).is_empty());
        desk.update(Message::Focused(false));
        let second = approval(&mut a, turn);
        let notices = desk.sync(&a, 1);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].decision, second);
        assert_eq!(notices[0].heading(), "Fix the build needs approval");
        desk.update(Message::Focused(true));
        approval(&mut a, turn);
        assert!(desk.sync(&a, 2).is_empty());
        assert_eq!(
            desk.update(Message::Notified(Err(NotifyError("no daemon".into())))),
            [Event::NotifyFailed("system notification failed: no daemon".into())]
        );
    }

    #[test]
    fn view_only_sessions_show_cards_that_cannot_be_answered() {
        let mut desk = desk();
        let (mut a, turn) = session("a");
        let id = approval(&mut a, turn);
        desk.set_view_only("a", true);
        desk.sync(&a, 0);
        desk.set_active(Some("a".into()));
        assert_eq!(desk.focused(), None);
        assert!(desk.update(Message::Card("a".into(), crate::Message::Choose(id, 0))).is_empty());
        let mut ui = simulator(desk.view_card("a", id).unwrap());
        ui.find("View only: you can't answer this prompt").expect("view only footer");
    }

    #[test]
    fn unattended_settings_and_timeout_answer_waiting_cards() {
        let mut desk = desk();
        let events = desk.update(Message::Settings(unattended::Message::Enabled(true)));
        assert!(matches!(events.as_slice(), [Event::PolicyChanged(p)] if p.timeout_ms(DecisionKind::Approval) == Some(300_000)));
        desk.update(Message::Settings(unattended::Message::Timeout(DecisionKind::Approval, Secs(30))));
        let events = desk.update(Message::Settings(unattended::Message::Answer(
            DecisionKind::Approval,
            Some(answer::ALLOW_ONCE.into()),
        )));
        assert!(
            matches!(events.as_slice(), [Event::PolicyChanged(p)] if p.0.approval.as_ref().is_some_and(|d| d.answer == answer::ALLOW_ONCE))
        );
        assert!(
            desk.update(Message::Settings(unattended::Message::Timeout(DecisionKind::Approval, Secs(30)))).is_empty(),
            "no change"
        );

        let (mut a, turn) = session("a");
        let id = approval(&mut a, turn);
        desk.sync(&a, 1_000);
        assert!(desk.update(Message::Tick(30_999)).is_empty());
        let events = desk.update(Message::Tick(31_000));
        assert_eq!(
            events,
            [Event::Answer {
                session: "a".into(),
                decision: id,
                answer: Answer { values: vec![answer::ALLOW_ONCE.into()], reason: None, by: crate::TIMEOUT_BY.into() },
            }]
        );
        simulator(desk.view_card("a", id).unwrap())
            .find("Sending answer…")
            .expect("the card is locked while the answer is sent");
        assert!(desk.update(Message::Tick(99_000)).is_empty());

        let mut ui = simulator(desk.settings_view());
        ui.find("Answer waiting prompts after a timeout").expect("settings section");
        ui.find("Plan reviews").expect("plan default row");
        ui.find("Approvals").expect("approval default row");
        ui.find("Forms").expect("form default row");
    }
}
