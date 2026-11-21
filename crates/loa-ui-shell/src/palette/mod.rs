//! The Ctrl/Cmd+K command palette and the keymap. The app registers [`Command`]s, stacks
//! [`State::view`] over its content and runs the id carried by [`Event::Run`].

mod keymap;

pub use keymap::{Binding, Chord, ChordKey, Keymap, Platform, Scope};

use iced::keyboard::{self, key::Named};
use iced::widget::{Column, Id, button, column, container, mouse_area, opaque, operation, rich_text, row, rule, scrollable, space, span, text, text_input};
use iced::{Element, Fill, Font, Padding, Subscription, Task, font};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::text::fuzzy::{Class, Matcher, highlight_runs};
use loa_ui_kit::theme::{LayoutRole, SpaceRole as Sp, TextRole as T, pad_x, pad_y, sp, text_size as ts, width};
use loa_ui_kit::widgets::{label, scroll_target};
use std::{borrow::Cow, cmp::Reverse, fmt};

pub const ROW_HEIGHT: f32 = 35.0;
pub const HEADER_HEIGHT: f32 = 35.0;
/// Tallest the result list grows before it scrolls.
pub const MAX_LIST_HEIGHT: f32 = 360.0;
const MAX_RECENT: usize = 32;

/// Sort key of a result: class, score (higher first), recency, registration order.
type RankKey = (Class, Reverse<i32>, usize, usize);

/// Stable identifier of an app command, such as `panel.toggle`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandId(Cow<'static, str>);

impl CommandId {
    pub const fn from_static(id: &'static str) -> Self { Self(Cow::Borrowed(id)) }
    pub fn new(id: impl Into<String>) -> Self { Self(Cow::Owned(id.into())) }
    pub fn as_str(&self) -> &str { &self.0 }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

/// Ids of the commands the default keymap binds.
pub mod ids {
    use super::CommandId;
    pub const PALETTE_TOGGLE: CommandId = CommandId::from_static("palette.toggle");
    pub const PANEL_TOGGLE: CommandId = CommandId::from_static("panel.toggle");
    pub const SESSION_INTERRUPT: CommandId = CommandId::from_static("session.interrupt");
    pub const COMPOSER_SUBMIT: CommandId = CommandId::from_static("composer.submit");
    pub const COMPOSER_NEWLINE: CommandId = CommandId::from_static("composer.newline");
    pub const SESSION_NEW: CommandId = CommandId::from_static("session.new");
    pub const SESSION_NEXT: CommandId = CommandId::from_static("session.next");
    pub const SESSION_PREVIOUS: CommandId = CommandId::from_static("session.previous");
    pub const PROJECT_TAB_NEXT: CommandId = CommandId::from_static("project.tab.next");
    pub const PROJECT_TAB_PREVIOUS: CommandId = CommandId::from_static("project.tab.previous");
    pub const COMPOSER_FOCUS: CommandId = CommandId::from_static("composer.focus");
    pub const SEARCH_FOCUS: CommandId = CommandId::from_static("sidebar.search");
    pub const KEYMAP_SHOW: CommandId = CommandId::from_static("keymap.show");
    pub const FOCUS_NEXT: CommandId = CommandId::from_static("focus.next");
    pub const FOCUS_PREVIOUS: CommandId = CommandId::from_static("focus.previous");
    /// Prefix of `session.goto.<n>`: the n-th session (1 based) in the sidebar's order.
    pub const SESSION_GOTO: &str = "session.goto.";
    /// Prefix of `panel.tab.<n>`: the n-th right panel tab (1 based).
    pub const PANEL_TAB: &str = "panel.tab.";
    pub fn session_goto(n: usize) -> CommandId { CommandId::new(format!("{SESSION_GOTO}{n}")) }
    pub fn panel_tab(n: usize) -> CommandId { CommandId::new(format!("{PANEL_TAB}{n}")) }
}

/// A command the app offers in the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: CommandId,
    pub title: String,
    /// Group heading, e.g. "Session".
    pub section: String,
    pub shortcut: Option<Chord>,
    /// A hint for keys that aren't one chord (e.g. `1–9`); wins over `shortcut`.
    pub keys: Option<String>,
    /// Disabled commands are listed but cannot run.
    pub enabled: bool,
}

impl Command {
    pub fn new(id: CommandId, title: impl Into<String>, section: impl Into<String>) -> Self {
        Self { id, title: title.into(), section: section.into(), shortcut: None, keys: None, enabled: true }
    }
    pub fn shortcut(self, shortcut: Option<Chord>) -> Self { Self { shortcut, ..self } }
    pub fn keys(self, keys: impl Into<String>) -> Self { Self { keys: Some(keys.into()), ..self } }
    pub fn enabled(self, enabled: bool) -> Self { Self { enabled, ..self } }
}

/// One listed result: an index into the commands, the matched title chars, and whether a
/// section header precedes it.
#[derive(Debug, Clone)]
struct Row {
    command: usize,
    indices: Vec<usize>,
    starts_section: bool,
}

impl Row {
    fn height(&self) -> f32 { ROW_HEIGHT + if self.starts_section { HEADER_HEIGHT } else { 0.0 } }
}

#[derive(Debug)]
pub struct State {
    platform: Platform,
    commands: Vec<Command>,
    /// Recently run ids, most recent first; they win ranking ties.
    recent: Vec<CommandId>,
    open: bool,
    query: String,
    /// Opened on the keyboard shortcuts: only commands with a hint list.
    shortcuts_only: bool,
    rows: Vec<Row>,
    selected: Option<usize>,
    scroll_offset: f32,
    matcher: Matcher,
    input_id: Id,
    list_id: Id,
}

#[derive(Debug, Clone)]
pub enum Message {
    Open,
    /// Open listing only the commands with a keyboard shortcut.
    OpenShortcuts,
    Close,
    Toggle,
    QueryChanged(String),
    SelectNext,
    SelectPrevious,
    Submit,
    /// Run the command in this display row.
    Activate(usize),
    Scrolled(f32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Run this command; the palette has closed.
    Run(CommandId),
    /// Closed without running anything; give focus back.
    Closed,
}

/// A result as the app or a test sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub command: &'a Command,
    pub matched: &'a [usize],
    pub starts_section: bool,
    pub selected: bool,
}

impl State {
    pub fn new(platform: Platform) -> Self {
        let (commands, recent, rows, query) = Default::default();
        let (input_id, list_id) = (Id::new("loa-palette-search"), Id::new("loa-palette-list"));
        Self { platform, commands, recent, open: false, query, shortcuts_only: false, rows, selected: None, scroll_offset: 0.0, matcher: Matcher::new(), input_id, list_id }
    }

    /// Replaces the commands; registration order breaks the last ranking ties.
    pub fn set_commands(&mut self, commands: Vec<Command>) {
        self.commands = commands;
        self.refilter();
    }

    pub fn commands(&self) -> &[Command] { &self.commands }
    /// While open, the app resolves keys in [`Scope::Palette`].
    pub fn is_open(&self) -> bool { self.open }
    pub fn query(&self) -> &str { &self.query }
    pub fn shortcuts_only(&self) -> bool { self.shortcuts_only }

    pub fn entries(&self) -> impl Iterator<Item = Entry<'_>> {
        let selected = self.selected;
        self.rows.iter().enumerate().map(move |(i, r)| Entry { command: &self.commands[r.command], matched: &r.indices, starts_section: r.starts_section, selected: selected == Some(i) })
    }

    pub fn selected(&self) -> Option<&Command> { self.selected.map(|i| &self.commands[self.rows[i].command]) }
    /// The shortcut hint for a command on this platform.
    pub fn hint(&self, c: &Command) -> Option<String> { c.keys.clone().or_else(|| c.shortcut.map(|s| s.label(self.platform))) }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let open = self.open;
        match message {
            Message::Open => (self.open(false), None),
            Message::OpenShortcuts => (self.open(true), None),
            Message::Toggle if !open => (self.open(false), None),
            Message::Close | Message::Toggle => {
                self.open = false;
                (Task::none(), open.then_some(Event::Closed))
            }
            Message::Scrolled(y) => {
                self.scroll_offset = y;
                (Task::none(), None)
            }
            _ if !open => (Task::none(), None),
            Message::QueryChanged(query) => {
                self.query = query;
                self.refilter();
                (self.scroll_to(0.0), None)
            }
            Message::SelectNext => (self.step(true), None),
            Message::SelectPrevious => (self.step(false), None),
            Message::Submit => (Task::none(), self.selected.and_then(|i| self.run(i))),
            Message::Activate(i) => (Task::none(), self.run(i)),
        }
    }

    /// Up, Down, Enter and Esc while open, captured or not (the search field captures Esc).
    pub fn subscription(&self) -> Subscription<Message> {
        if !self.open { return Subscription::none() }
        iced::event::listen_with(|event, _, _| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed { key: keyboard::Key::Named(named), .. }) => match named {
                Named::ArrowUp => Some(Message::SelectPrevious),
                Named::ArrowDown => Some(Message::SelectNext),
                Named::Enter => Some(Message::Submit),
                Named::Escape => Some(Message::Close),
                _ => None,
            },
            _ => None,
        })
    }

    /// A backdrop that closes on click with the card near the top; empty while closed.
    pub fn view(&self) -> Element<'_, Message> {
        if !self.open { return space().into() }
        let placeholder = if self.shortcuts_only { "Search keyboard shortcuts" } else { "Type a command" };
        let bare = |theme: &iced::Theme, status| text_input::Style { background: iced::Color::TRANSPARENT.into(), border: iced::Border::default(), ..style::input(theme, status) };
        let input = text_input(placeholder, &self.query).id(self.input_id.clone()).on_input(Message::QueryChanged).padding(pad_y(Sp::S)).size(ts(T::Body)).style(bare);
        let search = container(row![tinted(Icon::Search, T::Body, Tone::Muted), input].spacing(sp(Sp::S)).align_y(iced::Center)).padding(pad_x(Sp::M));
        let results: Element<'_, Message> = if self.rows.is_empty() {
            container(label("No matching commands", T::Body, Tone::Muted)).padding(sp(Sp::M)).into()
        } else {
            let mut list = Column::with_capacity(self.rows.len() * 2);
            for (i, r) in self.rows.iter().enumerate() {
                let command = &self.commands[r.command];
                if r.starts_section {
                    let head = container(label(&command.section, T::Caption, Tone::Muted)).height(HEADER_HEIGHT);
                    list = list.push(head.padding(Padding::ZERO.left(sp(Sp::M)).bottom(sp(Sp::S))).align_y(iced::Bottom));
                }
                list = list.push(container(self.row_view(i, r, command)).padding(pad_x(Sp::Xs)));
            }
            let list = scrollable(list).id(self.list_id.clone()).height(self.list_height()).on_scroll(|v| Message::Scrolled(v.absolute_offset().y));
            container(list).width(Fill).padding(Padding::ZERO.bottom(sp(Sp::Xs))).into()
        };
        // Inset past the 1px border so children never paint over it.
        let card = container(column![search, rule::horizontal(1).style(style::rule), results]).padding(Padding::new(1.0)).width(width(LayoutRole::PopupWidth)).style(style::container(Surface::Popup));
        let backdrop = container(opaque(card)).width(Fill).height(Fill).center_x(Fill).padding(Padding::ZERO.top(sp(Sp::Xl) * 5.0));
        opaque(mouse_area(backdrop).on_press(Message::Close))
    }

    fn row_view<'a>(&'a self, index: usize, r: &'a Row, command: &'a Command) -> Element<'a, Message> {
        let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
        let spans: Vec<text::Span<'a, (), Font>> = highlight_runs(&command.title, &r.indices).into_iter().map(|(range, hit)| if hit { span(&command.title[range]).font(bold) } else { span(&command.title[range]) }).collect();
        let content = row![rich_text(spans).size(ts(T::Body)), space::horizontal()].height(Fill).align_y(iced::Center).spacing(sp(Sp::M));
        let content = content.push(self.hint(command).map(|h| label(h, T::Caption, Tone::Muted)));
        // A row that can't run is static text, so it never reads as a broken control.
        if !command.enabled {
            return container(content).style(|t| container::Style { text_color: Some(Tone::Muted.color(t)), ..Default::default() }).width(Fill).height(ROW_HEIGHT).padding(pad_x(Sp::S)).into();
        }
        let selected = self.selected == Some(index);
        button(content).width(Fill).height(ROW_HEIGHT).padding(pad_x(Sp::S)).on_press(Message::Activate(index)).style(style::button(ButtonKind::Row { selected })).into()
    }

    fn open(&mut self, shortcuts_only: bool) -> Task<Message> {
        (self.shortcuts_only, self.open) = (shortcuts_only, true);
        self.query.clear();
        self.refilter();
        Task::batch([operation::focus(self.input_id.clone()), self.scroll_to(0.0)])
    }

    fn scroll_to(&mut self, y: f32) -> Task<Message> {
        self.scroll_offset = y;
        operation::scroll_to(self.list_id.clone(), operation::AbsoluteOffset { x: None, y: Some(y) })
    }

    fn run(&mut self, index: usize) -> Option<Event> {
        let id = self.rows.get(index).map(|r| &self.commands[r.command]).filter(|c| c.enabled)?.id.clone();
        self.open = false;
        self.recent.retain(|r| *r != id);
        self.recent.insert(0, id.clone());
        self.recent.truncate(MAX_RECENT);
        Some(Event::Run(id))
    }

    /// Moves to the next (or previous) enabled row, wrapping, and scrolls it into view.
    fn step(&mut self, forward: bool) -> Task<Message> {
        let n = self.rows.len();
        if n == 0 { return Task::none() }
        let start = match (self.selected, forward) {
            (Some(i), true) => i + 1,
            (Some(i), false) => i + n - 1,
            (None, true) => 0,
            (None, false) => n - 1,
        };
        let order = (0..n).map(|k| if forward { (start + k) % n } else { (start + n - k) % n });
        let Some(next) = order.into_iter().find(|&i| self.commands[self.rows[i].command].enabled) else { return Task::none() };
        self.selected = Some(next);
        // A section's header counts as part of its first row, so scrolling up shows it.
        let top: f32 = self.rows[..next].iter().map(Row::height).sum();
        match scroll_target(self.scroll_offset, self.list_height(), top, top + self.rows[next].height()) {
            Some(y) => self.scroll_to(y),
            None => Task::none(),
        }
    }

    fn list_height(&self) -> f32 { self.rows.iter().map(Row::height).sum::<f32>().min(MAX_LIST_HEIGHT) }

    /// Ranks by match class, score, recency and registration, then groups by section with
    /// sections ordered by their best result, and selects the first enabled row.
    fn refilter(&mut self) {
        self.matcher.set_query(&self.query);
        let mut ranked: Vec<(RankKey, Vec<usize>)> = Vec::new();
        for (i, c) in self.commands.iter().enumerate() {
            if self.shortcuts_only && c.keys.is_none() && c.shortcut.is_none() { continue }
            if let Some(found) = self.matcher.matches(&c.title) {
                let recency = self.recent.iter().position(|r| *r == c.id).unwrap_or(usize::MAX);
                ranked.push(((found.class, Reverse(found.score), recency, i), found.indices));
            }
        }
        ranked.sort_unstable_by_key(|(key, _)| *key);
        let mut sections: Vec<&str> = Vec::new();
        let mut keyed: Vec<(usize, usize, Vec<usize>)> = ranked.into_iter().map(|((.., i), indices)| {
            let section = self.commands[i].section.as_str();
            let rank = sections.iter().position(|s| *s == section).unwrap_or_else(|| { sections.push(section); sections.len() - 1 });
            (rank, i, indices)
        }).collect();
        keyed.sort_by_key(|(rank, ..)| *rank);
        let mut previous = None;
        self.rows = keyed.into_iter().map(|(rank, command, indices)| Row { command, indices, starts_section: previous.replace(rank) != Some(rank) }).collect();
        self.selected = self.rows.iter().position(|r| self.commands[r.command].enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &'static str) -> CommandId { CommandId::from_static(s) }

    fn palette(commands: Vec<Command>) -> State {
        let mut state = State::new(Platform::Other);
        state.set_commands(commands);
        let _ = state.update(Message::Open);
        state
    }

    fn listed(state: &mut State, q: &str) -> Vec<(String, bool)> {
        let _ = state.update(Message::QueryChanged(q.into()));
        state.entries().map(|e| (e.command.title.clone(), e.starts_section)).collect()
    }

    #[test]
    fn ranking_groups_sections_and_recency_breaks_ties() {
        let commands = [("find", "Find in files", "File"), ("tree", "File tree", "View"), ("open", "Open file", "File"), ("format", "Format imports", "File"), ("panel", "Toggle panel", "View"), ("term", "Toggle terminal", "View")];
        let mut state = palette(commands.iter().map(|(i, t, s)| Command::new(id(i), *t, *s)).collect());
        let titles = |l: Vec<(String, bool)>| l.into_iter().map(|(t, s)| format!("{}{t}", if s { "# " } else { "" })).collect::<Vec<_>>();
        assert_eq!(titles(listed(&mut state, "fi")), ["# Find in files", "Open file", "Format imports", "# File tree"]);
        assert_eq!(titles(listed(&mut state, "tog")), ["# Toggle panel", "Toggle terminal"]);
        let _ = state.update(Message::QueryChanged("terminal".into()));
        assert_eq!(state.update(Message::Submit).1, Some(Event::Run(id("term"))));
        let _ = state.update(Message::Open);
        assert_eq!(state.query(), "", "reopening clears the query");
        assert_eq!(titles(listed(&mut state, "tog")), ["# Toggle terminal", "Toggle panel"]);
        assert_eq!(titles(listed(&mut state, "pan")), ["# Toggle panel"], "recency never beats a better match");
        let _ = state.update(Message::QueryChanged("tp".into()));
        assert_eq!(state.entries().next().unwrap().matched, &[0, 7]);
    }

    #[test]
    fn navigation_skips_disabled_wraps_scrolls_and_closes() {
        let mut commands: Vec<_> = (0..20).map(|i| Command::new(CommandId::new(format!("c{i}")), format!("Command {i:02}"), "S")).collect();
        commands[1].enabled = false;
        let mut state = palette(commands);
        for _ in 0..10 { let _ = state.update(Message::SelectNext); }
        assert_eq!(state.selected().unwrap().title, "Command 11");
        assert_eq!(state.scroll_offset, HEADER_HEIGHT + 12.0 * ROW_HEIGHT - MAX_LIST_HEIGHT);
        for _ in 0..11 { let _ = state.update(Message::SelectPrevious); }
        assert_eq!((state.selected().unwrap().title.as_str(), state.scroll_offset), ("Command 19", HEADER_HEIGHT + 20.0 * ROW_HEIGHT - MAX_LIST_HEIGHT));
        assert_eq!(state.update(Message::Activate(1)).1, None, "disabled rows do not run");
        assert_eq!(listed(&mut state, "zzz"), []);
        assert_eq!(state.update(Message::Submit).1, None);
        assert_eq!(state.update(Message::Toggle).1, Some(Event::Closed));
        assert_eq!((state.update(Message::Close).1, state.update(Message::Submit).1), (None, None));
    }

    #[test]
    fn shortcuts_mode_lists_keyed_commands_and_clicks_run_rows() {
        let map = Keymap::defaults(Platform::Other);
        let panel = Command::new(ids::PANEL_TOGGLE, "Toggle panel", "View").shortcut(map.chord_for(&ids::PANEL_TOGGLE));
        let keys = Command::new(id("keys"), "Sessions", "Keys").keys("1–9").enabled(false);
        let mut state = palette(vec![Command::new(id("a"), "Alpha", "S"), panel, keys]);
        let _ = state.update(Message::OpenShortcuts);
        let hints: Vec<_> = state.entries().map(|e| state.hint(e.command)).collect();
        assert_eq!(hints, [Some("Ctrl+B".into()), Some("1–9".into())]);
        let mut ui = iced_test::simulator(state.view());
        ui.click("Ctrl+B").unwrap();
        ui.click("1–9").unwrap();
        let events: Vec<_> = ui.into_messages().filter_map(|m| state.update(m).1).collect();
        assert_eq!(events, [Event::Run(ids::PANEL_TOGGLE)]);
        let _ = state.update(Message::Open);
        assert!(!state.shortcuts_only() && state.entries().count() == 3);
    }
}
