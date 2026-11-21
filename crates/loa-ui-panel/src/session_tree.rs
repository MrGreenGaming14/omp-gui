//! Fork and rewind tree of a session family.
//!
//! The app hands over every session related by fork links with
//! [`State::set_family`]. The tree shows each session once, children under
//! the turn of their parent they forked from, and the turns of the current
//! session as rewind and fork targets.
//!
//! Layout rules:
//! - Roots are sessions without a parent in the family, in input order. A
//!   session whose ancestry cycles is a root too.
//! - Children are sorted by `forked_at`, then id, and attach to the parent's
//!   last turn whose `seq <= forked_at`. Children sharing that turn share one
//!   fork point row. A child without `forked_at` counts as forked at 0.
//! - Under the current session every turn is listed, each followed by the
//!   fork point that hangs off it. Forks before the first turn come first.
//!   Other sessions list their fork points directly.

use std::collections::{HashMap, HashSet};

use iced::font::{self, Font};
use iced::widget::text::Wrapping;
use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Center, Element, Fill, Task};
use loa_core::NodeId;
use loa_ui_kit::icons::Icon;

use crate::style;

/// One session of the family as the app knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// Session id.
    pub id: String,
    /// The session this one was forked from (`parent_session` on `<session>`).
    pub parent: Option<String>,
    /// Patch sequence number in the parent's journal where the fork starts
    /// (`forked_at` on `<session>`). The fork contains every patch of the
    /// parent up to and including this one.
    pub forked_at: Option<u64>,
    /// Display title; the id is shown when empty.
    pub title: String,
    /// Turns of the session in journal order. Only needed for the current
    /// session and for sessions that have forks.
    pub turns: Vec<TurnInfo>,
}

/// One turn of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnInfo {
    /// The turn element in the session DOM; the target of rewind and fork.
    pub node: NodeId,
    /// Patch sequence number that inserted the turn, from the journal. Maps a
    /// child's `forked_at` onto the turn it forked from. Ascending along
    /// [`SessionInfo::turns`].
    pub seq: u64,
    /// First line of the turn's prompt.
    pub first_line: String,
}

/// One visible row of the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row<'a> {
    /// Stable index of the row in the layout; the argument of every
    /// [`Message`] that targets a row. Unchanged by collapsing.
    pub index: usize,
    /// Nesting level, 0 for roots.
    pub depth: u16,
    /// What the row shows.
    pub kind: RowKind<'a>,
}

/// Content of a [`Row`], borrowed from the family passed to
/// [`State::set_family`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind<'a> {
    /// A session.
    Session {
        id: &'a str,
        title: &'a str,
        /// This is the session open in the chat.
        current: bool,
        /// This session is the current one or one of its ancestors.
        on_path: bool,
        /// The subtree below this row is hidden.
        collapsed: bool,
    },
    /// The point in `session` where the child rows below were forked.
    ForkPoint {
        session: &'a str,
        /// Sequence of the turn forked from, or the children's `forked_at`
        /// when no turn of the parent precedes it.
        seq: u64,
        /// First line of the turn forked from; `None` shows `at #<seq>`.
        first_line: Option<&'a str>,
    },
    /// A turn of the current session.
    Turn { session: &'a str, node: NodeId, first_line: &'a str },
}

/// Internal messages. Every `usize` is a [`Row::index`].
#[derive(Debug, Clone)]
pub enum Message {
    /// Collapse or expand the subtree of a session row.
    Toggle(usize),
    /// Rewind the current session to a turn row.
    Rewind(usize),
    /// Fork the current session at a turn row.
    Fork(usize),
    /// Open the session of a session row.
    Open(usize),
    /// Highlight a row.
    Select(usize),
}

/// What the app must act on.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Rewind `session` to just before `turn`.
    Rewind { session: String, turn: NodeId },
    /// Fork `session` at `turn` into a new session.
    Fork { session: String, turn: NodeId },
    /// Switch the chat to this session id.
    Open(String),
}

/// A layout row: indices into the family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Session(usize),
    ForkPoint { session: usize, seq: u64, turn: Option<usize> },
    Turn { session: usize, turn: usize },
}

/// Session tree tab.
#[derive(Debug, Default)]
pub struct State {
    sessions: Vec<SessionInfo>,
    /// Index of the current session in `sessions`, if it is in the family.
    current: Option<usize>,
    /// Per session: current session or an ancestor of it.
    on_path: Vec<bool>,
    /// Every row in tree order, with its depth.
    layout: Vec<(u16, Slot)>,
    /// Layout indices of the rows not hidden by a collapsed session.
    visible: Vec<usize>,
    /// Ids of collapsed sessions; kept across [`State::set_family`].
    collapsed: HashSet<String>,
    selected: Option<usize>,
}

impl State {
    /// An empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the family and rebuilds the layout. `current` is the id of
    /// the session open in the chat.
    pub fn set_family(&mut self, sessions: Vec<SessionInfo>, current: &str) {
        let tree = Tree::new(&sessions, current);
        let mut layout = Vec::with_capacity(sessions.len());
        for &root in &tree.roots {
            tree.emit(&sessions, root, 0, &mut layout);
        }
        self.on_path = tree.on_path;
        self.current = tree.current;
        self.layout = layout;
        self.sessions = sessions;
        let sessions = &self.sessions;
        self.collapsed.retain(|id| sessions.iter().any(|s| s.id == *id));
        self.selected = None;
        self.refresh_visible();
    }

    /// The visible rows in display order.
    pub fn rows(&self) -> impl Iterator<Item = Row<'_>> + '_ {
        self.visible.iter().map(|&index| self.row(index))
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::Toggle(index) => {
                if let Some(&(_, Slot::Session(session))) = self.layout.get(index) {
                    let id = &self.sessions[session].id;
                    if !self.collapsed.remove(id) {
                        self.collapsed.insert(id.clone());
                    }
                    self.refresh_visible();
                }
                None
            }
            Message::Rewind(index) => {
                self.current_turn(index).map(|(session, turn)| Event::Rewind { session, turn })
            }
            Message::Fork(index) => self.current_turn(index).map(|(session, turn)| Event::Fork { session, turn }),
            Message::Open(index) => match self.layout.get(index) {
                Some(&(_, Slot::Session(session))) if self.current != Some(session) => {
                    Some(Event::Open(self.sessions[session].id.clone()))
                }
                _ => None,
            },
            Message::Select(index) => {
                if index < self.layout.len() {
                    self.selected = Some(index);
                }
                None
            }
        };
        (Task::none(), event)
    }

    pub fn view(&self) -> Element<'_, Message> {
        scrollable(column(self.rows().map(|row| self.row_view(row))).padding([0, 6])).height(Fill).into()
    }

    /// Session id and turn node of a turn row of the current session.
    fn current_turn(&self, index: usize) -> Option<(String, NodeId)> {
        match self.layout.get(index) {
            Some(&(_, Slot::Turn { session, turn })) if self.current == Some(session) => {
                let info = &self.sessions[session];
                Some((info.id.clone(), info.turns[turn].node))
            }
            _ => None,
        }
    }

    fn refresh_visible(&mut self) {
        self.visible.clear();
        let mut hidden_below: Option<u16> = None;
        for (index, &(depth, slot)) in self.layout.iter().enumerate() {
            if let Some(limit) = hidden_below {
                if depth > limit {
                    continue;
                }
                hidden_below = None;
            }
            self.visible.push(index);
            if let Slot::Session(session) = slot
                && self.collapsed.contains(&self.sessions[session].id)
            {
                hidden_below = Some(depth);
            }
        }
    }

    fn row(&self, index: usize) -> Row<'_> {
        let (depth, slot) = self.layout[index];
        let kind = match slot {
            Slot::Session(session) => {
                let info = &self.sessions[session];
                RowKind::Session {
                    id: &info.id,
                    title: &info.title,
                    current: self.current == Some(session),
                    on_path: self.on_path[session],
                    collapsed: self.collapsed.contains(&info.id),
                }
            }
            Slot::ForkPoint { session, seq, turn } => {
                let info = &self.sessions[session];
                RowKind::ForkPoint {
                    session: &info.id,
                    seq,
                    first_line: turn.map(|turn| info.turns[turn].first_line.as_str()),
                }
            }
            Slot::Turn { session, turn } => {
                let info = &self.sessions[session];
                let turn = &info.turns[turn];
                RowKind::Turn { session: &info.id, node: turn.node, first_line: &turn.first_line }
            }
        };
        Row { index, depth, kind }
    }

    /// The layout row right after `index` is deeper, so `index` has a subtree.
    fn has_subtree(&self, index: usize) -> bool {
        match (self.layout.get(index), self.layout.get(index + 1)) {
            (Some(&(depth, _)), Some(&(next, _))) => next > depth,
            _ => false,
        }
    }

    fn row_view<'a>(&'a self, row: Row<'a>) -> Element<'a, Message> {
        let indent = space().width(style::INDENT * f32::from(row.depth));
        let selected = self.selected == Some(row.index);
        let content: Element<'a, Message> = match row.kind {
            RowKind::Session { id, title, current, on_path, collapsed } => {
                let chevron: Element<'a, Message> = if self.has_subtree(row.index) {
                    let (icon, label) = if collapsed { (Icon::ChevronRight, "Expand") } else { (Icon::ChevronDown, "Collapse") };
                    let toggle = button(loa_ui_kit::icons::muted(icon, 12.0))
                        .on_press(Message::Toggle(row.index))
                        .style(style::action_button)
                        .padding(1);
                    let tip = container(text(label).size(12)).padding([3, 8]).style(style::band);
                    iced::widget::tooltip(toggle, tip, iced::widget::tooltip::Position::Right).into()
                } else {
                    space().width(style::INDENT).into()
                };
                let mut label = text(if title.is_empty() { id } else { title });
                if current {
                    label = label.font(Font { weight: font::Weight::Bold, ..Font::DEFAULT });
                }
                if on_path {
                    label = label.style(style::primary_text);
                }
                let open = (!current).then(|| action("Open", Message::Open(row.index)));
                row![indent, chevron, row_label(label, selected, row.index), open].spacing(4).align_y(Center).into()
            }
            RowKind::ForkPoint { seq, first_line, .. } => {
                let label = match first_line {
                    Some(line) => text(line),
                    None => text(format!("at #{seq}")),
                };
                row![
                    indent,
                    loa_ui_kit::icons::muted(Icon::GitCompare, 12.0),
                    container(label.size(style::TEXT_SIZE).wrapping(Wrapping::None).style(style::muted))
                        .width(Fill)
                        .clip(true),
                ]
                .spacing(6)
                .align_y(Center)
                .into()
            }
            RowKind::Turn { first_line, .. } => row![
                indent,
                row_label(text(first_line), selected, row.index),
                action("Rewind", Message::Rewind(row.index)),
                action("Fork", Message::Fork(row.index)),
            ]
            .spacing(4)
            .align_y(Center)
            .into(),
        };
        container(content).height(style::LIST_ROW_HEIGHT).align_y(Center).into()
    }
}

/// A selectable one line label filling the rest of the row.
fn row_label<'a>(label: text::Text<'a>, selected: bool, index: usize) -> Element<'a, Message> {
    button(container(label.size(style::LIST_TEXT_SIZE).wrapping(Wrapping::None)).width(Fill).clip(true))
        .on_press(Message::Select(index))
        .style(style::row_button(selected))
        .padding([4, 8])
        .width(Fill)
        .into()
}

/// A small action button inside a row.
fn action<'a>(label: &'static str, message: Message) -> Element<'a, Message> {
    button(text(label).size(style::TEXT_SIZE)).on_press(message).style(style::action_button).padding([2, 6]).into()
}

/// Resolved fork links of a family: effective parents, children, roots.
struct Tree {
    roots: Vec<usize>,
    /// Children of each session, sorted by `forked_at` then id.
    children: Vec<Vec<usize>>,
    current: Option<usize>,
    on_path: Vec<bool>,
}

impl Tree {
    fn new(sessions: &[SessionInfo], current: &str) -> Self {
        let mut by_id: HashMap<&str, usize> = HashMap::with_capacity(sessions.len());
        for (index, session) in sessions.iter().enumerate() {
            by_id.entry(&session.id).or_insert(index);
        }
        let declared: Vec<Option<usize>> = sessions
            .iter()
            .map(|session| session.parent.as_deref().and_then(|parent| by_id.get(parent).copied()))
            .collect();
        let parent = break_cycles(&declared);

        let mut roots = Vec::new();
        let mut children = vec![Vec::new(); sessions.len()];
        for (index, parent) in parent.iter().enumerate() {
            match parent {
                Some(parent) => children[*parent].push(index),
                None => roots.push(index),
            }
        }
        for list in &mut children {
            list.sort_by(|&a, &b| {
                let (a, b) = (&sessions[a], &sessions[b]);
                a.forked_at.unwrap_or(0).cmp(&b.forked_at.unwrap_or(0)).then_with(|| a.id.cmp(&b.id))
            });
        }

        let current = by_id.get(current).copied();
        let mut on_path = vec![false; sessions.len()];
        let mut cursor = current;
        while let Some(index) = cursor {
            on_path[index] = true;
            cursor = parent[index];
        }
        Self { roots, children, current, on_path }
    }

    /// Appends the rows of `session` and its subtree.
    fn emit(&self, sessions: &[SessionInfo], session: usize, depth: u16, out: &mut Vec<(u16, Slot)>) {
        out.push((depth, Slot::Session(session)));
        let groups = self.fork_points(sessions, session);
        if self.current == Some(session) {
            let mut pending = groups.iter().peekable();
            while let Some(group) = pending.next_if(|group| group.turn.is_none()) {
                self.emit_group(sessions, session, group, depth + 1, out);
            }
            for turn in 0..sessions[session].turns.len() {
                out.push((depth + 1, Slot::Turn { session, turn }));
                if let Some(group) = pending.next_if(|group| group.turn == Some(turn)) {
                    self.emit_group(sessions, session, group, depth + 1, out);
                }
            }
        } else {
            for group in &groups {
                self.emit_group(sessions, session, group, depth + 1, out);
            }
        }
    }

    fn emit_group(
        &self,
        sessions: &[SessionInfo],
        session: usize,
        group: &ForkGroup<'_>,
        depth: u16,
        out: &mut Vec<(u16, Slot)>,
    ) {
        out.push((depth, Slot::ForkPoint { session, seq: group.seq, turn: group.turn }));
        for &child in group.children {
            self.emit(sessions, child, depth + 1, out);
        }
    }

    /// Children of `session` grouped by the turn they forked from, in fork
    /// order. Children are sorted by `forked_at`, so each group is a run.
    fn fork_points<'a>(&'a self, sessions: &[SessionInfo], session: usize) -> Vec<ForkGroup<'a>> {
        let turns = &sessions[session].turns;
        let children = &self.children[session];
        let point = |child: usize| {
            let forked_at = sessions[child].forked_at.unwrap_or(0);
            match turns.partition_point(|turn| turn.seq <= forked_at) {
                0 => (None, forked_at),
                after => (Some(after - 1), turns[after - 1].seq),
            }
        };
        let mut groups = Vec::new();
        let mut start = 0;
        while start < children.len() {
            let (turn, seq) = point(children[start]);
            let len = children[start..].iter().take_while(|&&child| point(child) == (turn, seq)).count();
            groups.push(ForkGroup { turn, seq, children: &children[start..start + len] });
            start += len;
        }
        groups
    }
}

/// Children forked from the same point of their parent.
struct ForkGroup<'a> {
    turn: Option<usize>,
    seq: u64,
    children: &'a [usize],
}

/// Effective parents: `declared` with every session whose ancestry reaches a
/// cycle turned into a root. Linear in the family size.
fn break_cycles(declared: &[Option<usize>]) -> Vec<Option<usize>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unknown,
        Walking,
        Rooted,
        Cyclic,
    }
    let mut marks = vec![Mark::Unknown; declared.len()];
    let mut chain = Vec::new();
    for start in 0..declared.len() {
        let mut cursor = Some(start);
        let verdict = loop {
            let Some(index) = cursor else { break Mark::Rooted };
            match marks[index] {
                Mark::Unknown => {
                    marks[index] = Mark::Walking;
                    chain.push(index);
                    cursor = declared[index];
                }
                Mark::Walking => break Mark::Cyclic,
                known => break known,
            }
        };
        for index in chain.drain(..) {
            marks[index] = verdict;
        }
    }
    declared
        .iter()
        .zip(&marks)
        .map(|(&parent, &mark)| if mark == Mark::Cyclic { None } else { parent })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(node: u64, seq: u64, first_line: &str) -> TurnInfo {
        TurnInfo { node: NodeId(node), seq, first_line: first_line.to_owned() }
    }

    fn session(id: &str, parent: Option<(&str, u64)>, turns: Vec<TurnInfo>) -> SessionInfo {
        SessionInfo {
            id: id.to_owned(),
            parent: parent.map(|(p, _)| p.to_owned()),
            forked_at: parent.map(|(_, at)| at),
            title: String::new(),
            turns,
        }
    }

    /// Compact rendering of the visible rows: indentation by depth, then
    /// `S id` (with `*` current, `+` on path), `F label`, `T first_line`.
    fn outline(state: &State) -> Vec<String> {
        state
            .rows()
            .map(|row| {
                let pad = "  ".repeat(usize::from(row.depth));
                match row.kind {
                    RowKind::Session { id, current, on_path, .. } => {
                        let mark = if current {
                            "*"
                        } else if on_path {
                            "+"
                        } else {
                            ""
                        };
                        format!("{pad}S {id}{mark}")
                    }
                    RowKind::ForkPoint { seq, first_line, .. } => match first_line {
                        Some(line) => format!("{pad}F {line}"),
                        None => format!("{pad}F at #{seq}"),
                    },
                    RowKind::Turn { first_line, .. } => format!("{pad}T {first_line}"),
                }
            })
            .collect()
    }

    fn index_of(state: &State, wanted: impl Fn(&RowKind<'_>) -> bool) -> usize {
        state.rows().find(|row| wanted(&row.kind)).map(|row| row.index).expect("row present")
    }

    fn turn_row(state: &State, line: &str) -> usize {
        index_of(state, |kind| matches!(kind, RowKind::Turn { first_line, .. } if *first_line == line))
    }

    fn session_row(state: &State, wanted: &str) -> usize {
        index_of(state, |kind| matches!(kind, RowKind::Session { id, .. } if *id == wanted))
    }

    fn root_turns() -> Vec<TurnInfo> {
        vec![turn(10, 1, "one"), turn(20, 5, "two"), turn(30, 9, "three")]
    }

    #[test]
    fn forks_hang_under_the_turn_they_forked_from() {
        let mut state = State::new();
        state.set_family(
            vec![session("b", Some(("root", 9)), vec![]), session("root", None, root_turns()), session(
                "a",
                Some(("root", 6)),
                vec![],
            )],
            "root",
        );
        assert_eq!(outline(&state), [
            "S root*", "  T one", "  T two", "  F two", "    S a", "  T three", "  F three", "    S b",
        ]);
    }

    #[test]
    fn forks_at_the_same_turn_share_one_fork_point() {
        let mut state = State::new();
        state.set_family(
            vec![
                session("root", None, root_turns()),
                session("y", Some(("root", 7)), vec![]),
                session("x", Some(("root", 5)), vec![]),
                session("z", Some(("root", 5)), vec![]),
            ],
            "root",
        );
        assert_eq!(outline(&state), [
            "S root*", "  T one", "  T two", "  F two", "    S x", "    S z", "    S y", "  T three",
        ]);
    }

    #[test]
    fn fork_of_a_fork_nests_deeper_and_current_path_is_marked() {
        let mut state = State::new();
        state.set_family(
            vec![
                session("root", None, root_turns()),
                session("a", Some(("root", 5)), vec![turn(40, 12, "a one"), turn(50, 14, "a two")]),
                session("b", Some(("a", 13)), vec![turn(60, 20, "b one")]),
                session("side", Some(("root", 1)), vec![]),
            ],
            "b",
        );
        assert_eq!(outline(&state), [
            "S root+", "  F one", "    S side", "  F two", "    S a+", "      F a one", "        S b*",
            "          T b one",
        ]);
    }

    #[test]
    fn fork_before_the_first_turn_is_labeled_by_sequence() {
        let mut state = State::new();
        state.set_family(
            vec![session("root", None, vec![turn(10, 4, "one")]), session("a", Some(("root", 2)), vec![])],
            "root",
        );
        assert_eq!(outline(&state), ["S root*", "  F at #2", "    S a", "  T one"]);
    }

    #[test]
    fn unknown_parent_becomes_a_root() {
        let mut state = State::new();
        state.set_family(
            vec![session("root", None, root_turns()), session("orphan", Some(("gone", 3)), vec![])],
            "root",
        );
        assert_eq!(outline(&state), ["S root*", "  T one", "  T two", "  T three", "S orphan"]);
    }

    #[test]
    fn parent_cycle_yields_roots() {
        let mut state = State::new();
        state.set_family(
            vec![
                session("a", Some(("b", 1)), vec![]),
                session("b", Some(("a", 1)), vec![]),
                session("c", Some(("a", 1)), vec![]),
                session("d", Some(("d", 1)), vec![]),
            ],
            "x",
        );
        assert_eq!(outline(&state), ["S a", "S b", "S c", "S d"]);
    }

    #[test]
    fn rewind_and_fork_emit_session_and_turn() {
        let mut state = State::new();
        state.set_family(vec![session("root", None, root_turns())], "root");
        let row = turn_row(&state, "two");
        let (_, event) = state.update(Message::Rewind(row));
        assert_eq!(event, Some(Event::Rewind { session: "root".into(), turn: NodeId(20) }));
        let (_, event) = state.update(Message::Fork(row));
        assert_eq!(event, Some(Event::Fork { session: "root".into(), turn: NodeId(20) }));
    }

    #[test]
    fn fork_on_a_non_turn_row_emits_nothing() {
        let mut state = State::new();
        state.set_family(vec![session("root", None, root_turns()), session("a", Some(("root", 5)), vec![])], "a");
        for row in state.rows().map(|row| row.index).collect::<Vec<_>>() {
            assert_eq!(state.update(Message::Fork(row)).1, None);
            assert_eq!(state.update(Message::Rewind(row)).1, None);
        }
    }

    #[test]
    fn open_emits_session_id_except_for_current() {
        let mut state = State::new();
        state.set_family(vec![session("root", None, root_turns()), session("a", Some(("root", 5)), vec![])], "root");
        let (_, event) = state.update(Message::Open(session_row(&state, "a")));
        assert_eq!(event, Some(Event::Open("a".into())));
        assert_eq!(state.update(Message::Open(session_row(&state, "root"))).1, None);
    }

    #[test]
    fn toggle_hides_and_restores_a_subtree() {
        let mut state = State::new();
        state.set_family(
            vec![
                session("root", None, root_turns()),
                session("a", Some(("root", 5)), vec![turn(40, 12, "a one")]),
                session("b", Some(("a", 12)), vec![]),
                session("other", None, vec![]),
            ],
            "root",
        );
        let a = session_row(&state, "a");
        let _ = state.update(Message::Toggle(a));
        assert_eq!(outline(&state), ["S root*", "  T one", "  T two", "  F two", "    S a", "  T three", "S other"]);
        let _ = state.update(Message::Toggle(a));
        assert!(outline(&state).contains(&"        S b".to_owned()));

        let _ = state.update(Message::Toggle(session_row(&state, "root")));
        assert_eq!(outline(&state), ["S root*", "S other"]);
    }
}
