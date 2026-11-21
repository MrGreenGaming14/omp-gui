//! Decision cards: approvals, questions, plan reviews and forms of every open
//! session, drawn inline in the transcript and answered through the session's
//! harness (or the sessions area, for plan reviews the app raised itself).

use iced::widget::{column, container, scrollable};
use iced::{Element, Fill, Subscription, Task, Theme};
use loa_view::session::Answer;
use loa_view::session::decisions::{DeskIntent, UnattendedView};
use loa_bus::SessionId;
use loa_core::{ElementKind, NodeId, Op};
use loa_settings::models::login::UrlOpener;
use loa_harness::account_support::opener::SystemUrlOpener;
use loa_ui_session::decisions::{self as decisions, Desk, TIMEOUT_BY, desk, unattended};
use loa_ui_kit::style::{ButtonKind, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, width};
use loa_ui_kit::widgets::{card, label, section_header, text_button};

use crate::area::{perform, Area, AreaCommand, AreaMessage, Ctx, Listed, Request};
use loa_runtime::session::SessionEvent;
use notifier::DesktopNotifier;

mod notifier;

#[derive(Clone, Debug)]
pub enum Message {
    Desk(desk::Message),
    /// The harness took (or refused) an answer.
    Answered { session: String, decision: NodeId, result: Result<(), String> },
    /// An answer given elsewhere (a peer with drive access), routed like a
    /// card's own: the tools area's remote server, an app plan review, or
    /// the session's harness.
    Answer { session: String, decision: NodeId, answer: Answer },
    /// A peer's session this node may only watch (`true`) or also drive.
    ViewOnly(String, bool),
    CloseSettings,
}

pub struct State {
    desk: Desk,
    /// The theme the cards are drawn with.
    theme: Theme,
    /// A failed system notification was reported; later ones stay quiet.
    notify_failed: bool,
}

impl State {
    pub(crate) fn set_theme(&mut self, theme: &Theme) {
        self.desk.set_theme(theme);
        self.theme = theme.clone();
    }

    /// The inline card of `decision` in the active session, for the
    /// transcript slot it sits in.
    pub fn card(&self, session: &str, decision: NodeId) -> Option<Element<'_, Message>> {
        self.desk.view_card(session, decision).map(|card| card.map(Message::Desk))
    }

    /// The card of the active session that takes keyboard shortcuts, if any.
    pub fn focused(&self) -> Option<NodeId> {
        self.desk.focused()
    }

    /// The message that dismisses the focused card (Esc): denies an
    /// approval, rejects a plan, skips a question or form.
    pub fn dismiss() -> Message {
        Message::Desk(desk::Message::Shortcut(decisions::Shortcut::Dismiss))
    }

    /// Brings the cards, badges and timeouts of `session` in line with its
    /// DOM; raises a system notification for new waiting cards while the
    /// window is in the background.
    fn sync(&mut self, session: &SessionId, ctx: &Ctx) -> Task<Message> {
        let Some(session) = ctx.runtime.sessions().get(session.as_str()) else { return Task::none() };
        let theme = ctx.shell.theme();
        if theme != self.theme {
            self.set_theme(&theme);
        }
        let notices = self.desk.sync(session.dom(), desk::now_ms());
        self.desk.notify(notices).map(Message::Desk)
    }

    fn answer(&mut self, session: String, decision: NodeId, answer: Answer, ctx: &mut Ctx) -> Task<Message> {
        let Some(open) = ctx.runtime.sessions().get(&session) else {
            self.desk.answer_failed(&session, decision);
            return Task::none();
        };
        // Remote host approvals and host key questions belong to the tools
        // area's `remote` server, not the harness.
        if crate::areas::tools::is_remote_decision(open.dom(), decision) {
            let answer = crate::areas::tools::Message::AnswerDecision { session: open.id().clone(), decision, answer };
            ctx.request(Request::Forward(Box::new(crate::app::Message::area(answer))));
            return Task::none();
        }
        if loa_session::plan::is_app_plan(open.dom(), decision) {
            let answer = crate::areas::sessions::Message::AnswerPlan { session: open.id().clone(), decision, answer };
            ctx.request(Request::Forward(Box::new(crate::app::Message::area(answer))));
            return Task::none();
        }
        let Some(harness) = open.harness().cloned() else {
            self.desk.answer_failed(&session, decision);
            // A timeout in a session nobody runs isn't worth a notice.
            if answer.by != TIMEOUT_BY {
                ctx.notify("This session's harness isn't running; send a prompt to resume it, then answer");
            }
            return Task::none();
        };
        perform(async move { harness.answer(decision, answer).await }, move |result| {
            Message::Answered { session, decision, result }
        })
    }

    fn save(policy: UnattendedView, ctx: &mut Ctx) {
        ctx.edit_config("unattended prompt settings", |config| config.unattended = crate::view::session::unattended_from_view(policy));
    }

    fn set_unattended(&mut self, enabled: bool, ctx: &mut Ctx) -> Task<Message> {
        self.update(Message::Desk(desk::Message::Settings(unattended::Message::Enabled(enabled))), ctx)
    }
}

impl AreaMessage for Message {
    type Area = State;
}

impl Area for State {
    const ID: &'static str = "decisions";
    const COMMANDS: &'static [AreaCommand<Self>] = &[
        AreaCommand::<Self>::new("decisions.settings", "Unattended prompt settings", "Settings", |_, _, ctx| {
            ctx.show_screen(Self::ID);
            Task::none()
        }),
        AreaCommand::<Self>::new("decisions.unattended.on", "", "Settings", |state, _, ctx| state.set_unattended(true, ctx)).listed(Listed::When(
            |state, _| (!state.desk.policy().enabled()).then_some("Unattended prompts: deny approvals and reject plans after 5 min"),
        )),
        AreaCommand::<Self>::new("decisions.unattended.off", "", "Settings", |state, _, ctx| state.set_unattended(false, ctx))
            .listed(Listed::When(|state, _| state.desk.policy().enabled().then_some("Unattended prompts: wait for someone"))),
    ];

    type Message = Message;

    fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let policy = crate::view::session::unattended_to_view(&ctx.read_config("unattended prompt settings").unwrap_or_default().unattended);
        let theme = ctx.shell.theme();
        let mut desk = Desk::new(ctx.services.node.key.clone(), &theme, policy, DesktopNotifier::new("loa"));
        desk.set_local_name(ctx.services.node.label.clone());
        (Self { desk, theme, notify_failed: false }, Task::none())
    }

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            // Keys answer the active session's card only while it shows;
            // Esc closes a screen over it first.
            Message::Desk(desk::Message::Shortcut(shortcut)) if ctx.screen().is_some() => {
                if shortcut == decisions::Shortcut::Dismiss {
                    ctx.close_screen();
                }
                Task::none()
            }
            Message::Desk(message) => {
                let mut tasks = Vec::new();
                for event in self.desk.update(message) {
                    match event {
                        DeskIntent::Answer { session, decision, answer } => {
                            tasks.push(self.answer(session, decision, answer, ctx));
                        }
                        DeskIntent::OpenLink(url) => {
                            if let Err(error) = SystemUrlOpener.open_url(&url) {
                                ctx.notify(error);
                            }
                        }
                        DeskIntent::PolicyChanged(policy) => Self::save(policy, ctx),
                        DeskIntent::NotifyFailed(error) => {
                            if !self.notify_failed {
                                self.notify_failed = true;
                                ctx.notify(format!("System notifications are unavailable: {error}"));
                            }
                        }
                    }
                }
                Task::batch(tasks)
            }
            Message::Answered { session, decision, result } => {
                if let Err(error) = result {
                    self.desk.answer_failed(&session, decision);
                    ctx.notify(format!("The answer wasn't taken: {error}"));
                }
                Task::none()
            }
            Message::Answer { session, decision, answer } => self.answer(session, decision, answer, ctx),
            Message::ViewOnly(session, view_only) => {
                self.desk.set_view_only(&session, view_only);
                Task::none()
            }
            Message::CloseSettings => {
                ctx.close_screen();
                Task::none()
            }
        }
    }

    fn view_screen<'a>(&'a self, _ctx: &'a Ctx) -> Option<Element<'a, Message>> {
        let waiting = match self.desk.total() {
            0 => "No prompt is waiting.".to_owned(),
            1 => "1 prompt is waiting.".to_owned(),
            n => format!("{n} prompts are waiting."),
        };
        let header = section_header("Unattended prompts", [text_button("Done", ButtonKind::Secondary, Some(Message::CloseSettings))]);
        let body = column![header, card(self.desk.settings_view().map(Message::Desk)).width(Fill), label(waiting, TextRole::Caption, Tone::Muted)];
        let body = body.spacing(sp(SpaceRole::L)).max_width(width(LayoutRole::ContentWidth));
        Some(scrollable(container(body).center_x(Fill).padding(sp(SpaceRole::Xl))).height(Fill).into())
    }

    fn on_session_event(&mut self, event: &SessionEvent, ctx: &mut Ctx) -> Task<Message> {
        match event {
            SessionEvent::Activated(id) => {
                self.desk.set_active(id.as_ref().map(ToString::to_string));
                id.as_ref().map_or_else(Task::none, |id| self.sync(id, ctx))
            }
            SessionEvent::Loaded(id) => self.sync(id, ctx),
            SessionEvent::Patch(id, patch) => {
                let touches_decision = match &patch.op {
                    Op::Insert { kind, .. } => *kind == ElementKind::Decision,
                    Op::Set { .. } => ctx.runtime.sessions()
                        .get(id.as_str())
                        .and_then(|session| session.dom().get(patch.node))
                        .is_some_and(|node| node.kind == ElementKind::Decision),
                    Op::Remove => true,
                    Op::Append { .. } => false,
                };
                if touches_decision { self.sync(id, ctx) } else { Task::none() }
            }
            SessionEvent::Closed(id) => {
                self.desk.forget(id.as_str());
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        self.desk.subscription().map(Message::Desk)
    }
}

#[cfg(test)]
mod tests;
