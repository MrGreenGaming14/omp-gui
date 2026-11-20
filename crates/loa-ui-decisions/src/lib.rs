//! Decision prompt cards: approval, question, plan review and form.
//!
//! [`Desk`] is the app-facing component: the cards of every open session,
//! waiting badges, system notifications and unattended timeouts. [`State`]
//! holds the cards of one session; [`Tracker`] follows waiting decisions
//! across sessions.

mod card;
pub mod decision;
pub mod desk;
pub mod diff;
pub mod form;
mod style;
pub mod tracker;
pub mod unattended;

pub use card::{FieldInput, form_field_id, text_field_id};
pub use decision::{ApprovalChoice, Decision, PlanChoice, answer_summary};
pub use desk::Desk;
pub use tracker::{DesktopNotifier, Expired, Notice, NotifyError, TIMEOUT_BY, Tracker};
pub use unattended::Unattended;

use card::{Card, Reply};
use iced::keyboard::{self, key};
use iced::widget::{column, container, markdown, text, text_editor};
use iced::{Element, Subscription, Task, Theme};
use loa_adapter::Answer;
use loa_core::{Dom, NodeId};
use std::collections::BTreeMap;

/// How `by` (a node key, a user, or [`TIMEOUT_BY`]) reads on a card: the
/// local node's own key shows as `local_name`, other node keys (long
/// alphanumeric ids) as their first 8 characters, anything else as is.
pub fn node_name<'a>(by: &'a str, local: &str, local_name: Option<&'a str>) -> &'a str {
    if let Some(name) = local_name.filter(|name| !name.is_empty() && by == local) {
        return name;
    }
    let is_key = by.len() >= 32 && by.bytes().all(|b| b.is_ascii_alphanumeric());
    if is_key { &by[..8] } else { by }
}

/// Who is looking at the cards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Viewer {
    /// Node key or user recorded as `by` on answers.
    pub by: String,
    /// A peer without drive access: cards show but can't be answered.
    pub view_only: bool,
}

/// A keyboard shortcut for the focused card.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Shortcut {
    /// Number key `n` picks option `n - 1`.
    Pick(usize),
    /// Enter confirms the pick.
    Confirm,
    /// Esc skips a question or form, denies an approval, rejects a plan.
    Dismiss,
}

/// Maps a key press to a card shortcut. Presses with Ctrl, Alt or Logo held
/// are left to the rest of the app.
pub fn shortcut(event: &keyboard::Event) -> Option<Shortcut> {
    let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
        return None;
    };
    if modifiers.control() || modifiers.alt() || modifiers.logo() {
        return None;
    }
    match key.as_ref() {
        keyboard::Key::Named(key::Named::Enter) => Some(Shortcut::Confirm),
        keyboard::Key::Named(key::Named::Escape) => Some(Shortcut::Dismiss),
        keyboard::Key::Character(c) => match c.parse::<usize>() {
            Ok(n @ 1..=9) => Some(Shortcut::Pick(n - 1)),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    /// Click on option `n` of a card.
    Choose(NodeId, usize),
    /// Submit button: multi choice, form, edited plan.
    Submit(NodeId),
    Skip(NodeId),
    /// Deny reason, reject feedback or typed answer edited.
    Text(NodeId, String),
    /// Enter in that text field.
    SubmitText(NodeId),
    EditPlan(NodeId, text_editor::Action),
    CancelEdit(NodeId),
    Field(NodeId, usize, FieldInput),
    Shortcut(Shortcut),
    Link(markdown::Uri),
    /// Expands or collapses the whole diff of an approval card.
    ToggleDiff(NodeId),
}

/// What the app must act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Pass to `HarnessSession::answer(decision, answer)`. The card stays
    /// locked until the DOM records the answer or
    /// [`State::answer_failed`] reopens it.
    Answer { decision: NodeId, answer: Answer },
    /// A link in a plan was clicked.
    OpenLink(String),
}

/// The decision cards of one session.
pub struct State {
    viewer: Viewer,
    markdown: markdown::Settings,
    /// By node id. Ids only go up, so the first open card is the oldest.
    cards: BTreeMap<NodeId, Card>,
    /// The local node's human name, shown instead of its key on answers.
    local_name: Option<String>,
}

impl State {
    pub fn new(viewer: Viewer, theme: &Theme) -> Self {
        Self {
            viewer,
            markdown: markdown::Settings::from(theme),
            cards: BTreeMap::new(),
            local_name: None,
        }
    }

    pub fn set_viewer(&mut self, viewer: Viewer) {
        self.viewer = viewer;
    }

    /// The local node's human name (its host name), shown on answers it
    /// gave instead of its key.
    pub fn set_local_name(&mut self, name: Option<String>) {
        self.local_name = name;
    }

    fn names(&self) -> (&str, Option<&str>) {
        (&self.viewer.by, self.local_name.as_deref())
    }

    pub fn set_theme(&mut self, theme: &Theme) {
        self.markdown = markdown::Settings::from(theme);
    }

    /// Brings the cards in line with the session DOM. Text typed into a
    /// waiting card survives; answered, expired and cancelled decisions
    /// become read only.
    pub fn sync(&mut self, dom: &Dom) {
        let mut seen = Vec::new();
        for decision in Decision::all(dom) {
            seen.push(decision.id);
            match self.cards.get_mut(&decision.id) {
                Some(card) => card.refresh(decision),
                None => {
                    self.cards.insert(decision.id, Card::new(decision));
                }
            }
        }
        seen.sort_unstable();
        self.cards.retain(|id, _| seen.binary_search(id).is_ok());
    }

    /// Reopens a card whose answer the harness refused, so it can be
    /// answered again.
    pub fn answer_failed(&mut self, decision: NodeId) {
        if let Some(card) = self.cards.get_mut(&decision) {
            card.pending = false;
        }
    }

    /// Locks a card answered from elsewhere (an unattended timeout) until
    /// the DOM records the answer or [`answer_failed`](Self::answer_failed)
    /// reopens it.
    pub fn answering(&mut self, decision: NodeId) {
        if let Some(card) = self.cards.get_mut(&decision).filter(|c| c.open()) {
            card.pending = true;
        }
    }

    /// The card that takes shortcuts: the oldest one still waiting for the
    /// local user. `None` for view only peers.
    pub fn focused(&self) -> Option<NodeId> {
        if self.viewer.view_only {
            return None;
        }
        self.cards
            .values()
            .find(|c| c.open())
            .map(|c| c.decision.id)
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let none = (Task::none(), None);
        let (id, reply) = match message {
            Message::ToggleDiff(id) => {
                if let Some(card) = self.cards.get_mut(&id) {
                    card.toggle_diff();
                }
                return none;
            }
            Message::Link(uri) => return (Task::none(), Some(Event::OpenLink(uri))),
            Message::Shortcut(shortcut) => {
                let Some(id) = self.focused() else {
                    return none;
                };
                let Some(card) = self.cards.get_mut(&id) else {
                    return none;
                };
                let reply = match shortcut {
                    Shortcut::Pick(index) => {
                        card.pick(index);
                        None
                    }
                    Shortcut::Confirm => card.confirm(),
                    Shortcut::Dismiss => card.dismiss(),
                };
                (id, reply)
            }
            Message::Choose(id, _)
            | Message::Submit(id)
            | Message::Skip(id)
            | Message::Text(id, _)
            | Message::SubmitText(id)
            | Message::EditPlan(id, _)
            | Message::CancelEdit(id)
            | Message::Field(id, _, _) => {
                if self.viewer.view_only {
                    return none;
                }
                let Some(card) = self.cards.get_mut(&id).filter(|c| c.open()) else {
                    return none;
                };
                let reply = match message {
                    Message::Choose(_, index) => card.choose(index),
                    Message::Submit(_) => card.confirm(),
                    Message::Skip(_) => card.dismiss(),
                    Message::Text(_, text) => {
                        card.text = text;
                        None
                    }
                    Message::SubmitText(_) => card.submit_text(),
                    Message::EditPlan(_, action) => {
                        card.edit(action);
                        None
                    }
                    Message::CancelEdit(_) => {
                        card.editor = None;
                        None
                    }
                    Message::Field(_, index, input) => {
                        card.set_field(index, input);
                        None
                    }
                    Message::Shortcut(_) | Message::Link(_) | Message::ToggleDiff(_) => None,
                };
                (id, reply)
            }
        };
        let Some(Reply { values, reason }) = reply else {
            return none;
        };
        if let Some(card) = self.cards.get_mut(&id) {
            card.pending = true;
        }
        let answer = Answer {
            values,
            reason,
            by: self.viewer.by.clone(),
        };
        (
            Task::none(),
            Some(Event::Answer {
                decision: id,
                answer,
            }),
        )
    }

    /// Every card in document order.
    pub fn view(&self) -> Element<'_, Message> {
        let focused = self.focused();
        column(self.cards.values().map(|c| {
            c.view(
                focused == Some(c.decision.id),
                self.viewer.view_only,
                self.markdown,
                self.names(),
            )
        }))
        .spacing(12)
        .into()
    }

    /// The card of one decision, for the chat to place inline.
    pub fn view_card(&self, decision: NodeId) -> Option<Element<'_, Message>> {
        let card = self.cards.get(&decision)?;
        Some(card.view(
            self.focused() == Some(decision),
            self.viewer.view_only,
            self.markdown,
            self.names(),
        ))
    }

    /// Number keys, Enter and Esc for the focused card. Keys a widget
    /// handled (typing into a text field) never arrive here.
    pub fn subscription(&self) -> Subscription<Message> {
        if self.focused().is_none() {
            return Subscription::none();
        }
        keyboard::listen().filter_map(|event| shortcut(&event).map(Message::Shortcut))
    }
}

/// The waiting count badge for the sidebar and top bar. `None` at zero.
pub fn waiting_badge<'a, M: 'a>(count: usize) -> Option<Element<'a, M>> {
    (count > 0).then(|| {
        container(text(count).size(12))
            .padding([1, 6])
            .style(|theme: &Theme| {
                let pair = theme.extended_palette().primary.strong;
                container::Style {
                    background: Some(pair.color.into()),
                    text_color: Some(pair.text),
                    border: iced::border::rounded(8),
                    ..container::Style::default()
                }
            })
            .into()
    })
}

#[cfg(test)]
mod tests;
