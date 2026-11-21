//! The Ctrl/Cmd+K command palette over registered [`Command`]s, plus the keymap.

mod keymap;

pub use keymap::{Chord, ChordKey, Keymap, Platform, Scope};

use iced::keyboard::{self, key::Named};
use iced::widget::{Column, button, column, container, mouse_area, opaque, operation, rich_text, row, rule, scrollable, space, span, text, text_input};
use iced::{Element, Fill, Font, Padding, Subscription, Task, font};
use loa_ui_kit::icons::tinted;
use loa_view::theme::Icon;
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::text::fuzzy::{Matcher, highlight_runs};
use loa_ui_kit::theme::{LayoutRole, SpaceRole as Sp, TextRole as T, pad_x, pad_y, sp, text_size as ts, width};
use loa_ui_kit::widgets::{label, scroll_target};
use std::{borrow::Cow, cmp::Reverse};

const ROW_HEIGHT: f32 = 35.0;
const HEADER_HEIGHT: f32 = 35.0;
const MAX_LIST_HEIGHT: f32 = 360.0;
const MAX_RECENT: usize = 32;
const INPUT_ID: &str = "loa-palette-search";
const LIST_ID: &str = "loa-palette-list";

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandId(Cow<'static, str>);

impl CommandId {
    pub const fn from_static(id: &'static str) -> Self { Self(Cow::Borrowed(id)) }
    pub fn new(id: impl Into<String>) -> Self { Self(Cow::Owned(id.into())) }
    pub fn as_str(&self) -> &str { &self.0 }
}

pub mod ids {
    use super::CommandId;
    macro_rules! ids { ($($name:ident = $id:literal),* $(,)?) => { $(pub const $name: CommandId = CommandId::from_static($id);)* } }
    ids! {
        PALETTE_TOGGLE = "palette.toggle", PANEL_TOGGLE = "panel.toggle", SESSION_INTERRUPT = "session.interrupt", COMPOSER_SUBMIT = "composer.submit",
        COMPOSER_NEWLINE = "composer.newline", SESSION_NEW = "session.new", SESSION_NEXT = "session.next", SESSION_PREVIOUS = "session.previous",
        PROJECT_TAB_NEXT = "project.tab.next", PROJECT_TAB_PREVIOUS = "project.tab.previous", COMPOSER_FOCUS = "composer.focus",
        SEARCH_FOCUS = "sidebar.search", KEYMAP_SHOW = "keymap.show", FOCUS_NEXT = "focus.next", FOCUS_PREVIOUS = "focus.previous",
    }
    /// Prefixes of `session.goto.<n>` (the n-th session in the sidebar) and `panel.tab.<n>`, 1 based.
    pub const SESSION_GOTO: &str = "session.goto.";
    pub const PANEL_TAB: &str = "panel.tab.";
    pub fn session_goto(n: usize) -> CommandId { CommandId::new(format!("{SESSION_GOTO}{n}")) }
    pub fn panel_tab(n: usize) -> CommandId { CommandId::new(format!("{PANEL_TAB}{n}")) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: CommandId,
    pub title: String,
    pub section: String,
    pub shortcut: Option<Chord>,
    /// A hint for keys that aren't one chord (e.g. `1–9`); wins over `shortcut`.
    pub keys: Option<String>,
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

/// A listed command with its matched title chars, maybe under a section header.
#[derive(Debug, Clone)]
struct Row { command: usize, indices: Vec<usize>, starts_section: bool }
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
    shortcuts_only: bool,
    rows: Vec<Row>,
    selected: Option<usize>,
    scroll_offset: f32,
    matcher: Matcher,
}

#[derive(Debug, Clone)]
pub enum Message { Open, OpenShortcuts, Close, Toggle, QueryChanged(String), SelectNext, SelectPrevious, Submit, Activate(usize), Scrolled(f32) }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Run(CommandId),
    /// Closed without running anything; give focus back.
    Closed,
}

impl State {
    pub fn new(platform: Platform) -> Self {
        let (commands, recent, rows, query) = Default::default();
        Self { platform, commands, recent, open: false, query, shortcuts_only: false, rows, selected: None, scroll_offset: 0.0, matcher: Matcher::new() }
    }
    pub fn set_commands(&mut self, commands: Vec<Command>) {
        self.commands = commands;
        self.refilter();
    }
    pub fn commands(&self) -> &[Command] { &self.commands }
    /// While open, the app resolves keys in [`Scope::Palette`].
    pub fn is_open(&self) -> bool { self.open }
    pub fn shortcuts_only(&self) -> bool { self.shortcuts_only }
    pub fn entries(&self) -> impl Iterator<Item = &Command> { self.rows.iter().map(|r| &self.commands[r.command]) }
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
        const KEYS: [(Named, Message); 4] = [(Named::ArrowUp, Message::SelectPrevious), (Named::ArrowDown, Message::SelectNext), (Named::Enter, Message::Submit), (Named::Escape, Message::Close)];
        if !self.open { return Subscription::none() }
        iced::event::listen_with(|event, _, _| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed { key: keyboard::Key::Named(named), .. }) => KEYS.iter().find(|(k, _)| *k == named).map(|(_, m)| m.clone()),
            _ => None,
        })
    }
    pub fn view(&self) -> Element<'_, Message> {
        if !self.open { return space().into() }
        let placeholder = if self.shortcuts_only { "Search keyboard shortcuts" } else { "Type a command" };
        let bare = |theme: &iced::Theme, status| text_input::Style { background: iced::Color::TRANSPARENT.into(), border: iced::Border::default(), ..style::input(theme, status) };
        let input = text_input(placeholder, &self.query).id(INPUT_ID).on_input(Message::QueryChanged).padding(pad_y(Sp::S)).size(ts(T::Body)).style(bare);
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
            let list = scrollable(list).id(LIST_ID).height(self.list_height()).on_scroll(|v| Message::Scrolled(v.absolute_offset().y));
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
        Task::batch([operation::focus(INPUT_ID), self.scroll_to(0.0)])
    }
    fn scroll_to(&mut self, y: f32) -> Task<Message> {
        self.scroll_offset = y;
        operation::scroll_to(LIST_ID, operation::AbsoluteOffset { x: None, y: Some(y) })
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
        let start = self.selected.map_or(if forward { 0 } else { n.saturating_sub(1) }, |i| if forward { i + 1 } else { i + n - 1 });
        let mut order = (0..n).map(|k| if forward { (start + k) % n } else { (start + n - k) % n });
        let Some(next) = order.find(|&i| self.commands[self.rows[i].command].enabled) else { return Task::none() };
        self.selected = Some(next);
        // A section's header counts as part of its first row, so scrolling up shows it.
        let top: f32 = self.rows[..next].iter().map(Row::height).sum();
        scroll_target(self.scroll_offset, self.list_height(), top, top + self.rows[next].height()).map_or_else(Task::none, |y| self.scroll_to(y))
    }
    fn list_height(&self) -> f32 { self.rows.iter().map(Row::height).sum::<f32>().min(MAX_LIST_HEIGHT) }
    /// Ranks by match class, score, recency and registration, then groups sections by their best result.
    fn refilter(&mut self) {
        self.matcher.set_query(&self.query);
        let mut ranked: Vec<_> = (self.commands.iter().enumerate())
            .filter(|(_, c)| !self.shortcuts_only || c.keys.is_some() || c.shortcut.is_some())
            .filter_map(|(i, c)| {
                let found = self.matcher.matches(&c.title)?;
                let recency = self.recent.iter().position(|r| *r == c.id).unwrap_or(usize::MAX);
                Some(((found.class, Reverse(found.score), recency, i), found.indices))
            })
            .collect();
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

    fn palette(commands: Vec<Command>) -> State {
        let mut state = State::new(Platform::Other);
        state.set_commands(commands);
        state.open = true;
        state
    }

    /// Sends the messages and lists the rows, as `# title` at a section start.
    fn listed(state: &mut State, messages: Vec<Message>) -> Vec<String> {
        messages.into_iter().for_each(|m| drop(state.update(m)));
        state.rows.iter().map(|r| format!("{}{}", if r.starts_section { "# " } else { "" }, state.commands[r.command].title)).collect()
    }

    #[test]
    fn filtering_ranks_groups_sections_and_prefers_recent() {
        let commands = [("find", "Find in files", "File"), ("tree", "File tree", "View"), ("open", "Open file", "File"), ("format", "Format imports", "File"), ("panel", "Toggle panel", "View"), ("term", "Toggle terminal", "View")];
        let mut state = palette(commands.iter().map(|(i, t, s)| Command::new(CommandId::from_static(i), *t, *s)).collect());
        let q = |s: &str| Message::QueryChanged(s.into());
        let cases: Vec<(Vec<Message>, Vec<&str>)> = vec![
            (vec![q("fi")], vec!["# Find in files", "Open file", "Format imports", "# File tree"]),
            (vec![q("tog")], vec!["# Toggle panel", "Toggle terminal"]),
            (vec![q("terminal"), Message::Submit, Message::Open, q("tog")], vec!["# Toggle terminal", "Toggle panel"]),
            (vec![q("pan")], vec!["# Toggle panel"]),
            (vec![q("zzz")], vec![]),
            (vec![Message::Close, Message::OpenShortcuts], vec![]),
        ];
        for (messages, want) in cases { assert_eq!(listed(&mut state, messages), want) }
        listed(&mut state, vec![Message::Open, q("tp")]);
        assert_eq!(state.rows[0].indices, [0, 7]);
    }

    #[test]
    fn navigation_skips_disabled_wraps_scrolls_and_closes() {
        let mut state = palette((0..20).map(|i| Command::new(CommandId::new(format!("c{i}")), format!("Command {i:02}"), "S").enabled(i != 1)).collect());
        for (steps, message, want, offset) in [(10, Message::SelectNext, 11, 12.0), (11, Message::SelectPrevious, 19, 20.0)] {
            listed(&mut state, vec![message; steps]);
            assert_eq!((state.rows[state.selected.unwrap()].command, state.scroll_offset), (want, HEADER_HEIGHT + offset * ROW_HEIGHT - MAX_LIST_HEIGHT));
        }
        assert_eq!(state.update(Message::Activate(1)).1, None, "disabled rows do not run");
        assert_eq!(state.update(Message::Toggle).1, Some(Event::Closed));
        assert_eq!((state.update(Message::Close).1, state.update(Message::Submit).1), (None, None));
    }

    #[test]
    fn shortcuts_mode_lists_keyed_commands_and_clicks_run_rows() {
        let panel = Command::new(ids::PANEL_TOGGLE, "Toggle panel", "View").shortcut(Keymap::defaults(Platform::Other).chord_for(&ids::PANEL_TOGGLE));
        let keys = Command::new(CommandId::from_static("keys"), "Sessions", "Keys").keys("1–9").enabled(false);
        let mut state = palette(vec![Command::new(CommandId::from_static("a"), "Alpha", "S"), panel, keys]);
        listed(&mut state, vec![Message::OpenShortcuts]);
        assert_eq!(state.entries().map(|c| state.hint(c)).collect::<Vec<_>>(), [Some("Ctrl+B".into()), Some("1–9".into())]);
        let mut ui = iced_test::simulator(state.view());
        ui.click("Ctrl+B").unwrap();
        ui.click("1–9").unwrap();
        assert_eq!(ui.into_messages().filter_map(|m| state.update(m).1).collect::<Vec<_>>(), [Event::Run(ids::PANEL_TOGGLE)]);
        listed(&mut state, vec![Message::Open]);
        assert!(!state.shortcuts_only() && state.entries().count() == 3);
    }
}
