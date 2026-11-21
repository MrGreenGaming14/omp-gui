//! The app's own commands (#78, #82): session and view actions, panel tabs,
//! themes, the shortcuts the keymap binds and a palette command for every
//! sidebar and composer control, so nothing needs a pointer.

use iced::Task;
use iced::keyboard::{Modifiers, key::Named};
use iced::widget::operation;
use loa_core::SessionMode;
use loa_ui_session::composer as composer;
use loa_ui_shell::palette::{self, Chord, CommandId, ids};
use loa_ui_shell as shell;

use super::{App, Message};
use crate::area::{CommandSpec, Listed};
use crate::areas;

/// Section of the rows that list keys which aren't commands.
const KEYS_SECTION: &str = "Keyboard shortcuts";
/// Prefix of the `Open X tab` commands; the rest is the tab id.
pub(super) const TAB_COMMAND: &str = "tab.";

/// A command of the app itself.
pub(crate) type AppCommand = CommandSpec<App, fn(&mut App, &str) -> Task<Message>>;

const fn command(id: &'static str, title: &'static str, section: &'static str, run: fn(&mut App, &str) -> Task<Message>) -> AppCommand {
    CommandSpec { id, title, section, slash: None, listed: Listed::Always, run }
}

/// Every app command, in palette order. Area commands follow them.
pub(crate) const APP_COMMANDS: &[AppCommand] = &[
    command("session.new", "New session", "Commands", |app, _| app.lifecycle(|s, ctx, areas| s.run(ctx, areas, loa_runtime::lifecycle::Command::Start { prompt: None })))
        .slash("new", "Start a new session"),
    command("panel.toggle", "Toggle right panel", "Commands", |app, _| {
        app.ctx.shell.set_panel_open(!app.ctx.shell.panel_open());
        Task::none()
    }),
    command("session.interrupt", "Interrupt session", "Commands", |app, _| app.update(Message::Composer(composer::Message::Interrupt))),
    command("project.tabs.toggle", "", "Commands", |app, _| {
        let enabled = !app.ctx.shell.project_tabs();
        app.ctx.shell.set_project_tabs(enabled);
        app.ctx.edit_config("settings", |config| config.project_tabs = enabled);
        Task::none()
    })
    .listed(Listed::When(|app, _| Some(if app.ctx.shell.project_tabs() { "Hide project tabs" } else { "Show project tabs" }))),
    command("palette.toggle", "Command palette", "Commands", |app, _| app.update(Message::Palette(palette::Message::Toggle))),
    command(TAB_COMMAND, "", "Commands", |app, tab| app.open_tab(tab))
        .listed(Listed::Each(|app, _| app.tabs().into_iter().map(|tab| (tab.id, format!("Open {} tab", tab.title))).collect())),
    command("theme.", "", "Theme", |app, theme| {
        if !app.ctx.shell.theme_state().themes().iter().any(|(key, _)| key == theme) {
            return app.unknown("theme.", theme);
        }
        app.update(Message::Shell(shell::Message::SetTheme(shell::ThemeRef(theme.to_owned()))))
    })
    .listed(Listed::Each(|app, _| {
        app.ctx.shell.theme_state().themes().into_iter().map(|(key, label)| (key.to_string(), format!("Theme: {label}"))).collect()
    })),
    command("session.next", "Next session", "Commands", |app, _| app.step_session(true)),
    command("session.previous", "Previous session", "Commands", |app, _| app.step_session(false)),
    command("project.tab.next", "Next project tab", "Commands", |app, _| app.step_project_tab(true)),
    command("project.tab.previous", "Previous project tab", "Commands", |app, _| app.step_project_tab(false)),
    command("composer.focus", "Focus the composer", "Commands", |app, _| app.focus_composer()),
    command("sidebar.search", "Search sessions", "Commands", |_, _| operation::focus(shell::sidebar::SEARCH_ID)),
    command("focus.next", "Focus the next field", "Commands", |_, _| operation::focus_next()),
    command("focus.previous", "Focus the previous field", "Commands", |_, _| operation::focus_previous()),
    command("keymap.show", "Keyboard shortcuts", "Commands", |app, _| app.update(Message::Palette(palette::Message::OpenShortcuts))),
    // The sidebar's Tools button and Peers rows.
    command("sidebar.tools", "Show tools", "Commands", |app, _| app.update(Message::area(areas::tools::Message::Open))),
    command("sidebar.peers", "Show peers", "Commands", |app, _| app.update(Message::area(areas::peers::Message::Open))),
    command(ids::SESSION_GOTO, "", "Sessions", |app, n| match n.parse::<usize>() {
        Ok(n) => match app.session_order().get(n.wrapping_sub(1)).cloned() {
            Some(session) => app.show_session(session),
            None => {
                app.ctx.notify(format!("There is no session {n} in the sidebar"));
                Task::none()
            }
        },
        Err(_) => app.unknown(ids::SESSION_GOTO, n),
    })
    .listed(Listed::Each(|app, _| {
        let sidebar = app.ctx.shell.sidebar();
        let title = |id: &str| sidebar.session(id).map_or(id.to_owned(), |summary| summary.title.clone());
        app.session_order().iter().enumerate().map(|(index, id)| ((index + 1).to_string(), format!("Go to session: {}", title(id)))).collect()
    })),
    // The hover-only Pin / Unpin button of the active session's sidebar row.
    command("sidebar.pin", "", "Session", |app, _| match app.ctx.runtime.sessions().active_id().map(ToString::to_string) {
        Some(id) => app.update(Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::TogglePin(id)))),
        None => {
            app.ctx.notify("Open a session to pin it");
            Task::none()
        }
    })
    .listed(Listed::When(|app, ctx| {
        let summary = ctx.runtime.sessions().active_id().and_then(|id| app.ctx.shell.sidebar().session(id.as_str()))?;
        Some(if summary.pinned { "Unpin session" } else { "Pin session" })
    })),
    // The composer's pickers: the mode, the effort, the model by its index.
    command("composer.mode.", "", "Composer", |app, mode| app.pick(composer::PickerId::Mode, "composer.mode.", mode))
        .listed(Listed::Each(|_, ctx| match ctx.runtime.sessions().active() {
            Some(_) => SessionMode::ALL.iter().map(|mode| (mode.as_str().to_owned(), format!("Mode: {}", mode.as_str()))).collect(),
            None => Vec::new(),
        })),
    command("composer.effort.", "", "Composer", |app, effort| app.pick(composer::PickerId::Effort, "composer.effort.", effort))
        .listed(Listed::Each(|_, ctx| match ctx.runtime.sessions().active() {
            Some(_) => ctx.composer.efforts().0.iter().map(|effort| (effort.to_string(), format!("Effort: {effort}"))).collect(),
            None => Vec::new(),
        })),
    command("composer.model.", "", "Composer", |app, index| match index.parse::<usize>() {
        Ok(index) if index < app.ctx.composer.models().0.len() => {
            app.update(Message::Composer(composer::Message::Pick(composer::PickerId::Model, index)))
        }
        _ => app.unknown("composer.model.", index),
    })
    .listed(Listed::Each(|_, ctx| match ctx.runtime.sessions().active() {
        Some(_) => ctx.composer.models().0.iter().enumerate().map(|(index, model)| (index.to_string(), format!("Model: {model}"))).collect(),
        None => Vec::new(),
    })),
    // Ctrl/Cmd+Alt+1…9: the n-th tab in the panel.
    command(ids::PANEL_TAB, "", "Commands", |app, n| match n.parse::<usize>() {
        Ok(n) => match app.ctx.shell.tabs().get(n.wrapping_sub(1)).map(|tab| tab.id.clone()) {
            Some(tab) => app.open_tab(&tab),
            None => {
                app.ctx.notify(format!("There is no panel tab {n}"));
                Task::none()
            }
        },
        Err(_) => app.unknown(ids::PANEL_TAB, n),
    })
    .listed(Listed::Never),
];

impl App {
    /// Keys that aren't palette commands, listed for reference.
    pub(super) fn reference_keys(&self) -> Vec<palette::Command> {
        let keymap = &self.ctx.keymap;
        let platform = keymap.platform();
        let primary = platform.primary();
        let none = Modifiers::empty();
        let label = |key: Named, modifiers| Chord::named(key, modifiers).label(platform);
        let hint = |id: &CommandId, fallback: String| keymap.chord_for(id).map_or(fallback, |chord| chord.label(platform));
        let (escape, enter, tab) = (label(Named::Escape, none), label(Named::Enter, none), label(Named::Tab, none));
        let (up, down) = (label(Named::ArrowUp, none), label(Named::ArrowDown, none));
        let range = |modifiers| format!("{}–9", Chord::char('1', modifiers).label(platform));
        let rows = [
            ("close", "Close the palette or a screen, dismiss the focused prompt", escape.clone()),
            ("prompt.pick", "Prompt card: pick option 1 to 9", "1–9".to_owned()),
            ("prompt.confirm", "Prompt card: confirm the pick", enter.clone()),
            ("composer.send", "Composer: send", hint(&ids::COMPOSER_SUBMIT, enter.clone())),
            ("composer.newline", "Composer: new line", hint(&ids::COMPOSER_NEWLINE, label(Named::Enter, Modifiers::SHIFT))),
            (
                "composer.undo",
                "Composer: undo, redo",
                format!("{} {}", Chord::char('z', primary).label(platform), Chord::char('z', primary | Modifiers::SHIFT).label(platform)),
            ),
            ("composer.popup", "Composer: move in the / and @ lists, accept", format!("{up} {down} {tab}")),
            ("palette", "Palette: move, run, close", format!("{up} {down} {enter} {escape}")),
            ("session.goto", "Go to session 1 to 9 in the sidebar", range(primary)),
            ("panel.tab", "Show right panel tab 1 to 9", range(primary | Modifiers::ALT)),
        ];
        rows.into_iter()
            .map(|(id, title, keys)| {
                palette::Command::new(CommandId::new(format!("keys.{id}")), title, KEYS_SECTION).keys(keys).enabled(false)
            })
            .collect()
    }

    /// Fills in the shortcut hint of every command the keymap binds,
    /// including `Open X tab` by the tab's place in the panel.
    pub(super) fn hint_commands(&self, commands: &mut [palette::Command]) {
        let keymap = &self.ctx.keymap;
        let tabs = self.ctx.shell.tabs();
        for command in commands.iter_mut().filter(|command| command.shortcut.is_none() && command.keys.is_none()) {
            let bound = match command.id.as_str().strip_prefix(TAB_COMMAND) {
                Some(tab) => tabs.iter().position(|shown| shown.id == tab).map(|index| ids::panel_tab(index + 1)),
                None => Some(command.id.clone()),
            };
            command.shortcut = bound.and_then(|id| keymap.chord_for(&id));
        }
    }

    /// The `Unknown command` notice for `prefix` + `arg`.
    fn unknown(&mut self, prefix: &str, arg: &str) -> Task<Message> {
        self.ctx.notify(format!("Unknown command {prefix}{arg}"));
        Task::none()
    }

    /// Picks `key` in the composer's `picker`.
    fn pick(&mut self, picker: composer::PickerId, prefix: &str, key: &str) -> Task<Message> {
        match self.ctx.composer.pick_key(picker, key) {
            Some(pick) => self.update(Message::Composer(pick)),
            None => self.unknown(prefix, key),
        }
    }

    /// Session ids in the sidebar's order: Pinned, then Recents, as
    /// filtered by its search.
    fn session_order(&self) -> Vec<String> {
        let sidebar = self.ctx.shell.sidebar();
        sidebar.pinned().into_iter().chain(sidebar.recents()).map(|summary| summary.id.clone()).collect()
    }

    /// Shows the next (or previous) session after the highlighted one,
    /// wrapping around.
    fn step_session(&mut self, forward: bool) -> Task<Message> {
        let order = self.session_order();
        if order.is_empty() {
            self.ctx.notify("No sessions yet");
            return Task::none();
        }
        let current = self.ctx.shell.sidebar().active().and_then(|active| order.iter().position(|id| id == active));
        let len = order.len();
        let index = match (current, forward) {
            (Some(index), true) => (index + 1) % len,
            (Some(index), false) => (index + len - 1) % len,
            (None, true) => 0,
            (None, false) => len - 1,
        };
        self.show_session(order[index].clone())
    }

    /// Opens `session` the way a click on its sidebar row does.
    fn show_session(&mut self, session: String) -> Task<Message> {
        self.update(Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::OpenSession(session))))
    }

    /// Puts the focus in the composer, closing a screen over the chat.
    fn focus_composer(&mut self) -> Task<Message> {
        if self.ctx.screen().is_some() {
            self.ctx.close_screen();
        }
        if self.ctx.runtime.sessions().active().is_none() {
            self.ctx.notify("Start or open a session to type a prompt");
            return Task::none();
        }
        operation::focus(composer::EDITOR_ID)
    }
}
