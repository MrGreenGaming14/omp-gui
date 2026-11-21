//! A peer's session opened on this node: a live, read-only mirror kept
//! current over `omp/session/1`.

use std::sync::Arc;

use iced::widget::{button, column, container, row, text, text_input};
use iced::{Alignment, Element, Fill, Subscription, Task, Theme};
use loa_bus::{Event, Policy};
use loa_core::{Dom, NodeId, Op, attr};
use loa_trust::Action;
use loa_p2p::RemoteView;
use loa_ui_session::transcript as chat;
use loa_ui_session::decisions as cards;
use loa_ui_kit::icons::tinted;
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::widgets::{icon_button, label, text_button};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, text_size as ts, width};
use loa_view::session::{decisions, transcript};
use loa_view::settings::peers::Intent;
use loa_view::theme::Icon;
use loa_contracts::session::SessionSummary;

use super::{Message, State};
use crate::area::{perform, Area, Ctx};
use loa_runtime::session::{Feed, WriterFeed};
use crate::watch::{PtrKey, watch};

/// Patches the mirror's feed may hold before it resubscribes.
const FEED_CAPACITY: usize = 4096;

#[derive(Clone, Debug)]
pub enum RemoteMessage {
    Feed(Feed),
    Chat(chat::Message),
    Card(cards::Message),
    Input(String),
    /// Send the typed prompt to the owner's harness.
    Send,
    Sent(Result<(), String>),
    Answered(NodeId, Result<(), String>),
    Fork,
    Close,
}

/// The peer's session on screen.
pub struct Remote {
    view: Arc<RemoteView>,
    peer_name: String,
    /// The UI's copy of the mirror.
    dom: Dom,
    chat: chat::State,
    cards: cards::State,
    input: String,
    /// A prompt is on its way to the owner.
    sending: bool,
    /// Why the view stopped following the owner.
    pub(crate) ended: Option<String>,
}

impl Remote {
    /// The view of `view`; `by` is this node's key, recorded on answers.
    pub fn new(view: Arc<RemoteView>, peer_name: String, by: String, theme: &Theme) -> Self {
        let dom = view.writer().with_dom(Dom::clone);
        let mut chat = chat::State::new(theme);
        chat.load(&dom);
        let mut cards = cards::State::new(cards::Viewer { by, view_only: !view.can(Action::Drive) }, theme);
        cards.sync(&dom);
        Self { view, peer_name, dom, chat, cards, input: String::new(), sending: false, ended: None }
    }

    /// Whether this shows `session` of `peer`.
    pub fn is(&self, peer: &str, session: &str) -> bool {
        self.view.peer().to_string() == peer && self.view.session() == session
    }

    /// The title Recents shows: the `title` attribute, else the first prompt.
    pub fn title(&self) -> String {
        SessionSummary::from_dom(&self.dom, false)
            .map_or_else(|| format!("Session {}", self.view.session()), |summary| summary.title)
    }

    fn reload(&mut self, dom: Dom) {
        self.dom = dom;
        self.chat.load(&self.dom);
        self.cards.sync(&self.dom);
    }

    fn feed(&mut self, feed: Feed) {
        match feed {
            Feed::Synced(dom) => {
                if dom.seq() != self.dom.seq() {
                    self.reload(Arc::unwrap_or_clone(dom));
                }
            }
            Feed::Event(Event::Patch(patch)) => {
                if patch.seq <= self.dom.seq() {
                    return;
                }
                if self.dom.apply(&patch).is_err() {
                    // The copy fell out of step: take the mirror's DOM.
                    let dom = self.view.writer().with_dom(Dom::clone);
                    self.reload(dom);
                    return;
                }
                self.chat.apply(&patch, &self.dom);
                if !matches!(patch.op, Op::Append { .. }) {
                    self.cards.sync(&self.dom);
                }
            }
            // Merged appends only reach coalescing feeds; take the DOM.
            Feed::Event(_) => self.reload(self.view.writer().with_dom(Dom::clone)),
            Feed::Ended => {
                self.ended = Some(self.view.ended().unwrap_or_else(|| "the owner closed the session".to_owned()));
                self.cards.set_viewer(cards::Viewer { by: String::new(), view_only: true });
                self.cards.sync(&self.dom);
            }
        }
    }

    fn can_drive(&self) -> bool {
        self.ended.is_none() && self.view.can(Action::Drive)
    }

    /// The mirror's DOM, then each patch; resubscribes after falling behind
    /// and reports the end when the view stops following the owner.
    pub fn subscription(&self) -> Subscription<RemoteMessage> {
        let writer = Arc::clone(self.view.writer());
        watch(PtrKey(self.view.writer()), move || WriterFeed::new(writer, Policy::every_patch(FEED_CAPACITY)), |feed| Some(RemoteMessage::Feed(feed)))
    }

    pub fn view(&self) -> Element<'_, RemoteMessage> {
        let access: Vec<&str> = self.view.actions().iter().map(|action| action.as_str()).collect();
        let harness = self.dom.root().and_then(|root| root.str_attr(attr::HARNESS)).unwrap_or("harness");
        let subtitle = format!(
            "{} · {harness} · {}",
            self.peer_name,
            if access.is_empty() { "some prompts".to_owned() } else { access.join(", ") }
        );
        let fork = self.view.can(Action::Fork).then(|| text_button("Fork to this node", ButtonKind::Secondary, Some(RemoteMessage::Fork)));
        let title = column![label(self.title(), TextRole::Title, Tone::Normal), label(subtitle, TextRole::Caption, Tone::Muted)].spacing(sp(SpaceRole::Xs)).width(Fill);
        let header = row![title].push(fork).push(icon_button(Icon::X, "Close", Some(RemoteMessage::Close))).spacing(sp(SpaceRole::M)).align_y(Alignment::Center);
        let transcript = self.chat.view(&self.dom, RemoteMessage::Chat, |slot| {
            self.cards
                .view_card(slot.node)
                .map(|card| card.map(RemoteMessage::Card))
                .unwrap_or_else(|| text("Decision no longer available").into())
        });
        let footer: Element<'_, RemoteMessage> = if let Some(reason) = &self.ended {
            label(format!("No longer following {}: {reason}", self.peer_name), TextRole::Caption, Tone::Muted).into()
        } else if self.can_drive() {
            let ready = !self.sending && !self.input.trim().is_empty();
            let input = text_input(&format!("Prompt {}'s harness", self.peer_name), &self.input).on_input(RemoteMessage::Input).on_submit_maybe(ready.then_some(RemoteMessage::Send));
            let send = button(tinted(Icon::ArrowUp, TextRole::Heading, Tone::OnAccent)).style(style::button(ButtonKind::Primary)).padding(sp(SpaceRole::S)).on_press_maybe(ready.then_some(RemoteMessage::Send));
            row![input.style(style::input).padding(sp(SpaceRole::S)).size(ts(TextRole::Body)), send].spacing(sp(SpaceRole::S)).align_y(Alignment::Center).into()
        } else {
            label(format!("View only: {}'s grant doesn't let this node prompt or answer", self.peer_name), TextRole::Caption, Tone::Muted).into()
        };
        column![
            container(header).padding([sp(SpaceRole::L), sp(SpaceRole::Xl)]),
            container(transcript).height(Fill),
            container(container(footer).width(Fill).max_width(width(LayoutRole::ComposerWidth)))
                .center_x(Fill)
                .padding(loa_ui_kit::theme::pad_x(SpaceRole::Xl).top(sp(SpaceRole::S)).bottom(sp(SpaceRole::L))),
        ]
        .into()
    }
}

impl State {
    pub(super) fn remote_update(&mut self, message: RemoteMessage, ctx: &mut Ctx) -> Task<Message> {
        let Some(remote) = &mut self.remote else { return Task::none() };
        match message {
            RemoteMessage::Feed(feed) => {
                remote.feed(feed);
                Task::none()
            }
            RemoteMessage::Chat(message) => {
                let (task, event) = remote.chat.update(message);
                match event {
                    Some(transcript::Intent::Rewind(_)) => ctx.notify("Only the owner rewinds this session; fork it to rewind a copy on this node"),
                    Some(transcript::Intent::OpenLink(url)) => {
                        if let Err(error) = ctx.backends.opener.open_url(&url) {
                            ctx.notify(error);
                        }
                    }
                    None => {}
                }
                task.map(|message| Message::Remote(RemoteMessage::Chat(message)))
            }
            RemoteMessage::Card(message) => {
                // Card keys act only while this screen shows.
                if matches!(message, cards::Message::Shortcut(_)) && ctx.screen() != Some(super::State::ID) {
                    return Task::none();
                }
                let (task, event) = remote.cards.update(message);
                let answer = match event {
                    Some(decisions::Intent::Answer { decision, answer }) => {
                        let view = Arc::clone(&remote.view);
                        perform(
            async move { view.answer(decision, answer.values, answer.reason).await },
                            move |result| Message::Remote(RemoteMessage::Answered(decision, result)),
                        )
                    }
                    Some(decisions::Intent::OpenLink(url)) => {
                        if let Err(error) = ctx.backends.opener.open_url(&url) {
                            ctx.notify(error);
                        }
                        Task::none()
                    }
                    None => Task::none(),
                };
                Task::batch([task.map(|message| Message::Remote(RemoteMessage::Card(message))), answer])
            }
            RemoteMessage::Answered(decision, result) => {
                if let Err(error) = result {
                    remote.cards.answer_failed(decision);
                    ctx.notify(format!("{} didn't take the answer: {error}", remote.peer_name));
                }
                Task::none()
            }
            RemoteMessage::Input(input) => {
                remote.input = input;
                Task::none()
            }
            RemoteMessage::Send => {
                let text = remote.input.trim().to_owned();
                if text.is_empty() || remote.sending || !remote.can_drive() {
                    return Task::none();
                }
                remote.sending = true;
                let view = Arc::clone(&remote.view);
                perform(async move { view.prompt(&text).await }, |result| {
                    Message::Remote(RemoteMessage::Sent(result))
                })
            }
            RemoteMessage::Sent(result) => {
                remote.sending = false;
                match result {
                    Ok(()) => remote.input.clear(),
                    Err(error) => ctx.notify(format!("{} didn't take the prompt: {error}", remote.peer_name)),
                }
                Task::none()
            }
            RemoteMessage::Fork => {
                let event = Intent::ForkRemote { peer: remote.view.peer().to_string(), session: remote.view.session().to_owned() };
                self.apply(event, ctx)
            }
            RemoteMessage::Close => {
                self.remote = None;
                ctx.close_screen();
                Task::none()
            }
        }
    }
}
