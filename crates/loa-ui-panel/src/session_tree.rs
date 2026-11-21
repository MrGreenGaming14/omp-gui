//! The session tree tab: the fork family of a session, each session once with children under
//! the parent turn they forked from, and the current session's turns as rewind and fork
//! targets. Roots are sessions without a parent in the family (a cycling ancestry is a root);
//! children sort by `forked_at` then id and attach to the parent's last turn with
//! `seq <= forked_at`, children sharing that turn sharing one fork point row.

use crate::{action, indent, list_row_height};
use iced::font::{self, Font};
use iced::widget::text::Wrapping;
use iced::widget::{button, column, container, row, scrollable, space, text, tooltip};
use iced::{Center, Element, Fill, Padding, Task};
use loa_core::NodeId;
use loa_ui_kit::icons::{Icon, muted};
use loa_ui_kit::style::{self as kit, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, sp, text_size as ts};
use loa_ui_kit::widgets::{PanelPane, label};
use std::collections::{HashMap, HashSet};

/// One session of the family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    /// `parent_session` on `<session>`.
    pub parent: Option<String>,
    /// The parent's journal sequence the fork starts after (`forked_at`).
    pub forked_at: Option<u64>,
    /// Shown instead of the id when not empty.
    pub title: String,
    /// In journal order; needed for the current session and sessions with forks.
    pub turns: Vec<TurnInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnInfo {
    /// The turn element; the target of rewind and fork.
    pub node: NodeId,
    /// The sequence that inserted the turn, ascending along the turns.
    pub seq: u64,
    pub first_line: String,
}

/// One visible row; `index` is stable under collapsing and is what messages carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row<'a> {
    pub index: usize,
    pub depth: u16,
    pub kind: RowKind<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind<'a> {
    Session { id: &'a str, title: &'a str, current: bool, on_path: bool, collapsed: bool },
    /// Where the children below forked: the turn's first line, or `at #seq` before any turn.
    ForkPoint { session: &'a str, seq: u64, first_line: Option<&'a str> },
    /// A turn of the current session.
    Turn { session: &'a str, node: NodeId, first_line: &'a str },
}

/// Every `usize` is a [`Row::index`].
#[derive(Debug, Clone)]
pub enum Message {
    Toggle(usize),
    Rewind(usize),
    Fork(usize),
    Open(usize),
    Select(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Rewind `session` to just before `turn`.
    Rewind { session: String, turn: NodeId },
    Fork { session: String, turn: NodeId },
    Open(String),
}

/// A layout row as indices into the family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Session(usize),
    ForkPoint { session: usize, seq: u64, turn: Option<usize> },
    Turn { session: usize, turn: usize },
}

#[derive(Debug, Default)]
pub struct State {
    sessions: Vec<SessionInfo>,
    current: Option<usize>,
    /// Per session: the current one or an ancestor of it.
    on_path: Vec<bool>,
    layout: Vec<(u16, Slot)>,
    /// Layout indices not hidden by a collapsed session.
    visible: Vec<usize>,
    /// Kept across [`State::set_family`].
    collapsed: HashSet<String>,
    selected: Option<usize>,
}

impl State {
    pub fn new() -> Self { Self::default() }

    /// Replaces the family; `current` is the session open in the chat.
    pub fn set_family(&mut self, sessions: Vec<SessionInfo>, current: &str) {
        let tree = Tree::new(&sessions, current);
        let mut layout = Vec::with_capacity(sessions.len());
        tree.roots.iter().for_each(|&r| tree.emit(&sessions, r, 0, &mut layout));
        (self.on_path, self.current, self.layout, self.sessions, self.selected) = (tree.on_path, tree.current, layout, sessions, None);
        let sessions = &self.sessions;
        self.collapsed.retain(|id| sessions.iter().any(|s| s.id == *id));
        self.refresh_visible();
    }

    pub fn rows(&self) -> impl Iterator<Item = Row<'_>> + '_ { self.visible.iter().map(|&i| self.row(i)) }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::Toggle(i) => {
                if let Some(&(_, Slot::Session(s))) = self.layout.get(i) {
                    let id = &self.sessions[s].id;
                    if !self.collapsed.remove(id) { self.collapsed.insert(id.clone()); }
                    self.refresh_visible();
                }
                None
            }
            Message::Rewind(i) => self.current_turn(i).map(|(session, turn)| Event::Rewind { session, turn }),
            Message::Fork(i) => self.current_turn(i).map(|(session, turn)| Event::Fork { session, turn }),
            Message::Open(i) => match self.layout.get(i) {
                Some(&(_, Slot::Session(s))) if self.current != Some(s) => Some(Event::Open(self.sessions[s].id.clone())),
                _ => None,
            },
            Message::Select(i) => {
                if i < self.layout.len() { self.selected = Some(i) }
                None
            }
        };
        (Task::none(), event)
    }

    pub fn view(&self) -> Element<'_, Message> { scrollable(column(self.rows().map(|r| self.row_view(r))).padding(Padding::ZERO.left(sp(Sp::S)).right(sp(Sp::S)))).height(Fill).into() }

    fn current_turn(&self, i: usize) -> Option<(String, NodeId)> {
        match self.layout.get(i) {
            Some(&(_, Slot::Turn { session, turn })) if self.current == Some(session) => Some((self.sessions[session].id.clone(), self.sessions[session].turns[turn].node)),
            _ => None,
        }
    }

    fn refresh_visible(&mut self) {
        self.visible.clear();
        let mut hidden_below: Option<u16> = None;
        for (i, &(depth, slot)) in self.layout.iter().enumerate() {
            if hidden_below.is_some_and(|limit| depth > limit) { continue }
            self.visible.push(i);
            hidden_below = matches!(slot, Slot::Session(s) if self.collapsed.contains(&self.sessions[s].id)).then_some(depth);
        }
    }

    fn row(&self, index: usize) -> Row<'_> {
        let (depth, slot) = self.layout[index];
        let kind = match slot {
            Slot::Session(s) => {
                let i = &self.sessions[s];
                RowKind::Session { id: &i.id, title: &i.title, current: self.current == Some(s), on_path: self.on_path[s], collapsed: self.collapsed.contains(&i.id) }
            }
            Slot::ForkPoint { session, seq, turn } => RowKind::ForkPoint { session: &self.sessions[session].id, seq, first_line: turn.map(|t| self.sessions[session].turns[t].first_line.as_str()) },
            Slot::Turn { session, turn } => {
                let (info, t) = (&self.sessions[session], &self.sessions[session].turns[turn]);
                RowKind::Turn { session: &info.id, node: t.node, first_line: &t.first_line }
            }
        };
        Row { index, depth, kind }
    }

    fn row_view<'a>(&'a self, r: Row<'a>) -> Element<'a, Message> {
        let lead = space().width(indent() * f32::from(r.depth));
        let selected = self.selected == Some(r.index);
        let pick = |label: text::Text<'a>| button(container(label.size(ts(T::Body)).wrapping(Wrapping::None)).width(Fill).clip(true)).on_press(Message::Select(r.index)).style(kit::button(ButtonKind::Row { selected })).padding([sp(Sp::Xs), sp(Sp::S)]).width(Fill);
        let content: Element<'a, Message> = match r.kind {
            RowKind::Session { id, title, current, on_path, collapsed } => {
                // The layout row after a session is deeper exactly when it has a subtree.
                let subtree = self.layout.get(r.index + 1).is_some_and(|&(d, _)| d > r.depth);
                let chevron: Element<'a, Message> = if subtree {
                    let (icon, tip) = if collapsed { (Icon::ChevronRight, "Expand") } else { (Icon::ChevronDown, "Collapse") };
                    let toggle = button(muted(icon, ts(T::Caption))).on_press(Message::Toggle(r.index)).style(kit::button(ButtonKind::Ghost)).padding(sp(Sp::Xs));
                    tooltip(toggle, container(label(tip, T::Caption, Tone::Normal)).padding([sp(Sp::Xs), sp(Sp::S)]).style(kit::container(Surface::Tooltip)), tooltip::Position::Right).into()
                } else {
                    space().width(indent()).into()
                };
                let mut name = text(if title.is_empty() { id } else { title });
                if current { name = name.font(Font { weight: font::Weight::Bold, ..Font::DEFAULT }) }
                if on_path { name = name.style(kit::text(Tone::Accent)) }
                row![lead, chevron, pick(name), (!current).then(|| action("Open", Message::Open(r.index)))].spacing(sp(Sp::Xs)).align_y(Center).into()
            }
            RowKind::ForkPoint { seq, first_line, .. } => {
                let text = first_line.map_or_else(|| format!("at #{seq}"), str::to_owned);
                row![lead, muted(Icon::GitCompare, ts(T::Caption)), container(label(text, T::Label, Tone::Muted).wrapping(Wrapping::None)).width(Fill).clip(true)].spacing(sp(Sp::S)).align_y(Center).into()
            }
            RowKind::Turn { first_line, .. } => row![lead, pick(text(first_line)), action("Rewind", Message::Rewind(r.index)), action("Fork", Message::Fork(r.index))].spacing(sp(Sp::Xs)).align_y(Center).into(),
        };
        container(content).height(list_row_height()).align_y(Center).into()
    }
}

impl PanelPane for State {
    type Message = Message;
    fn id(&self) -> &str { "session" }
    fn title(&self) -> &str { "Session tree" }
    fn icon(&self) -> Icon { Icon::GitFork }
    fn view(&self) -> Element<'_, Message> { State::view(self) }
}

/// Resolved fork links: roots, children sorted by `forked_at` then id, and the current path.
struct Tree {
    roots: Vec<usize>,
    children: Vec<Vec<usize>>,
    current: Option<usize>,
    on_path: Vec<bool>,
}

impl Tree {
    fn new(sessions: &[SessionInfo], current: &str) -> Self {
        let mut by_id: HashMap<&str, usize> = HashMap::with_capacity(sessions.len());
        sessions.iter().enumerate().for_each(|(i, s)| { by_id.entry(&s.id).or_insert(i); });
        let parent = break_cycles(&sessions.iter().map(|s| s.parent.as_deref().and_then(|p| by_id.get(p).copied())).collect::<Vec<_>>());
        let (mut roots, mut children) = (Vec::new(), vec![Vec::new(); sessions.len()]);
        for (i, p) in parent.iter().enumerate() {
            match p {
                Some(p) => children[*p].push(i),
                None => roots.push(i),
            }
        }
        children.iter_mut().for_each(|l| l.sort_by_key(|&c| (sessions[c].forked_at.unwrap_or(0), &sessions[c].id)));
        let current = by_id.get(current).copied();
        let mut on_path = vec![false; sessions.len()];
        let mut cursor = current;
        while let Some(i) = cursor {
            on_path[i] = true;
            cursor = parent[i];
        }
        Self { roots, children, current, on_path }
    }

    /// Appends the rows of `session` and its subtree. The current session lists every turn
    /// with the fork point hanging off it (forks before the first turn first); others list
    /// only their fork points.
    fn emit(&self, sessions: &[SessionInfo], session: usize, depth: u16, out: &mut Vec<(u16, Slot)>) {
        out.push((depth, Slot::Session(session)));
        let groups = self.fork_points(sessions, session);
        let group = |g: &(Option<usize>, u64, &[usize]), out: &mut Vec<(u16, Slot)>| {
            out.push((depth + 1, Slot::ForkPoint { session, seq: g.1, turn: g.0 }));
            g.2.iter().for_each(|&c| self.emit(sessions, c, depth + 2, out));
        };
        if self.current != Some(session) { return groups.iter().for_each(|g| group(g, out)) }
        let mut pending = groups.iter().peekable();
        while let Some(g) = pending.next_if(|g| g.0.is_none()) { group(g, out) }
        for turn in 0..sessions[session].turns.len() {
            out.push((depth + 1, Slot::Turn { session, turn }));
            if let Some(g) = pending.next_if(|g| g.0 == Some(turn)) { group(g, out) }
        }
    }

    /// Children grouped by the turn they forked from (`None` before the first turn), with
    /// that turn's sequence, in fork order.
    fn fork_points<'a>(&'a self, sessions: &[SessionInfo], session: usize) -> Vec<(Option<usize>, u64, &'a [usize])> {
        let (turns, children) = (&sessions[session].turns, &self.children[session]);
        let point = |c: usize| {
            let at = sessions[c].forked_at.unwrap_or(0);
            match turns.partition_point(|t| t.seq <= at) {
                0 => (None, at),
                after => (Some(after - 1), turns[after - 1].seq),
            }
        };
        children.chunk_by(|&a, &b| point(a) == point(b)).map(|run| { let (t, s) = point(run[0]); (t, s, run) }).collect()
    }
}

/// `declared` parents with every session whose ancestry reaches a cycle made a root.
fn break_cycles(declared: &[Option<usize>]) -> Vec<Option<usize>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unknown,
        Walking,
        Rooted,
        Cyclic,
    }
    let (mut marks, mut chain) = (vec![Mark::Unknown; declared.len()], Vec::new());
    for start in 0..declared.len() {
        let mut cursor = Some(start);
        let verdict = loop {
            let Some(i) = cursor else { break Mark::Rooted };
            match marks[i] {
                Mark::Unknown => {
                    marks[i] = Mark::Walking;
                    chain.push(i);
                    cursor = declared[i];
                }
                Mark::Walking => break Mark::Cyclic,
                known => break known,
            }
        };
        chain.drain(..).for_each(|i| marks[i] = verdict);
    }
    declared.iter().zip(&marks).map(|(&p, &m)| if m == Mark::Cyclic { None } else { p }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turns(list: &[(u64, u64, &str)]) -> Vec<TurnInfo> { list.iter().map(|&(n, seq, l)| TurnInfo { node: NodeId(n), seq, first_line: l.into() }).collect() }

    fn session(id: &str, parent: Option<(&str, u64)>, turns: Vec<TurnInfo>) -> SessionInfo {
        SessionInfo { id: id.into(), parent: parent.map(|(p, _)| p.into()), forked_at: parent.map(|(_, at)| at), title: String::new(), turns }
    }

    fn root() -> SessionInfo { session("root", None, turns(&[(10, 1, "one"), (20, 5, "two"), (30, 9, "three")])) }

    /// Rows indented by depth: `S id` (`*` current, `+` on path), `F label`, `T line`.
    fn outline(s: &State) -> Vec<String> {
        s.rows().map(|r| {
            let body = match r.kind {
                RowKind::Session { id, current, on_path, .. } => format!("S {id}{}", if current { "*" } else if on_path { "+" } else { "" }),
                RowKind::ForkPoint { seq, first_line, .. } => format!("F {}", first_line.map_or_else(|| format!("at #{seq}"), str::to_owned)),
                RowKind::Turn { first_line, .. } => format!("T {first_line}"),
            };
            format!("{}{body}", "  ".repeat(usize::from(r.depth)))
        }).collect()
    }

    fn index(s: &State, want: &str) -> usize { s.rows().zip(outline(s)).find(|(_, o)| o.trim_start().trim_end_matches(['*', '+']) == want).unwrap().0.index }

    #[test]
    fn forks_hang_under_their_turns_and_cycles_become_roots() {
        let cases: [(Vec<SessionInfo>, &str, &[&str]); 5] = [
            (vec![session("b", Some(("root", 9)), vec![]), root(), session("a", Some(("root", 6)), vec![]), session("x", Some(("root", 5)), vec![]), session("z", Some(("root", 5)), vec![])], "root",
                &["S root*", "  T one", "  T two", "  F two", "    S x", "    S z", "    S a", "  T three", "  F three", "    S b"]),
            (vec![root(), session("a", Some(("root", 5)), turns(&[(40, 12, "a one"), (50, 14, "a two")])), session("b", Some(("a", 13)), turns(&[(60, 20, "b one")])), session("side", Some(("root", 1)), vec![])], "b",
                &["S root+", "  F one", "    S side", "  F two", "    S a+", "      F a one", "        S b*", "          T b one"]),
            (vec![session("root", None, turns(&[(10, 4, "one")])), session("a", Some(("root", 2)), vec![])], "root", &["S root*", "  F at #2", "    S a", "  T one"]),
            (vec![root(), session("orphan", Some(("gone", 3)), vec![])], "root", &["S root*", "  T one", "  T two", "  T three", "S orphan"]),
            (vec![session("a", Some(("b", 1)), vec![]), session("b", Some(("a", 1)), vec![]), session("c", Some(("a", 1)), vec![]), session("d", Some(("d", 1)), vec![])], "x", &["S a", "S b", "S c", "S d"]),
        ];
        for (family, current, want) in cases {
            let mut s = State::new();
            s.set_family(family, current);
            assert_eq!(outline(&s), want);
        }
    }

    #[test]
    fn turn_rows_rewind_and_fork_sessions_open_and_toggle() {
        let mut s = State::new();
        s.set_family(vec![root(), session("a", Some(("root", 5)), turns(&[(40, 12, "a one")])), session("b", Some(("a", 12)), vec![]), session("other", None, vec![])], "root");
        let two = index(&s, "T two");
        assert_eq!(s.update(Message::Rewind(two)).1, Some(Event::Rewind { session: "root".into(), turn: NodeId(20) }));
        assert_eq!(s.update(Message::Fork(two)).1, Some(Event::Fork { session: "root".into(), turn: NodeId(20) }));
        assert_eq!((s.update(Message::Fork(index(&s, "S a"))).1, s.update(Message::Open(index(&s, "S root"))).1), (None, None));
        assert_eq!(s.update(Message::Open(index(&s, "S a"))).1, Some(Event::Open("a".into())));
        let a = index(&s, "S a");
        let _ = s.update(Message::Toggle(a));
        assert_eq!(outline(&s), ["S root*", "  T one", "  T two", "  F two", "    S a", "  T three", "S other"]);
        let _ = s.update(Message::Toggle(a));
        let _ = s.update(Message::Toggle(index(&s, "S root")));
        assert_eq!(outline(&s), ["S root*", "S other"]);
        drop(iced_test::simulator(s.view()));
    }
}
