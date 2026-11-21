//! The decisions of every open session behind one component: cards per session, waiting
//! badges, notifications through a [`Notifier`] while the window is in the background, and
//! unattended timeouts. The app syncs it with each session's DOM, forwards messages, passes
//! [`Event::Answer`]s to the harness and persists [`Event::PolicyChanged`].

use super::card::kind_for;
use super::model::Decision;
use super::unattended::{self, Unattended};
use super::{Shortcut, State, TIMEOUT_BY, Viewer, shortcut};
use iced::futures::future::BoxFuture;
use iced::{Element, Subscription, Task, Theme, event, keyboard, time, window};
use loa_contracts::harness::Answer;
use loa_core::{DecisionKind, Dom, NodeId, attr};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

pub use loa_util::time::now_ms;

/// How often pending unattended timeouts are checked.
const TICK: Duration = Duration::from_millis(500);

/// A system notification about a session that needs input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub session: String,
    pub session_title: Option<String>,
    pub decision: NodeId,
    pub kind: DecisionKind,
    /// One line about what is asked: tool and command, question, title.
    pub summary: String,
}

impl Notice {
    pub fn heading(&self) -> String { format!("{} {}", self.session_title.as_deref().unwrap_or("A session"), kind_for(self.kind).notice()) }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotifyError(pub String);

impl std::fmt::Display for NotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "system notification failed: {}", self.0) }
}

/// Port for raising notifications off the UI thread.
pub trait Notifier: Send + Sync {
    fn notify(&self, n: &Notice) -> BoxFuture<'static, Result<(), NotifyError>>;
}

/// Desktop notifications through `notify-rust` (D-Bus on Linux).
#[derive(Clone, Debug)]
pub struct DesktopNotifier(pub String);

impl DesktopNotifier {
    pub fn new(app_name: impl Into<String>) -> Self { Self(app_name.into()) }
}

impl Notifier for DesktopNotifier {
    fn notify(&self, n: &Notice) -> BoxFuture<'static, Result<(), NotifyError>> {
        let mut notification = notify_rust::Notification::new();
        notification.appname(&self.0).summary(&n.heading()).body(&n.summary);
        Box::pin(async move { notification.show_async().await.map(drop).map_err(|e| NotifyError(e.to_string())) })
    }
}

/// Drops every notice: tests and headless runs.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullNotifier;

impl Notifier for NullNotifier {
    fn notify(&self, _: &Notice) -> BoxFuture<'static, Result<(), NotifyError>> { Box::pin(async { Ok(()) }) }
}

/// An unattended timeout that passed: hand `answer` to the session's harness.
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
    /// The decision's own valid default and deadline; wins over the policy.
    own: Option<(i64, Answer)>,
    fired: bool,
}

/// Waiting decisions of every open session: badges, notices, unattended timeouts. Pure: time is
/// passed in.
#[derive(Debug, Default)]
pub struct Tracker {
    pub background: bool,
    pub policy: Unattended,
    sessions: HashMap<String, BTreeMap<NodeId, Waiting>>,
}

impl Tracker {
    /// Reads the waiting decisions of the session in `dom` at `now_ms`. Returns a notice per
    /// decision first seen waiting while in the background.
    pub fn observe(&mut self, dom: &Dom, now_ms: i64) -> Vec<Notice> {
        let Some((root, id)) = dom.root().and_then(|r| Some((r, r.str_attr(attr::ID)?))) else { return Vec::new() };
        let known = self.sessions.entry(id.to_owned()).or_default();
        let mut still = BTreeMap::new();
        let mut notices = Vec::new();
        for decision in Decision::all(dom).filter(Decision::is_waiting) {
            let waiting = known.remove(&decision.id).unwrap_or_else(|| {
                if self.background {
                    let summary = summary(&decision);
                    notices.push(Notice { session: id.to_owned(), session_title: root.str_attr(attr::TITLE).map(str::to_owned), decision: decision.id, kind: decision.kind, summary });
                }
                let since = decision.created.unwrap_or(now_ms);
                let own = decision.timeout_ms.zip(decision.default_answer(TIMEOUT_BY)).map(|(t, a)| (since.saturating_add(t), a));
                Waiting { decision, since, own, fired: false }
            });
            still.insert(waiting.decision.id, waiting);
        }
        *known = still;
        notices
    }

    pub fn forget(&mut self, session: &str) { self.sessions.remove(session); }
    pub fn badge(&self, session: &str) -> usize { self.sessions.get(session).map_or(0, BTreeMap::len) }
    pub fn total(&self) -> usize { self.sessions.values().map(BTreeMap::len).sum() }

    fn deadline(&self, w: &Waiting) -> Option<i64> {
        match &w.own {
            Some((deadline, _)) => Some(*deadline),
            None => self.policy.timeout_ms(w.decision.kind).map(|t| w.since.saturating_add(t)),
        }
    }

    /// Default answers whose timeout passed at `now_ms`, each decision once.
    pub fn tick(&mut self, now_ms: i64) -> Vec<Expired> {
        let mut due: Vec<(String, NodeId)> =
            self.sessions.iter().flat_map(|(s, w)| w.iter().filter(|(_, w)| !w.fired && self.deadline(w).is_some_and(|d| d <= now_ms)).map(move |(id, _)| (s.clone(), *id))).collect();
        due.sort();
        let policy = self.policy.clone();
        due.into_iter()
            .filter_map(|(session, decision)| {
                let w = self.sessions.get_mut(&session)?.get_mut(&decision)?;
                w.fired = true;
                // A hand-edited default that doesn't fit leaves it waiting for someone.
                let answer = w.own.as_ref().map(|(_, a)| a.clone()).or_else(|| policy.answer(&w.decision, TIMEOUT_BY))?;
                Some(Expired { session, decision, answer })
            })
            .collect()
    }

    /// The earliest pending timeout.
    pub fn next_deadline(&self) -> Option<i64> { self.sessions.values().flat_map(BTreeMap::values).filter(|w| !w.fired).filter_map(|w| self.deadline(w)).min() }
}

fn summary(d: &Decision) -> String {
    let first = d.body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let lead = match (d.kind, &d.tool, &d.title) {
        (DecisionKind::Approval, Some(tool), _) => Some(tool),
        (_, _, title) => title.as_ref(),
    };
    match lead {
        Some(lead) if first.is_empty() => lead.clone(),
        Some(lead) => format!("{lead}: {first}"),
        None => first.to_owned(),
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    /// A card message of one session.
    Card(String, super::Message),
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
    Answer { session: String, decision: NodeId, answer: Answer },
    OpenLink(String),
    /// The unattended settings changed: persist them.
    PolicyChanged(Unattended),
    NotifyFailed(String),
}

pub struct Desk {
    /// Who answers here: the local node, with drive access.
    local: String,
    local_name: Option<String>,
    theme: Theme,
    sessions: HashMap<String, State>,
    active: Option<String>,
    pub(crate) tracker: Tracker,
    notifier: Arc<dyn Notifier>,
}

impl Desk {
    /// `by` is recorded on every answer given here.
    pub fn new(by: impl Into<String>, theme: &Theme, policy: Unattended, notifier: impl Notifier + 'static) -> Self {
        let tracker = Tracker { policy, ..Tracker::default() };
        Self { local: by.into(), local_name: None, theme: theme.clone(), sessions: HashMap::new(), active: None, tracker, notifier: Arc::new(notifier) }
    }

    /// The session shown in the center; it takes the keyboard shortcuts.
    pub fn set_active(&mut self, session: Option<String>) { self.active = session }

    pub fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.sessions.values_mut().for_each(|s| s.set_theme(theme));
    }

    /// The local node's host name, shown on its answers instead of its key.
    pub fn set_local_name(&mut self, name: impl Into<String>) {
        let name = name.into();
        self.sessions.values_mut().for_each(|s| s.set_local_name(Some(name.clone())));
        self.local_name = Some(name);
    }

    /// A peer's session this node may only watch: its cards show but can't be answered.
    pub fn set_view_only(&mut self, session: &str, view_only: bool) {
        let viewer = Viewer { by: self.local.clone(), view_only };
        self.state(session).set_viewer(viewer);
    }

    fn state(&mut self, session: &str) -> &mut State {
        let (local, theme, name) = (&self.local, &self.theme, &self.local_name);
        self.sessions.entry(session.to_owned()).or_insert_with(|| {
            let mut state = State::new(Viewer { by: local.clone(), view_only: false }, theme);
            state.set_local_name(name.clone());
            state
        })
    }

    /// Brings the cards and badges of the session in `dom` in line at `now_ms`; returns the
    /// notices to raise with [`notify`](Self::notify).
    pub fn sync(&mut self, dom: &Dom, now_ms: i64) -> Vec<Notice> {
        let Some(session) = dom.root().and_then(|r| r.str_attr(attr::ID)) else { return Vec::new() };
        self.state(session).sync(dom);
        self.tracker.observe(dom, now_ms)
    }

    /// Raises notifications off the UI thread.
    pub fn notify(&self, notices: Vec<Notice>) -> Task<Message> { Task::batch(notices.iter().map(|n| Task::perform(self.notifier.notify(n), Message::Notified))) }

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

    pub fn policy(&self) -> &Unattended { &self.tracker.policy }

    pub fn update(&mut self, message: Message) -> Vec<Event> {
        match message {
            Message::Card(session, message) => self.card(session, message),
            Message::Shortcut(key) => self.active.clone().map_or_else(Vec::new, |s| self.card(s, super::Message::Shortcut(key))),
            Message::Focused(focused) => {
                self.tracker.background = !focused;
                Vec::new()
            }
            Message::Tick(now) => {
                let expired = self.tracker.tick(now);
                for e in &expired {
                    if let Some(state) = self.sessions.get_mut(&e.session) {
                        state.answering(e.decision);
                    }
                }
                expired.into_iter().map(|Expired { session, decision, answer }| Event::Answer { session, decision, answer }).collect()
            }
            Message::Notified(result) => result.err().map(|e| Event::NotifyFailed(e.to_string())).into_iter().collect(),
            Message::Settings(edit) => {
                let mut policy = self.tracker.policy.clone();
                policy.update(edit);
                if policy == self.tracker.policy {
                    return Vec::new();
                }
                self.tracker.policy = policy.clone();
                vec![Event::PolicyChanged(policy)]
            }
        }
    }

    fn card(&mut self, session: String, message: super::Message) -> Vec<Event> {
        let Some(state) = self.sessions.get_mut(&session) else { return Vec::new() };
        match state.update(message).1 {
            Some(super::Event::Answer { decision, answer }) => vec![Event::Answer { session, decision, answer }],
            Some(super::Event::OpenLink(url)) => vec![Event::OpenLink(url)],
            None => Vec::new(),
        }
    }

    /// The card of one decision, for the transcript to place inline.
    pub fn view_card(&self, session: &str, decision: NodeId) -> Option<Element<'_, Message>> {
        let owned = session.to_owned();
        Some(self.sessions.get(session)?.view_card(decision)?.map(move |m| Message::Card(owned.clone(), m)))
    }

    /// The card taking shortcuts in the active session.
    pub fn focused(&self) -> Option<NodeId> { self.sessions.get(self.active.as_deref()?)?.focused() }
    /// Waiting decisions of `session`, for its sidebar row.
    pub fn badge(&self, session: &str) -> usize { self.tracker.badge(session) }
    /// Waiting decisions across every session.
    pub fn total(&self) -> usize { self.tracker.total() }
    pub fn settings_view(&self) -> Element<'_, Message> { super::settings::view(&self.tracker.policy).map(Message::Settings) }

    /// Window focus always; number keys and Enter while the active session has a card to answer
    /// (Esc is left to the app, which closes screens first); a clock while a timeout is pending.
    pub fn subscription(&self) -> Subscription<Message> {
        let focus = event::listen_with(|event, _, _| match event {
            iced::Event::Window(window::Event::Focused) => Some(Message::Focused(true)),
            iced::Event::Window(window::Event::Unfocused) => Some(Message::Focused(false)),
            _ => None,
        });
        let keys = match self.focused() {
            Some(_) => keyboard::listen().filter_map(|e| shortcut(&e).filter(|k| *k != Shortcut::Dismiss).map(Message::Shortcut)),
            None => Subscription::none(),
        };
        let clock = match self.tracker.next_deadline() {
            Some(_) => time::every(TICK).map(|_| Message::Tick(now_ms())),
            None => Subscription::none(),
        };
        Subscription::batch([focus, keys, clock])
    }
}
