//! Detached windows: a panel tab (browser surface, terminal, changes, files,
//! agents, tools) or a subagent's session shown in its own OS window.
//!
//! The app runs as an iced daemon: one main window plus any number of
//! detached ones. A detached tab leaves a placeholder in the panel with Show
//! window and Reattach here. Closing a detached window, or its Reattach
//! button, puts the view back; closing the main window quits.

use iced::widget::{button, column, container, row, rule, text};
use iced::{Alignment, Element, Fill, Size, Task, window};
use loa_ui_kit::icons::{Icon, muted};
use loa_ui_shell::theme::style;

use super::{App, Message};

/// Size of the main window when the app starts.
pub const MAIN_SIZE: Size = Size::new(1440.0, 900.0);

/// What a detached window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detached {
    /// A panel tab, by id.
    Tab(String),
    /// A session (a subagent's), by library id.
    Session(String),
}

impl Detached {
    fn size(&self) -> Size {
        match self {
            Detached::Tab(tab) if tab.starts_with("surface") => Size::new(520.0, 780.0),
            Detached::Tab(_) => Size::new(640.0, 560.0),
            Detached::Session(_) => Size::new(560.0, 720.0),
        }
    }
}

/// Settings of a new window.
pub fn settings(size: Size) -> window::Settings {
    window::Settings { size, min_size: Some(Size::new(360.0, 280.0)), ..window::Settings::default() }
}

impl App {
    /// Opens the main window. Called once, when the daemon boots.
    pub fn open_main(&mut self) -> Task<Message> {
        let (id, open) = window::open(settings(MAIN_SIZE));
        self.main_window = Some(id);
        open.discard()
    }

    /// Shows `what` in its own window, or brings its window forward when it
    /// already has one.
    pub(super) fn detach(&mut self, what: Detached) -> Task<Message> {
        if let Some((id, _)) = self.detached.iter().find(|(_, shown)| *shown == what) {
            return window::gain_focus(*id);
        }
        let (id, open) = window::open(settings(what.size()));
        self.detached.push((id, what));
        self.sync_detached();
        open.discard()
    }

    /// A detached tab's window, brought to the front.
    pub(super) fn show_detached_tab(&mut self, tab: &str) -> Task<Message> {
        match self.detached.iter().find(|(_, shown)| *shown == Detached::Tab(tab.to_owned())) {
            Some((id, _)) => window::gain_focus(*id),
            None => Task::none(),
        }
    }

    /// Puts a detached tab back in the panel and closes its window.
    pub(super) fn reattach_tab(&mut self, tab: &str) -> Task<Message> {
        match self.detached.iter().find(|(_, shown)| *shown == Detached::Tab(tab.to_owned())).map(|(id, _)| *id) {
            Some(id) => self.reattach(id),
            None => Task::none(),
        }
    }

    /// Closes a detached window and shows its view in the panel again.
    pub(super) fn reattach(&mut self, id: window::Id) -> Task<Message> {
        let Some(index) = self.detached.iter().position(|(window, _)| *window == id) else { return Task::none() };
        let (_, what) = self.detached.remove(index);
        self.sync_detached();
        let show = match what {
            Detached::Tab(tab) => tab,
            Detached::Session(session) => {
                self.detached_chats.remove(&session);
                "agents".to_owned()
            }
        };
        self.ctx.shell.show_tab(&show);
        Task::batch([window::close(id), self.ctx.shell.reveal_active_tab()])
    }

    /// A window closed: the main one quits the app, a detached one gives its
    /// view back to the panel.
    pub(super) fn window_closed(&mut self, id: window::Id) -> Task<Message> {
        if self.main_window == Some(id) {
            return iced::exit();
        }
        if let Some(index) = self.detached.iter().position(|(window, _)| *window == id) {
            let (_, what) = self.detached.remove(index);
            if let Detached::Session(session) = what {
                self.detached_chats.remove(&session);
            }
            self.sync_detached();
        }
        Task::none()
    }

    fn sync_detached(&mut self) {
        let tabs = self
            .detached
            .iter()
            .filter_map(|(_, what)| match what {
                Detached::Tab(tab) => Some(tab.clone()),
                Detached::Session(_) => None,
            })
            .collect();
        self.ctx.shell.set_detached(tabs);
    }

    /// The window's title.
    pub fn title(&self, id: window::Id) -> String {
        match self.detached.iter().find(|(window, _)| *window == id).map(|(_, what)| what) {
            Some(Detached::Tab(tab)) => {
                let title = self.ctx.shell.tabs().iter().find(|shown| shown.id == *tab).map_or(tab.as_str(), |shown| shown.title.as_str());
                format!("{title} — Loa")
            }
            Some(Detached::Session(session)) => format!("{} — Loa", self.session_title(session)),
            None => "Loa".to_owned(),
        }
    }

    fn session_title(&self, session: &str) -> String {
        self.ctx.shell.sidebar().session(session).map_or_else(|| "Subagent".to_owned(), |summary| summary.title.clone())
    }

    /// Draws a window: the app in the main one, the detached view elsewhere.
    pub fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        let Some((_, what)) = self.detached.iter().find(|(window, _)| *window == id) else { return self.view() };
        let (title, body): (String, Element<'_, Message>) = match what {
            Detached::Tab(tab) => {
                let title = self.ctx.shell.tabs().iter().find(|shown| shown.id == *tab).map_or(tab.clone(), |shown| shown.title.clone());
                (title, self.view_tab(tab))
            }
            Detached::Session(session) => (self.session_title(session), self.detached_session(session)),
        };
        let header = row![
            muted(Icon::ExternalLink, 14.0),
            text(title).size(14).width(Fill),
            button(row![muted(Icon::PanelRight, 14.0), text("Reattach").size(13)].spacing(6).align_y(Alignment::Center))
                .on_press(Message::Reattach(id))
                .style(style::ghost_button)
                .padding([5, 8]),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        container(column![
            container(header).padding([8, 12]).style(style::side),
            rule::horizontal(1).style(style::divider),
            container(body).width(Fill).height(Fill),
        ])
        .style(style::surface)
        .width(Fill)
        .height(Fill)
        .into()
    }

    fn detached_session(&self, session: &str) -> Element<'_, Message> {
        let Some(open) = self.ctx.sessions.get(session) else {
            return container(text("This session is no longer loaded").style(style::muted)).center(Fill).into();
        };
        let Some(chat) = self.detached_chats.get(session) else {
            return container(text("Loading…").style(style::muted)).center(Fill).into();
        };
        let id = open.id().as_str();
        let owned = session.to_owned();
        chat.view(open.dom(), move |message| Message::DetachedChat(owned.clone(), message), move |slot| {
            self.areas
                .get::<crate::areas::decisions::State>()
                .card(id, slot.node)
                .map(|card| card.map(Message::area))
                .unwrap_or_else(|| text("Decision no longer available").into())
        })
    }

    /// Loads a session without making it active and shows it in a window.
    pub(super) fn detach_session(&mut self, session: &str) -> Task<Message> {
        if let Err(error) = self.ctx.sessions.load(&self.ctx.services, session) {
            self.ctx.notify(error);
            return Task::none();
        }
        if let Some(open) = self.ctx.sessions.get(session) {
            let theme = self.theme();
            self.detached_chats.entry(session.to_owned()).or_insert_with(|| loa_ui_session::transcript::State::from_dom(open.dom(), &theme));
        }
        self.detach(Detached::Session(session.to_owned()))
    }
}
