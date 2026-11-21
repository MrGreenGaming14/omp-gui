//! A peer's session opened on this node: a live, read-only mirror kept
//! current over `omp/session/1`. Drive grants prompt the owner's harness and
//! answer its decision cards; fork grants copy the session to this node. The
//! owner stays the single writer: nothing here writes the mirror.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use iced::futures::{SinkExt, Stream};
use iced::widget::{button, column, container, row, text, text_input};
use iced::{Alignment, Element, Fill, Padding, Subscription, Task, Theme};
use loa_bus::{Event, Policy, SessionWriter};
use loa_core::{Dom, NodeId, Op, Patch, attr};
use loa_grants::Action;
use loa_models::login::UrlOpener;
use loa_models::opener::SystemUrlOpener;
use loa_node::RemoteView;
use loa_ui_chat as chat;
use loa_ui_decisions as cards;
use loa_ui_kit::icons::{Icon, icon};
use loa_ui_peers as peers;
use loa_ui_shell::SessionSummary;
use loa_ui_shell::theme::style;

use super::{Message, State};
use crate::area::{AreaId, Ctx};

/// Width of the prompt box: the transcript's column width.
const PROMPT_WIDTH: f32 = 700.0;

/// Patches the mirror's feed may hold before it resubscribes.
const FEED_CAPACITY: usize = 4096;

/// What the mirror's feed delivers.
#[derive(Clone, Debug)]
pub enum Feed {
    /// The mirror's DOM when the feed (re)subscribed.
    Synced(Arc<Dom>),
    Patch(Arc<Patch>),
    /// The view stopped following the owner.
    Ended,
}

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

    #[cfg(test)]
    pub fn dom(&self) -> &Dom {
        &self.dom
    }

    #[cfg(test)]
    pub fn view_handle(&self) -> &Arc<RemoteView> {
        &self.view
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
            Feed::Patch(patch) => {
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

    pub fn subscription(&self) -> Subscription<RemoteMessage> {
        Subscription::run_with(Watch(Arc::clone(self.view.writer())), feed).map(RemoteMessage::Feed)
    }

    pub fn view(&self) -> Element<'_, RemoteMessage> {
        let access: Vec<&str> = self.view.actions().iter().map(|action| action.as_str()).collect();
        let harness = self.dom.root().and_then(|root| root.str_attr(attr::HARNESS)).unwrap_or("harness");
        let subtitle = format!(
            "{} · {harness} · {}",
            self.peer_name,
            if access.is_empty() { "some prompts".to_owned() } else { access.join(", ") }
        );
        let fork = self.view.can(Action::Fork).then(|| {
            button(text("Fork to this node").size(13)).style(button::secondary).on_press(RemoteMessage::Fork)
        });
        let header = row![
            column![text(self.title()).size(20), text(subtitle).size(12).style(style::muted)].spacing(4).width(Fill),
        ]
        .push(fork)
        .push(button(icon(Icon::X, 16.0)).style(button::text).on_press(RemoteMessage::Close))
        .spacing(12)
        .align_y(Alignment::Center);
        let transcript = self.chat.view(&self.dom, RemoteMessage::Chat, |slot| {
            self.cards
                .view_card(slot.node)
                .map(|card| card.map(RemoteMessage::Card))
                .unwrap_or_else(|| text("Decision no longer available").into())
        });
        let footer: Element<'_, RemoteMessage> = if let Some(reason) = &self.ended {
            text(format!("No longer following {}: {reason}", self.peer_name)).size(12).style(style::muted).into()
        } else if self.can_drive() {
            let ready = !self.sending && !self.input.trim().is_empty();
            row![
                text_input(&format!("Prompt {}'s harness", self.peer_name), &self.input)
                    .on_input(RemoteMessage::Input)
                    .on_submit_maybe(ready.then_some(RemoteMessage::Send))
                    .style(style::input)
                    .padding(10)
                    .size(14),
                button(icon(Icon::ArrowUp, 16.0)).style(button::primary).padding(10).on_press_maybe(ready.then_some(RemoteMessage::Send)),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
        } else {
            text(format!("View only: {}'s grant doesn't let this node prompt or answer", self.peer_name)).size(12).style(style::muted).into()
        };
        column![
            container(header).padding([16, 24]),
            container(transcript).height(Fill),
            container(container(footer).width(Fill).max_width(PROMPT_WIDTH))
                .center_x(Fill)
                .padding(Padding { top: 8.0, right: 24.0, bottom: 16.0, left: 24.0 }),
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
                    Some(chat::Event::Rewind(_)) => ctx.notify("Only the owner rewinds this session; fork it to rewind a copy on this node"),
                    Some(chat::Event::OpenLink(url)) => {
                        if let Err(error) = SystemUrlOpener.open_url(&url) {
                            ctx.notify(error);
                        }
                    }
                    None => {}
                }
                task.map(|message| Message::Remote(RemoteMessage::Chat(message)))
            }
            RemoteMessage::Card(message) => {
                // Card keys act only while this screen shows.
                if matches!(message, cards::Message::Shortcut(_)) && ctx.screen() != Some(AreaId::Peers) {
                    return Task::none();
                }
                let (task, event) = remote.cards.update(message);
                let answer = match event {
                    Some(cards::Event::Answer { decision, answer }) => {
                        let view = Arc::clone(&remote.view);
                        Task::perform(
                            async move { view.answer(decision, answer.values, answer.reason).await.map_err(|error| error.to_string()) },
                            move |result| Message::Remote(RemoteMessage::Answered(decision, result)),
                        )
                    }
                    Some(cards::Event::OpenLink(url)) => {
                        if let Err(error) = SystemUrlOpener.open_url(&url) {
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
                Task::perform(async move { view.prompt(&text).await.map_err(|error| error.to_string()) }, |result| {
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
                let event = peers::Event::ForkRemote { peer: remote.view.peer().to_string(), session: remote.view.session().to_owned() };
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

/// Identifies the mirror's feed: another view gets a new one.
pub(crate) struct Watch(pub(crate) Arc<SessionWriter>);

impl Hash for Watch {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

/// The mirror's DOM, then each patch; resubscribes after falling behind and
/// reports the end when the view stops following the owner.
pub(crate) fn feed(watch: &Watch) -> impl Stream<Item = Feed> + use<> {
    let writer = Arc::clone(&watch.0);
    iced::stream::channel(64, async move |mut out: iced::futures::channel::mpsc::Sender<Feed>| {
        loop {
            let (dom, mut subscription) = writer.subscribe(Policy::every_patch(FEED_CAPACITY));
            if out.send(Feed::Synced(Arc::new(dom))).await.is_err() {
                return;
            }
            loop {
                match subscription.recv().await {
                    None => {
                        let _ = out.send(Feed::Ended).await;
                        return;
                    }
                    Some(Event::Lagged { .. }) => break,
                    Some(Event::Patch(patch)) => {
                        if out.send(Feed::Patch(patch)).await.is_err() {
                            return;
                        }
                    }
                    Some(Event::Appends(_)) => break,
                }
            }
        }
    })
}
