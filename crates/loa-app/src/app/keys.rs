//! Keyboard control (#78) and keyboard reachability (#82): the shortcuts
//! the keymap binds beyond the palette and panel toggles, palette commands
//! for every sidebar and composer control so nothing needs a pointer, and
//! reference rows that list the keys which aren't commands (Esc, Enter,
//! number keys on prompt cards…), so the palette shows the whole keymap.

use iced::Task;
use iced::keyboard::{Modifiers, key::Named};
use iced::widget::operation;
use loa_core::SessionMode;
use loa_ui_session::composer as composer;
use loa_ui_shell::palette::{self, Chord, CommandId, ids};
use loa_ui_shell as shell;

use super::{App, Message, TAB_COMMAND};
use crate::areas;

/// Section of the rows that list keys which aren't commands.
const KEYS_SECTION: &str = "Keyboard shortcuts";
/// The sidebar's Tools button.
const SHOW_TOOLS: CommandId = CommandId::from_static("sidebar.tools");
/// The sidebar's Peers rows.
const SHOW_PEERS: CommandId = CommandId::from_static("sidebar.peers");
/// The hover-only Pin / Unpin button of the active session's sidebar row.
const PIN: CommandId = CommandId::from_static("sidebar.pin");
/// Prefixes of the composer picker commands; the rest is the mode, the
/// effort, or the model's index in the picker.
const MODE: &str = "composer.mode.";
const EFFORT: &str = "composer.effort.";
const MODEL: &str = "composer.model.";

impl App {
    /// Navigation, focus, sidebar and composer commands, then the keymap's
    /// reference rows.
    pub(super) fn keyboard_commands(&self) -> Vec<palette::Command> {
        let keymap = &self.ctx.keymap;
        let hinted = |id: CommandId, title: String, section: &str| {
            let chord = keymap.chord_for(&id);
            palette::Command::new(id, title, section).shortcut(chord)
        };
        let mut commands = vec![
            hinted(ids::SESSION_NEXT, "Next session".into(), "Commands"),
            hinted(ids::SESSION_PREVIOUS, "Previous session".into(), "Commands"),
            hinted(ids::PROJECT_TAB_NEXT, "Next project tab".into(), "Commands"),
            hinted(ids::PROJECT_TAB_PREVIOUS, "Previous project tab".into(), "Commands"),
            hinted(ids::COMPOSER_FOCUS, "Focus the composer".into(), "Commands"),
            hinted(ids::SEARCH_FOCUS, "Search sessions".into(), "Commands"),
            hinted(ids::FOCUS_NEXT, "Focus the next field".into(), "Commands"),
            hinted(ids::FOCUS_PREVIOUS, "Focus the previous field".into(), "Commands"),
            hinted(ids::KEYMAP_SHOW, "Keyboard shortcuts".into(), "Commands"),
            palette::Command::new(SHOW_TOOLS, "Show tools", "Commands"),
            palette::Command::new(SHOW_PEERS, "Show peers", "Commands"),
        ];
        let sidebar = self.ctx.shell.sidebar();
        for (index, id) in self.session_order().iter().enumerate() {
            let title = sidebar.session(id).map_or(id.as_str(), |summary| summary.title.as_str());
            commands.push(hinted(ids::session_goto(index + 1), format!("Go to session: {title}"), "Sessions"));
        }
        if let Some(summary) = self.ctx.sessions.active_id().and_then(|id| sidebar.session(id.as_str())) {
            commands.push(palette::Command::new(PIN, if summary.pinned { "Unpin session" } else { "Pin session" }, "Session"));
        }
        if self.ctx.sessions.active().is_some() {
            let composer = &self.ctx.composer;
            commands.extend(SessionMode::ALL.iter().map(|mode| {
                let id = CommandId::new(format!("{MODE}{}", mode.as_str()));
                palette::Command::new(id, format!("Mode: {}", mode.as_str()), "Composer")
            }));
            commands.extend(composer.efforts().0.iter().map(|effort| {
                palette::Command::new(CommandId::new(format!("{EFFORT}{effort}")), format!("Effort: {effort}"), "Composer")
            }));
            commands.extend(composer.models().0.iter().enumerate().map(|(index, model)| {
                palette::Command::new(CommandId::new(format!("{MODEL}{index}")), format!("Model: {model}"), "Composer")
            }));
        }
        commands.extend(self.reference_keys());
        commands
    }

    /// Keys that aren't palette commands, listed for reference.
    fn reference_keys(&self) -> Vec<palette::Command> {
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

    /// Runs `id` when it is one of the keyboard commands.
    pub(super) fn keyboard_command(&mut self, id: &str) -> Option<Task<Message>> {
        let task = match id {
            "session.next" => self.step_session(true),
            "session.previous" => self.step_session(false),
            "project.tab.next" => self.step_project_tab(true),
            "project.tab.previous" => self.step_project_tab(false),
            "composer.focus" => self.focus_composer(),
            "sidebar.search" => operation::focus(shell::sidebar::SEARCH_ID),
            "focus.next" => operation::focus_next(),
            "focus.previous" => operation::focus_previous(),
            "keymap.show" => self.update(Message::Palette(palette::Message::OpenShortcuts)),
            "sidebar.pin" => match self.ctx.sessions.active_id().map(ToString::to_string) {
                Some(id) => self.update(Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::TogglePin(id)))),
                None => {
                    self.ctx.notify("Open a session to pin it");
                    Task::none()
                }
            },
            "sidebar.tools" => self.update(Message::Tools(areas::tools::Message::Open)),
            "sidebar.peers" => self.update(Message::Peers(areas::peers::Message::Open)),
            _ => {
                if let Some(n) = id.strip_prefix(ids::SESSION_GOTO) {
                    let n = n.parse::<usize>().ok()?;
                    return Some(match self.session_order().get(n.wrapping_sub(1)).cloned() {
                        Some(session) => self.show_session(session),
                        None => {
                            self.ctx.notify(format!("There is no session {n} in the sidebar"));
                            Task::none()
                        }
                    });
                }
                if let Some(n) = id.strip_prefix(ids::PANEL_TAB) {
                    let n = n.parse::<usize>().ok()?;
                    return Some(match self.ctx.shell.tabs().get(n.wrapping_sub(1)).map(|tab| tab.id.clone()) {
                        Some(tab) => self.open_tab(&tab),
                        None => {
                            self.ctx.notify(format!("There is no panel tab {n}"));
                            Task::none()
                        }
                    });
                }
                if let Some(mode) = id.strip_prefix(MODE) {
                    let pick = self.ctx.composer.pick_key(composer::PickerId::Mode, mode)?;
                    return Some(self.update(Message::Composer(pick)));
                }
                if let Some(effort) = id.strip_prefix(EFFORT) {
                    let pick = self.ctx.composer.pick_key(composer::PickerId::Effort, effort)?;
                    return Some(self.update(Message::Composer(pick)));
                }
                let index = id.strip_prefix(MODEL)?.parse::<usize>().ok()?;
                self.ctx.composer.models().0.get(index)?;
                self.update(Message::Composer(composer::Message::Pick(composer::PickerId::Model, index)))
            }
        };
        Some(task)
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
        if self.ctx.sessions.active().is_none() {
            self.ctx.notify("Start or open a session to type a prompt");
            return Task::none();
        }
        operation::focus(composer::EDITOR_ID)
    }
}

#[cfg(test)]
mod tests;
