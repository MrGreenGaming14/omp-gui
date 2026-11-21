//! Decision prompt cards: approval, question, plan review and form. [`Desk`] is the app-facing
//! component over every open session; [`State`] holds the cards of one session. Both report
//! what the app must do as [`loa_view::session::decisions`] intents.

mod card;
pub mod desk;
pub mod form;
mod model;
mod settings;
pub mod unattended;

pub use card::{CardKind, FieldInput, form_field_id, kind_for, text_field_id};
pub use desk::{Desk, Tracker};
pub use model::{ApprovalChoice, Choice, Decision, PlanChoice, answer_summary};
pub use unattended::Unattended;

use card::{Card, CardCtx, Reply};
use iced::keyboard::{self, key};
use iced::widget::{column, markdown, text_editor};
use iced::{Element, Subscription, Task, Theme};
use loa_core::{Dom, NodeId};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole, TokenSet, for_theme, generation, sp, tokens};
use loa_view::session::Answer;
use loa_view::session::decisions::Intent;
use std::collections::BTreeMap;

/// `by` of an answer applied because an unattended timeout passed.
pub const TIMEOUT_BY: &str = "timeout";

/// How `by` reads on a card: the local node's key as `local_name`, other node keys (long
/// alphanumeric ids) as their first 8 characters, anything else as is.
pub fn node_name<'a>(by: &'a str, local: &str, local_name: Option<&'a str>) -> &'a str {
    match local_name.filter(|name| !name.is_empty() && by == local) {
        Some(name) => name,
        None if by.len() >= 32 && by.bytes().all(|b| b.is_ascii_alphanumeric()) => &by[..8],
        None => by,
    }
}

/// Who looks at the cards: `by` is recorded on answers; view-only peers can't answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Viewer {
    pub by: String,
    pub view_only: bool,
}

/// A keyboard shortcut for the focused card: digit `n` picks option `n - 1`, Enter confirms,
/// Esc skips, denies or rejects.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Shortcut {
    Pick(usize),
    Confirm,
    Dismiss,
}

/// Maps a key press to a card shortcut; presses with Ctrl, Alt or Logo are left to the app.
pub fn shortcut(event: &keyboard::Event) -> Option<Shortcut> {
    let keyboard::Event::KeyPressed { key, modifiers, .. } = event else { return None };
    if modifiers.control() || modifiers.alt() || modifiers.logo() {
        return None;
    }
    match key.as_ref() {
        keyboard::Key::Named(key::Named::Enter) => Some(Shortcut::Confirm),
        keyboard::Key::Named(key::Named::Escape) => Some(Shortcut::Dismiss),
        keyboard::Key::Character(c) => c.parse::<usize>().ok().filter(|n| (1..=9).contains(n)).map(|n| Shortcut::Pick(n - 1)),
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
}

/// The decision cards of one session.
pub struct State {
    viewer: Viewer,
    /// Plan markdown settings of one token generation; rebuilt when it changes.
    markdown: std::cell::Cell<(u64, markdown::Settings)>,
    /// By node id; ids only go up, so the first open card is the oldest.
    cards: BTreeMap<NodeId, Card>,
    local_name: Option<String>,
}

fn markdown_settings(t: &TokenSet) -> markdown::Settings { markdown::Settings::with_text_size(t.text(TextRole::Body), t.palette()) }

impl State {
    pub fn new(viewer: Viewer, theme: &Theme) -> Self { Self { viewer, markdown: (generation(), markdown_settings(&for_theme(theme))).into(), cards: BTreeMap::new(), local_name: None } }
    pub fn set_viewer(&mut self, viewer: Viewer) { self.viewer = viewer }
    /// The local node's host name, shown on its answers instead of its key.
    pub fn set_local_name(&mut self, name: Option<String>) { self.local_name = name }
    pub fn set_theme(&mut self, theme: &Theme) { self.markdown.set((generation(), markdown_settings(&for_theme(theme)))) }

    fn cx(&self, decision: NodeId) -> CardCtx<'_> {
        let (g, mut settings) = self.markdown.get();
        if g != generation() {
            settings = markdown_settings(&tokens());
            self.markdown.set((generation(), settings));
        }
        CardCtx { focused: self.focused() == Some(decision), view_only: self.viewer.view_only, markdown: settings, names: (&self.viewer.by, self.local_name.as_deref()) }
    }

    /// Brings the cards in line with the DOM, keeping what was typed into waiting cards.
    pub fn sync(&mut self, dom: &Dom) {
        let mut seen = Vec::new();
        for decision in Decision::all(dom) {
            seen.push(decision.id);
            match self.cards.get_mut(&decision.id) {
                Some(card) => card.refresh(decision),
                None => drop(self.cards.insert(decision.id, Card::new(decision))),
            }
        }
        self.cards.retain(|id, _| seen.contains(id));
    }

    /// Reopens a card whose answer the harness refused.
    pub fn answer_failed(&mut self, decision: NodeId) { self.cards.get_mut(&decision).into_iter().for_each(|c| c.pending = false) }
    /// Locks a card answered from elsewhere (an unattended timeout) until the DOM records it.
    pub fn answering(&mut self, decision: NodeId) { self.cards.get_mut(&decision).filter(|c| c.open()).into_iter().for_each(|c| c.pending = true) }

    /// The card that takes shortcuts: the oldest one waiting for the local user.
    pub fn focused(&self) -> Option<NodeId> {
        if self.viewer.view_only { None } else { self.cards.values().find(|c| c.open()).map(|c| c.decision.id) }
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Intent>) {
        let id = match &message {
            Message::Link(uri) => return (Task::none(), Some(Intent::OpenLink(uri.clone()))),
            Message::Shortcut(_) => self.focused(),
            Message::Choose(id, _) | Message::Submit(id) | Message::Skip(id) | Message::Text(id, _) | Message::SubmitText(id) | Message::EditPlan(id, _) | Message::CancelEdit(id) | Message::Field(id, ..) => {
                (!self.viewer.view_only).then_some(*id)
            }
        };
        let Some((id, card)) = id.and_then(|id| Some((id, self.cards.get_mut(&id).filter(|c| c.open())?))) else { return (Task::none(), None) };
        let kind = card.kind();
        let reply = match message {
            Message::Shortcut(Shortcut::Pick(index)) => {
                card.pick(index);
                None
            }
            Message::Shortcut(Shortcut::Confirm) | Message::Submit(_) => kind.confirm(card),
            Message::Shortcut(Shortcut::Dismiss) | Message::Skip(_) => kind.dismiss(card),
            Message::Choose(_, index) => card.choose(index),
            Message::SubmitText(_) => kind.submit_text(card),
            Message::Text(_, text) => {
                card.text = text;
                None
            }
            Message::EditPlan(_, action) => {
                if let Some(editor) = &mut card.editor {
                    editor.perform(action);
                }
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
            Message::Link(_) => None,
        };
        let Some(Reply { values, reason }) = reply else { return (Task::none(), None) };
        card.pending = true;
        (Task::none(), Some(Intent::Answer { decision: id, answer: Answer { values, reason, by: self.viewer.by.clone() } }))
    }

    /// Every card in document order.
    pub fn view(&self) -> Element<'_, Message> { column(self.cards.values().map(|c| c.view(&self.cx(c.decision.id)))).spacing(sp(Sp::M)).into() }
    /// The card of one decision, for the transcript to place inline.
    pub fn view_card(&self, decision: NodeId) -> Option<Element<'_, Message>> { Some(self.cards.get(&decision)?.view(&self.cx(decision))) }

    /// Number keys, Enter and Esc for the focused card; keys a text field took never arrive.
    pub fn subscription(&self) -> Subscription<Message> {
        if self.focused().is_none() { Subscription::none() } else { keyboard::listen().filter_map(|event| shortcut(&event).map(Message::Shortcut)) }
    }
}
