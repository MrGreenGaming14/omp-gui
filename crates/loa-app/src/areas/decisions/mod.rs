//! Decision cards: approvals, questions, plan reviews and forms of every open
//! session, drawn inline in the transcript and answered through the session's
//! harness (or the sessions area, for plan reviews the app raised itself).
//! Also the waiting notifications while the window is in the background, the
//! unattended timeouts and their settings screen.

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Element, Fill, Subscription, Task, Theme};
use loa_contracts::harness::Answer;
use loa_bus::SessionId;
use loa_core::{ElementKind, NodeId, Op};
use loa_models::login::UrlOpener;
use loa_models::opener::SystemUrlOpener;
use loa_ui_session::decisions::{self as decisions, Desk, DesktopNotifier, TIMEOUT_BY, Unattended, desk, unattended};
use loa_ui_palette as palette;
use loa_ui_shell::theme::style;

use crate::area::{Area, AreaId, Ctx, Request, SessionEvent};

/// Width of the settings column on the settings screen.
const SETTINGS_WIDTH: f32 = 720.0;

const SETTINGS: &str = "settings";
const UNATTENDED_ON: &str = "unattended.on";
const UNATTENDED_OFF: &str = "unattended.off";

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
    pub fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let policy = Unattended(ctx.read_config("unattended prompt settings").unwrap_or_default().unattended);
        let theme = ctx.shell.theme();
        let mut desk = Desk::new(ctx.services.node.key.clone(), &theme, policy, DesktopNotifier::new("loa"));
        desk.set_local_name(ctx.services.node.label.clone());
        (Self { desk, theme, notify_failed: false }, Task::none())
    }

    pub(crate) fn set_theme(&mut self, theme: &Theme) {
        self.desk.set_theme(theme);
        self.theme = theme.clone();
    }

    #[cfg(test)]
    pub(crate) fn theme(&self) -> &Theme {
        &self.theme
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
        let Some(session) = ctx.sessions.get(session.as_str()) else { return Task::none() };
        let theme = ctx.shell.theme();
        if theme != self.theme {
            self.set_theme(&theme);
        }
        let notices = self.desk.sync(session.dom(), desk::now_ms());
        self.desk.notify(notices).map(Message::Desk)
    }

    fn answer(&mut self, session: String, decision: NodeId, answer: Answer, ctx: &mut Ctx) -> Task<Message> {
        let Some(open) = ctx.sessions.get(&session) else {
            self.desk.answer_failed(&session, decision);
            return Task::none();
        };
        // Remote host approvals and host key questions belong to the tools
        // area's `remote` server, not the harness.
        if crate::areas::tools::is_remote_decision(open.dom(), decision) {
            let answer = crate::areas::tools::Message::AnswerDecision { session: open.id().clone(), decision, answer };
            ctx.request(Request::Forward(Box::new(crate::app::Message::Tools(answer))));
            return Task::none();
        }
        if loa_features::plan::is_app_plan(open.dom(), decision) {
            let answer = crate::areas::sessions::Message::AnswerPlan { session: open.id().clone(), decision, answer };
            ctx.request(Request::Forward(Box::new(crate::app::Message::Sessions(answer))));
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
        Task::perform(async move { harness.answer(decision, answer).await.map_err(|error| error.to_string()) }, move |result| {
            Message::Answered { session, decision, result }
        })
    }

    fn save(policy: &Unattended, ctx: &mut Ctx) {
        ctx.edit_config("unattended prompt settings", |config| config.unattended = policy.0.clone());
    }
}

impl Area for State {
    type Message = Message;

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
                        desk::Event::Answer { session, decision, answer } => {
                            tasks.push(self.answer(session, decision, answer, ctx));
                        }
                        desk::Event::OpenLink(url) => {
                            if let Err(error) = SystemUrlOpener.open_url(&url) {
                                ctx.notify(error);
                            }
                        }
                        desk::Event::PolicyChanged(policy) => Self::save(&policy, ctx),
                        desk::Event::NotifyFailed(error) => {
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

    fn palette_commands(&self, _ctx: &Ctx) -> Vec<palette::Command> {
        let toggle = if self.desk.policy().enabled() {
            palette::Command::new(AreaId::Decisions.command(UNATTENDED_OFF), "Unattended prompts: wait for someone", "Settings")
        } else {
            palette::Command::new(
                AreaId::Decisions.command(UNATTENDED_ON),
                "Unattended prompts: deny approvals and reject plans after 5 min",
                "Settings",
            )
        };
        vec![palette::Command::new(AreaId::Decisions.command(SETTINGS), "Unattended prompt settings", "Settings"), toggle]
    }

    fn run_command(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Message>> {
        let name = id.strip_prefix(AreaId::Decisions.key())?.strip_prefix('.')?;
        let enabled = match name {
            SETTINGS => {
                ctx.show_screen(AreaId::Decisions);
                return Some(Task::none());
            }
            UNATTENDED_ON => true,
            UNATTENDED_OFF => false,
            _ => return None,
        };
        Some(self.update(Message::Desk(desk::Message::Settings(unattended::Message::Enabled(enabled))), ctx))
    }

    fn view_screen<'a>(&'a self, _ctx: &'a Ctx) -> Option<Element<'a, Message>> {
        let header = row![
            text("Unattended prompts").size(20).width(Fill),
            button(text("Done").size(13)).style(button::secondary).on_press(Message::CloseSettings),
        ]
        .align_y(iced::alignment::Vertical::Center);
        let waiting = match self.desk.total() {
            0 => "No prompt is waiting.".to_owned(),
            1 => "1 prompt is waiting.".to_owned(),
            n => format!("{n} prompts are waiting."),
        };
        let body = column![
            header,
            container(self.desk.settings_view().map(Message::Desk)).style(style::card).padding(16).width(Fill),
            text(waiting).size(12).style(style::muted),
        ]
        .spacing(16)
        .max_width(SETTINGS_WIDTH);
        Some(scrollable(container(column![space().height(24), body]).center_x(Fill).padding([0, 24])).height(Fill).into())
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
                    Op::Set { .. } => ctx
                        .sessions
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
