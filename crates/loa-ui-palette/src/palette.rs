//! The command palette overlay.
//!
//! The app registers [`Command`]s, opens the palette when the keymap resolves
//! [`crate::ids::PALETTE_TOGGLE`], stacks [`State::view`] over its content,
//! and runs the id carried by [`Event::Run`].
//!
//! Accessibility: the palette is built only from standard iced widgets (a
//! `text_input`, a `scrollable` of `button`s and `rich_text`). Role: dialog
//! with a search field and a list of command buttons; each button's label is
//! the command title.

use std::cmp::Reverse;

use iced::keyboard::{self, key::Named};
use iced::widget::{
    Column, Id, button, column, container, mouse_area, opaque, operation, rich_text, row, rule,
    scrollable, space, span, text, text_input,
};
use iced::{Element, Fill, Font, Length, Subscription, Task, Theme, font};
use loa_ui_icons::Icon;

use crate::CommandId;
use crate::fuzzy::{Class, Matcher, highlight_runs};
use crate::keymap::{Chord, Platform};

/// Height of one command row, in logical pixels.
pub const ROW_HEIGHT: f32 = 35.0;
/// Height of a section header, in logical pixels.
pub const HEADER_HEIGHT: f32 = 35.0;
/// Tallest the result list grows before it scrolls.
pub const MAX_LIST_HEIGHT: f32 = 360.0;
/// Width of the palette card.
pub const WIDTH: f32 = 520.0;
/// How many recently run commands are remembered for ranking.
pub const MAX_RECENT: usize = 32;

/// A command the app offers in the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// What the app runs.
    pub id: CommandId,
    /// What the user reads and searches.
    pub title: String,
    /// Group heading, e.g. "Session" or "View".
    pub section: String,
    /// Shortcut shown as a hint, usually from [`crate::Keymap::chord_for`].
    pub shortcut: Option<Chord>,
    /// Disabled commands are listed but cannot be selected or run.
    pub enabled: bool,
}

impl Command {
    /// An enabled command without a shortcut.
    pub fn new(id: CommandId, title: impl Into<String>, section: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            section: section.into(),
            shortcut: None,
            enabled: true,
        }
    }

    /// Sets the shortcut hint.
    pub fn shortcut(mut self, chord: Option<Chord>) -> Self {
        self.shortcut = chord;
        self
    }

    /// Sets whether the command can run.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// One listed result, in display order.
#[derive(Debug, Clone)]
struct Row {
    /// Index into `State::commands`.
    command: usize,
    /// Matched char indices in the title.
    indices: Vec<usize>,
    /// First row of its section: a header is drawn above it.
    starts_section: bool,
}

/// Palette state.
#[derive(Debug)]
pub struct State {
    platform: Platform,
    commands: Vec<Command>,
    recent: Vec<CommandId>,
    open: bool,
    query: String,
    rows: Vec<Row>,
    selected: Option<usize>,
    scroll_offset: f32,
    matcher: Matcher,
    input_id: Id,
    list_id: Id,
}

/// Internal palette messages.
#[derive(Debug, Clone)]
pub enum Message {
    /// Open with an empty query.
    Open,
    /// Close without running anything.
    Close,
    /// Open when closed, close when open.
    Toggle,
    /// The query text changed.
    QueryChanged(String),
    /// Move the selection down (wraps).
    SelectNext,
    /// Move the selection up (wraps).
    SelectPrevious,
    /// Run the selected command.
    Submit,
    /// Run the command in the given display row (mouse click).
    Activate(usize),
    /// The result list scrolled to this vertical offset.
    Scrolled(f32),
}

/// What the app must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Run this command. The palette has closed itself.
    Run(CommandId),
    /// The palette closed without running anything; give focus back.
    Closed,
}

/// A result as the app or a test sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    /// The command.
    pub command: &'a Command,
    /// Char indices of the title characters the query matched.
    pub matched: &'a [usize],
    /// Whether a section header precedes this entry.
    pub starts_section: bool,
    /// Whether it is the selected entry.
    pub selected: bool,
}

impl State {
    /// A closed palette with no commands, labelling shortcuts for `platform`.
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            commands: Vec::new(),
            recent: Vec::new(),
            open: false,
            query: String::new(),
            rows: Vec::new(),
            selected: None,
            scroll_offset: 0.0,
            matcher: Matcher::new(),
            input_id: Id::unique(),
            list_id: Id::unique(),
        }
    }

    /// Replaces the registered commands. Registration order breaks the last
    /// ranking ties and orders sections when nothing else does.
    pub fn set_commands(&mut self, commands: Vec<Command>) {
        self.commands = commands;
        self.refilter();
    }

    /// The registered commands.
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// Enables or disables a registered command. Returns false when no
    /// command has that id.
    pub fn set_enabled(&mut self, id: &CommandId, enabled: bool) -> bool {
        let Some(command) = self.commands.iter_mut().find(|c| c.id == *id) else {
            return false;
        };
        command.enabled = enabled;
        self.refilter();
        true
    }

    /// Recently run command ids, most recent first, for persisting.
    pub fn recent(&self) -> &[CommandId] {
        &self.recent
    }

    /// Restores recently run ids, most recent first.
    pub fn set_recent(&mut self, mut recent: Vec<CommandId>) {
        recent.truncate(MAX_RECENT);
        self.recent = recent;
        self.refilter();
    }

    /// Records that `id` ran, e.g. through its shortcut, so it ranks first
    /// on ties.
    pub fn note_used(&mut self, id: &CommandId) {
        if let Some(pos) = self.recent.iter().position(|r| r == id) {
            let used = self.recent.remove(pos);
            self.recent.insert(0, used);
        } else {
            self.recent.insert(0, id.clone());
            self.recent.truncate(MAX_RECENT);
        }
    }

    /// Whether the palette is showing. While it is, the app resolves keys in
    /// [`crate::Scope::Palette`].
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The current query.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The results in display order.
    pub fn entries(&self) -> impl Iterator<Item = Entry<'_>> {
        self.rows.iter().enumerate().map(|(i, row)| Entry {
            command: &self.commands[row.command],
            matched: &row.indices,
            starts_section: row.starts_section,
            selected: self.selected == Some(i),
        })
    }

    /// The selected command, if any.
    pub fn selected(&self) -> Option<&Command> {
        self.selected
            .map(|i| &self.commands[self.rows[i].command])
    }

    /// The shortcut hint for a command on this platform.
    pub fn hint(&self, command: &Command) -> Option<String> {
        command.shortcut.map(|c| c.label(self.platform))
    }

    /// Handles a message.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Open => (self.open(), None),
            Message::Close => self.close(),
            Message::Toggle if self.open => self.close(),
            Message::Toggle => (self.open(), None),
            Message::QueryChanged(query) if self.open => {
                self.query = query;
                self.refilter();
                self.scroll_offset = 0.0;
                (
                    operation::scroll_to(self.list_id.clone(), operation::AbsoluteOffset {
                        x: None,
                        y: Some(0.0),
                    }),
                    None,
                )
            }
            Message::SelectNext if self.open => (self.step(true), None),
            Message::SelectPrevious if self.open => (self.step(false), None),
            Message::Submit if self.open => match self.selected {
                Some(i) => self.run(i),
                None => (Task::none(), None),
            },
            Message::Activate(i) if self.open => self.run(i),
            Message::Scrolled(offset) => {
                self.scroll_offset = offset;
                (Task::none(), None)
            }
            Message::QueryChanged(_)
            | Message::SelectNext
            | Message::SelectPrevious
            | Message::Submit
            | Message::Activate(_) => (Task::none(), None),
        }
    }

    /// Navigation keys while open: Up, Down, Enter and Esc. Listens to
    /// captured events too, because the focused search field captures Esc.
    pub fn subscription(&self) -> Subscription<Message> {
        if !self.open {
            return Subscription::none();
        }
        iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(named),
                ..
            }) => match named {
                Named::ArrowUp => Some(Message::SelectPrevious),
                Named::ArrowDown => Some(Message::SelectNext),
                Named::Enter => Some(Message::Submit),
                Named::Escape => Some(Message::Close),
                _ => None,
            },
            _ => None,
        })
    }

    /// The overlay: a backdrop that closes the palette on click, with the
    /// card centered near the top. Stack it over the app content. Closed, it
    /// is an empty space.
    pub fn view(&self) -> Element<'_, Message> {
        if !self.open {
            return space().into();
        }

        let input = text_input("Type a command", &self.query)
            .id(self.input_id.clone())
            .on_input(Message::QueryChanged)
            .padding([7, 0])
            .size(14)
            .style(Self::search_style);
        let search = container(
            row![loa_ui_icons::muted(Icon::Search, 14.0), input].spacing(6).align_y(iced::Center),
        )
        .padding([0, 13]);

        let results: Element<'_, Message> = if self.rows.is_empty() {
            container(text("No matching commands").size(14).style(Self::muted_text))
                .padding(12)
                .into()
        } else {
            let mut list = Column::with_capacity(self.rows.len() * 2);
            for (i, row) in self.rows.iter().enumerate() {
                let command = &self.commands[row.command];
                if row.starts_section {
                    list = list.push(
                        container(text(&command.section).size(12).style(Self::muted_text))
                            .height(HEADER_HEIGHT)
                            .padding(iced::Padding::ZERO.left(13).bottom(7))
                            .align_y(iced::Bottom),
                    );
                }
                list = list.push(container(self.row_view(i, row, command)).padding([0, 4]));
            }
            container(
                scrollable(list)
                    .id(self.list_id.clone())
                    .height(Length::Fixed(self.list_height()))
                    .on_scroll(|viewport| Message::Scrolled(viewport.absolute_offset().y)),
            )
            .width(Fill)
            .padding(iced::Padding::ZERO.bottom(4))
            .into()
        };

        // Inset children past the 1px border so they never paint over it.
        let card = container(column![search, rule::horizontal(1).style(Self::divider), results])
            .padding(1)
            .width(WIDTH)
            .style(Self::card_style);

        let backdrop = container(opaque(card))
            .width(Fill)
            .height(Fill)
            .center_x(Fill)
            .padding(iced::Padding::ZERO.top(120));

        opaque(mouse_area(backdrop).on_press(Message::Close))
    }

    fn row_view<'a>(&'a self, index: usize, row: &'a Row, command: &'a Command) -> Element<'a, Message> {
        let bold = Font {
            weight: font::Weight::Bold,
            ..Font::DEFAULT
        };
        let spans: Vec<text::Span<'a, (), Font>> = highlight_runs(&command.title, &row.indices)
            .into_iter()
            .map(|(range, hit)| {
                let piece = span(&command.title[range]);
                if hit { piece.font(bold) } else { piece }
            })
            .collect();

        let mut content = row![rich_text(spans).size(14), space::horizontal()]
            .height(Fill)
            .align_y(iced::Center)
            .spacing(12);
        if let Some(hint) = self.hint(command) {
            content = content.push(text(hint).size(12).style(Self::muted_text));
        }

        let selected = self.selected == Some(index);
        button(content)
            .width(Fill)
            .height(ROW_HEIGHT)
            .padding([0, 8])
            .on_press_maybe(command.enabled.then_some(Message::Activate(index)))
            .style(move |theme: &Theme, status| {
                let palette = theme.extended_palette();
                let mut style = button::text(theme, status);
                style.border.radius = 6.0.into();
                if selected {
                    style.background = Some(palette.background.neutral.color.into());
                    style.text_color = palette.background.neutral.text;
                } else if matches!(status, button::Status::Hovered) {
                    style.background = Some(palette.background.weak.color.into());
                    style.text_color = palette.background.weak.text;
                }
                style
            })
            .into()
    }

    /// Secondary text: the palette's text at reduced opacity.
    fn muted_text(theme: &Theme) -> text::Style {
        text::Style { color: Some(theme.palette().text.scale_alpha(0.55)) }
    }

    /// The search field: no box of its own, it sits on the card.
    fn search_style(theme: &Theme, _status: text_input::Status) -> text_input::Style {
        let palette = theme.extended_palette();
        let muted = theme.palette().text.scale_alpha(0.55);
        text_input::Style {
            background: iced::Color::TRANSPARENT.into(),
            border: iced::Border::default(),
            icon: muted,
            placeholder: muted,
            value: palette.background.base.text,
            selection: palette.background.stronger.color,
        }
    }

    /// The card: a shade above the window background, with a 1px neutral border.
    fn card_style(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        let raised = iced::theme::palette::mix(palette.background.base.color, palette.background.weakest.color, 0.3);
        container::Style {
            background: Some(raised.into()),
            border: iced::Border { color: palette.background.neutral.color, width: 1.0, radius: 6.0.into() },
            ..container::Style::default()
        }
    }

    /// A 1px rule between the search field and the list.
    fn divider(theme: &Theme) -> rule::Style {
        rule::Style { color: theme.extended_palette().background.neutral.color, ..rule::default(theme) }
    }

    fn open(&mut self) -> Task<Message> {
        self.open = true;
        self.query.clear();
        self.scroll_offset = 0.0;
        self.refilter();
        Task::batch([
            operation::focus(self.input_id.clone()),
            operation::scroll_to(self.list_id.clone(), operation::AbsoluteOffset {
                x: None,
                y: Some(0.0),
            }),
        ])
    }

    fn close(&mut self) -> (Task<Message>, Option<Event>) {
        if !self.open {
            return (Task::none(), None);
        }
        self.open = false;
        (Task::none(), Some(Event::Closed))
    }

    fn run(&mut self, index: usize) -> (Task<Message>, Option<Event>) {
        let Some(command) = self.rows.get(index).map(|r| &self.commands[r.command]) else {
            return (Task::none(), None);
        };
        if !command.enabled {
            return (Task::none(), None);
        }
        let id = command.id.clone();
        self.open = false;
        self.note_used(&id);
        (Task::none(), Some(Event::Run(id)))
    }

    /// Moves the selection to the next (or previous) enabled row, wrapping,
    /// and scrolls it into view.
    fn step(&mut self, forward: bool) -> Task<Message> {
        let n = self.rows.len();
        if n == 0 {
            return Task::none();
        }
        let start = match (self.selected, forward) {
            (Some(i), true) => i + 1,
            (Some(i), false) => i + n - 1,
            (None, true) => 0,
            (None, false) => n - 1,
        };
        let next = (0..n)
            .map(|k| {
                if forward {
                    (start + k) % n
                } else {
                    (start + n - k) % n
                }
            })
            .find(|&i| self.commands[self.rows[i].command].enabled);
        let Some(next) = next else {
            return Task::none();
        };
        self.selected = Some(next);

        let (top, bottom) = self.row_extent(next);
        match scroll_target(self.scroll_offset, self.list_height(), top, bottom) {
            Some(y) => {
                self.scroll_offset = y;
                operation::scroll_to(self.list_id.clone(), operation::AbsoluteOffset {
                    x: None,
                    y: Some(y),
                })
            }
            None => Task::none(),
        }
    }

    /// Top and bottom of a row inside the list content. A section's header
    /// counts as part of its first row, so scrolling up shows the header.
    fn row_extent(&self, index: usize) -> (f32, f32) {
        let mut y = 0.0;
        for row in &self.rows[..index] {
            y += row_block_height(row);
        }
        (y, y + row_block_height(&self.rows[index]))
    }

    fn list_height(&self) -> f32 {
        self.rows
            .iter()
            .map(row_block_height)
            .sum::<f32>()
            .min(MAX_LIST_HEIGHT)
    }

    /// Recomputes the results for the current query and selects the first
    /// enabled one.
    fn refilter(&mut self) {
        self.matcher.set_query(&self.query);
        let mut ranked: Vec<(RankKey, Vec<usize>)> = Vec::new();
        for (index, command) in self.commands.iter().enumerate() {
            if let Some(found) = self.matcher.matches(&command.title) {
                let recency = self
                    .recent
                    .iter()
                    .position(|r| *r == command.id)
                    .unwrap_or(usize::MAX);
                let key = (found.class, Reverse(found.score), recency, index);
                ranked.push((key, found.indices));
            }
        }
        ranked.sort_unstable_by_key(|(key, _)| *key);

        // Group by section, sections ordered by their best result.
        let mut sections: Vec<&str> = Vec::new();
        let mut keyed: Vec<(usize, usize, Vec<usize>)> = Vec::with_capacity(ranked.len());
        for ((_, _, _, index), indices) in ranked {
            let section = self.commands[index].section.as_str();
            let rank = match sections.iter().position(|s| *s == section) {
                Some(rank) => rank,
                None => {
                    sections.push(section);
                    sections.len() - 1
                }
            };
            keyed.push((rank, index, indices));
        }
        // Stable: keeps rank order inside each section.
        keyed.sort_by_key(|(rank, _, _)| *rank);

        self.rows.clear();
        let mut previous = None;
        for (rank, command, indices) in keyed {
            self.rows.push(Row {
                command,
                indices,
                starts_section: previous != Some(rank),
            });
            previous = Some(rank);
        }
        self.selected = self
            .rows
            .iter()
            .position(|r| self.commands[r.command].enabled);
    }
}

/// Sort key of a result: class, score (higher first), recency, registration.
type RankKey = (Class, Reverse<i32>, usize, usize);

fn row_block_height(row: &Row) -> f32 {
    ROW_HEIGHT + if row.starts_section { HEADER_HEIGHT } else { 0.0 }
}

/// The scroll offset that brings `top..bottom` into a viewport of `height`
/// currently scrolled to `offset`, or `None` when it is already visible.
pub fn scroll_target(offset: f32, height: f32, top: f32, bottom: f32) -> Option<f32> {
    if top < offset {
        Some(top)
    } else if bottom > offset + height {
        Some(bottom - height)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids;
    use crate::keymap::Keymap;

    fn id(s: &'static str) -> CommandId {
        CommandId::from_static(s)
    }

    fn palette(commands: Vec<Command>) -> State {
        let mut state = State::new(Platform::Other);
        state.set_commands(commands);
        let _ = state.update(Message::Open);
        state
    }

    fn titles(state: &State) -> Vec<&str> {
        state.entries().map(|e| e.command.title.as_str()).collect()
    }

    fn query(state: &mut State, q: &str) {
        let _ = state.update(Message::QueryChanged(q.to_owned()));
    }

    fn run(state: &mut State, q: &str) -> Option<Event> {
        let _ = state.update(Message::Open);
        query(state, q);
        state.update(Message::Submit).1
    }

    #[test]
    fn prefix_and_word_matches_rank_above_fuzzy() {
        let mut state = palette(vec![
            Command::new(id("profile"), "Profile", "S"),
            Command::new(id("format"), "Format imports", "S"),
            Command::new(id("open"), "Open file", "S"),
            Command::new(id("find"), "Find in files", "S"),
            Command::new(id("theme"), "Switch theme", "S"),
        ]);
        query(&mut state, "fi");
        assert_eq!(
            titles(&state),
            vec!["Find in files", "Open file", "Format imports", "Profile"]
        );
        assert_eq!(state.selected().map(|c| c.id.clone()), Some(id("find")));
    }

    #[test]
    fn sections_are_ordered_by_their_best_result() {
        let mut state = palette(vec![
            Command::new(id("open"), "Open file", "File"),
            Command::new(id("tree"), "File tree", "View"),
            Command::new(id("find"), "Find in files", "File"),
        ]);
        query(&mut state, "file");
        let listed: Vec<(&str, bool)> = state
            .entries()
            .map(|e| (e.command.title.as_str(), e.starts_section))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("File tree", true),
                ("Open file", true),
                ("Find in files", false),
            ]
        );
    }

    #[test]
    fn recently_used_breaks_ties() {
        let mut state = palette(vec![
            Command::new(id("panel"), "Toggle panel", "View"),
            Command::new(id("term"), "Toggle terminal", "View"),
        ]);
        query(&mut state, "tog");
        assert_eq!(titles(&state), vec!["Toggle panel", "Toggle terminal"]);

        assert_eq!(run(&mut state, "terminal"), Some(Event::Run(id("term"))));
        assert!(!state.is_open());

        let _ = state.update(Message::Open);
        query(&mut state, "tog");
        assert_eq!(titles(&state), vec!["Toggle terminal", "Toggle panel"]);

        // Recency never beats a better match.
        query(&mut state, "pan");
        assert_eq!(titles(&state), vec!["Toggle panel"]);
    }

    #[test]
    fn empty_query_lists_recent_first_grouped_by_section() {
        let mut state = palette(vec![
            Command::new(id("new"), "New session", "Session"),
            Command::new(id("fork"), "Fork session", "Session"),
            Command::new(id("panel"), "Toggle panel", "View"),
            Command::new(id("theme"), "Switch theme", "View"),
        ]);
        state.set_recent(vec![id("theme")]);
        let _ = state.update(Message::Open);
        let listed: Vec<(&str, bool)> = state
            .entries()
            .map(|e| (e.command.title.as_str(), e.starts_section))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("Switch theme", true),
                ("Toggle panel", false),
                ("New session", true),
                ("Fork session", false),
            ]
        );
    }

    #[test]
    fn matched_characters_are_reported_for_highlighting() {
        let mut state = palette(vec![Command::new(id("panel"), "Toggle panel", "View")]);
        query(&mut state, "tp");
        let entry = state.entries().next().unwrap();
        assert_eq!(entry.matched, &[0, 7]);
        let runs: Vec<(&str, bool)> = highlight_runs(&entry.command.title, entry.matched)
            .into_iter()
            .map(|(r, hit)| (&entry.command.title[r], hit))
            .collect();
        assert_eq!(
            runs,
            vec![("T", true), ("oggle ", false), ("p", true), ("anel", false)]
        );
    }

    #[test]
    fn navigation_skips_disabled_and_wraps() {
        let mut state = palette(vec![
            Command::new(id("a"), "Alpha", "S"),
            Command::new(id("b"), "Bravo", "S").enabled(false),
            Command::new(id("c"), "Charlie", "S"),
        ]);
        assert_eq!(state.selected().unwrap().id, id("a"));
        let _ = state.update(Message::SelectNext);
        assert_eq!(state.selected().unwrap().id, id("c"));
        let _ = state.update(Message::SelectNext);
        assert_eq!(state.selected().unwrap().id, id("a"));
        let _ = state.update(Message::SelectPrevious);
        assert_eq!(state.selected().unwrap().id, id("c"));

        assert_eq!(state.update(Message::Submit).1, Some(Event::Run(id("c"))));
        assert_eq!(state.recent(), &[id("c")]);
    }

    #[test]
    fn disabled_and_missing_commands_do_not_run() {
        let mut state = palette(vec![
            Command::new(id("a"), "Alpha", "S"),
            Command::new(id("b"), "Bravo", "S").enabled(false),
        ]);
        assert_eq!(state.update(Message::Activate(1)).1, None);
        assert!(state.is_open());

        query(&mut state, "bravo");
        assert_eq!(state.selected(), None);
        assert_eq!(state.update(Message::Submit).1, None);

        query(&mut state, "zzz");
        assert_eq!(state.entries().count(), 0);
        assert_eq!(state.update(Message::Submit).1, None);

        assert!(state.set_enabled(&id("b"), true));
        query(&mut state, "bravo");
        assert_eq!(state.update(Message::Submit).1, Some(Event::Run(id("b"))));
    }

    #[test]
    fn escape_and_toggle_close_without_running() {
        let mut state = palette(vec![Command::new(id("a"), "Alpha", "S")]);
        assert_eq!(state.update(Message::Close).1, Some(Event::Closed));
        assert!(!state.is_open());
        assert_eq!(state.update(Message::Close).1, None);
        assert_eq!(state.update(Message::Submit).1, None, "closed palette ignores Enter");

        let _ = state.update(Message::Toggle);
        assert!(state.is_open());
        assert_eq!(state.update(Message::Toggle).1, Some(Event::Closed));
    }

    #[test]
    fn reopening_clears_the_query() {
        let mut state = palette(vec![
            Command::new(id("a"), "Alpha", "S"),
            Command::new(id("b"), "Bravo", "S"),
        ]);
        query(&mut state, "bra");
        let _ = state.update(Message::Close);
        let _ = state.update(Message::Open);
        assert_eq!(state.query(), "");
        assert_eq!(state.entries().count(), 2);
    }

    #[test]
    fn hints_come_from_the_keymap_per_platform() {
        let commands = |map: &Keymap| {
            vec![Command::new(ids::PANEL_TOGGLE, "Toggle panel", "View")
                .shortcut(map.chord_for(&ids::PANEL_TOGGLE))]
        };
        let mac_map = Keymap::defaults(Platform::MacOs);
        let mut mac = State::new(Platform::MacOs);
        mac.set_commands(commands(&mac_map));
        assert_eq!(mac.hint(&mac.commands()[0]).as_deref(), Some("⌘B"));

        let linux_map = Keymap::defaults(Platform::Other);
        let mut linux = State::new(Platform::Other);
        linux.set_commands(commands(&linux_map));
        assert_eq!(linux.hint(&linux.commands()[0]).as_deref(), Some("Ctrl+B"));
    }

    #[test]
    fn selection_scrolls_into_view() {
        // 20 rows in one section: content 35 + 20 * 35 = 735 px, viewport 360.
        let commands = (0..20)
            .map(|i| Command::new(CommandId::new(format!("c{i}")), format!("Command {i:02}"), "S"))
            .collect();
        let mut state = palette(commands);
        assert_eq!(state.list_height(), MAX_LIST_HEIGHT);

        for _ in 0..10 {
            let _ = state.update(Message::SelectNext);
        }
        // Row 10 spans 35 + 350 .. 35 + 385 = 385..420, so offset 60.
        assert_eq!(state.selected().unwrap().title, "Command 10");
        assert_eq!(state.scroll_offset, HEADER_HEIGHT + 11.0 * ROW_HEIGHT - MAX_LIST_HEIGHT);

        // Wrap to the last row from the top.
        for _ in 0..10 {
            let _ = state.update(Message::SelectPrevious);
        }
        assert_eq!(state.selected().unwrap().title, "Command 00");
        assert_eq!(state.scroll_offset, 0.0);
        let _ = state.update(Message::SelectPrevious);
        assert_eq!(state.selected().unwrap().title, "Command 19");
        assert_eq!(state.scroll_offset, HEADER_HEIGHT + 20.0 * ROW_HEIGHT - MAX_LIST_HEIGHT);
    }

    #[test]
    fn scroll_target_only_moves_when_needed() {
        assert_eq!(scroll_target(0.0, 100.0, 10.0, 40.0), None);
        assert_eq!(scroll_target(50.0, 100.0, 10.0, 40.0), Some(10.0));
        assert_eq!(scroll_target(0.0, 100.0, 90.0, 124.0), Some(24.0));
    }

    #[test]
    fn headless_clicks_run_rows_and_backdrop_closes() {
        let map = Keymap::defaults(Platform::Other);
        let mut state = palette(vec![
            Command::new(ids::PANEL_TOGGLE, "Toggle panel", "View")
                .shortcut(map.chord_for(&ids::PANEL_TOGGLE)),
            Command::new(ids::PALETTE_TOGGLE, "Command palette", "View")
                .shortcut(map.chord_for(&ids::PALETTE_TOGGLE))
                .enabled(false),
        ]);

        // Clicking the hint inside a row presses that row's button.
        let mut ui = iced_test::simulator(state.view());
        ui.click("Ctrl+B").unwrap();
        // A disabled row does nothing when clicked.
        ui.click("Ctrl+K").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages.len(), 1, "{messages:?}");
        let mut events = messages.into_iter().map(|m| state.update(m).1);
        assert_eq!(events.next(), Some(Some(Event::Run(ids::PANEL_TOGGLE))));
        assert!(!state.is_open());

        // Clicking outside the card closes the palette.
        let _ = state.update(Message::Open);
        query(&mut state, "nothing matches this");
        let mut ui = iced_test::simulator(state.view());
        assert!(ui.find("No matching commands").is_ok());
        ui.point_at(iced::Point::new(10.0, 700.0));
        let _ = ui.simulate(iced_test::simulator::click());
        let closed: Vec<Option<Event>> = ui.into_messages().map(|m| state.update(m).1).collect();
        assert_eq!(closed, vec![Some(Event::Closed)]);
    }
}
