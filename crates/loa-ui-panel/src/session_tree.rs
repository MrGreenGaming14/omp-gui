//! The session tree tab: a fork family with forks under the parent turn they started after, and current turns.

use crate::{action, indent, list_row_height};
use iced::font::{self, Font};
use iced::widget::text::Wrapping;
use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Center, Element, Fill, Task};
use loa_core::NodeId;
use loa_ui_kit::icons::{Icon, muted};
use loa_ui_kit::style::{self as kit, ButtonKind, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, pad_x, sp, text_size as ts};
use loa_ui_kit::widgets::{icon_button, label};
use std::collections::{HashMap, HashSet};

/// A session of the family: `parent` is `parent_session`, `forked_at` the parent's sequence the fork starts after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo { pub id: String, pub parent: Option<String>, pub forked_at: Option<u64>, pub title: String, pub turns: Vec<TurnInfo> }
/// A turn element, the target of rewind and fork, and the sequence that inserted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnInfo { pub node: NodeId, pub seq: u64, pub first_line: String }
/// One visible row; `index` is stable under collapsing and is what messages carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row<'a> { pub index: usize, pub depth: u16, pub kind: RowKind<'a> }
/// A session; where the children below forked (a turn's first line, or `at #seq`); or a current session's turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind<'a> {
    Session { id: &'a str, title: &'a str, current: bool, on_path: bool, collapsed: bool },
    ForkPoint { session: &'a str, seq: u64, first_line: Option<&'a str> },
    Turn { session: &'a str, node: NodeId, first_line: &'a str },
}

/// Every `usize` is a [`Row::index`].
#[derive(Debug, Clone)]
pub enum Message { Toggle(usize), Rewind(usize), Fork(usize), Open(usize), Select(usize) }
/// `Rewind` rewinds `session` to just before `turn`.
#[derive(Debug, Clone, PartialEq)]
pub enum Event { Rewind { session: String, turn: NodeId }, Fork { session: String, turn: NodeId }, Open(String) }
/// A layout row as indices into the family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot { Session(usize), ForkPoint { session: usize, seq: u64, turn: Option<usize> }, Turn { session: usize, turn: usize } }
/// The family, on-current-path flags, the layout, its visible indices and collapsed ids (kept across [`State::set_family`]).
#[derive(Debug, Default)]
pub struct State {
    sessions: Vec<SessionInfo>, current: Option<usize>, on_path: Vec<bool>, layout: Vec<(u16, Slot)>,
    visible: Vec<usize>, collapsed: HashSet<String>, selected: Option<usize>,
}

impl State {
    pub fn new() -> Self { Self::default() }
    /// Replaces the family; `current` is the session open in the chat.
    pub fn set_family(&mut self, sessions: Vec<SessionInfo>, current: &str) {
        let mut by_id: HashMap<&str, usize> = HashMap::with_capacity(sessions.len());
        sessions.iter().enumerate().for_each(|(i, s)| { by_id.entry(&s.id).or_insert(i); });
        let declared: Vec<_> = sessions.iter().map(|s| s.parent.as_deref().and_then(|p| by_id.get(p).copied())).collect();
        // A session whose ancestry still continues after more steps than there are sessions reaches a cycle.
        let parent: Vec<_> = (0..sessions.len()).map(|i| if (0..=sessions.len()).try_fold(i, |at, _| declared[at]).is_some() { None } else { declared[i] }).collect();
        let mut children = vec![Vec::new(); sessions.len()];
        (0..sessions.len()).for_each(|i| if let Some(p) = parent[i] { children[p].push(i) });
        children.iter_mut().for_each(|l| l.sort_by_key(|&c| (sessions[c].forked_at.unwrap_or(0), &sessions[c].id)));
        let current = by_id.get(current).copied();
        let mut on_path = vec![false; sessions.len()];
        std::iter::successors(current, |&i| parent[i]).for_each(|i| on_path[i] = true);
        let tree = Tree { sessions: &sessions, children, current };
        let mut layout = Vec::with_capacity(sessions.len());
        (0..sessions.len()).filter(|&i| parent[i].is_none()).for_each(|r| tree.emit(r, 0, &mut layout));
        (self.on_path, self.current, self.layout, self.sessions, self.selected) = (on_path, current, layout, sessions, None);
        let sessions = &self.sessions;
        self.collapsed.retain(|id| sessions.iter().any(|s| s.id == *id));
        self.refresh_visible();
    }
    pub fn rows(&self) -> impl Iterator<Item = Row<'_>> + '_ { self.visible.iter().map(|&i| self.row(i)) }
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::Toggle(i) => {
                if let Some(&(_, Slot::Session(s))) = self.layout.get(i) && !self.collapsed.remove(&self.sessions[s].id) { self.collapsed.insert(self.sessions[s].id.clone()); }
                self.refresh_visible();
                None
            }
            Message::Rewind(i) => self.current_turn(i).map(|(session, turn)| Event::Rewind { session, turn }),
            Message::Fork(i) => self.current_turn(i).map(|(session, turn)| Event::Fork { session, turn }),
            Message::Open(i) => match self.layout.get(i) {
                Some(&(_, Slot::Session(s))) if self.current != Some(s) => Some(Event::Open(self.sessions[s].id.clone())),
                _ => None,
            },
            Message::Select(i) => { self.selected = Some(i).filter(|&i| i < self.layout.len()).or(self.selected); None }
        };
        (Task::none(), event)
    }
    pub fn view(&self) -> Element<'_, Message> { scrollable(column(self.rows().map(|r| self.row_view(r))).padding(pad_x(Sp::S))).height(Fill).into() }
    fn current_turn(&self, i: usize) -> Option<(String, NodeId)> {
        let &(_, Slot::Turn { session, turn }) = self.layout.get(i)? else { return None };
        let s = &self.sessions[session];
        (self.current == Some(session)).then(|| (s.id.clone(), s.turns[turn].node))
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
        let s = |i: usize| &self.sessions[i];
        let kind = match slot {
            Slot::Session(i) => RowKind::Session { id: &s(i).id, title: &s(i).title, current: self.current == Some(i), on_path: self.on_path[i], collapsed: self.collapsed.contains(&s(i).id) },
            Slot::ForkPoint { session, seq, turn } => RowKind::ForkPoint { session: &s(session).id, seq, first_line: turn.map(|t| s(session).turns[t].first_line.as_str()) },
            Slot::Turn { session, turn } => RowKind::Turn { session: &s(session).id, node: s(session).turns[turn].node, first_line: &s(session).turns[turn].first_line },
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
                let chevron = match (subtree, collapsed) {
                    (false, _) => space().width(indent()).into(),
                    (true, true) => icon_button(Icon::ChevronRight, "Expand", Some(Message::Toggle(r.index))),
                    (true, false) => icon_button(Icon::ChevronDown, "Collapse", Some(Message::Toggle(r.index))),
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

panel_pane!("session", "Session tree", GitFork);

/// Resolved fork links for laying the family out.
struct Tree<'a> { sessions: &'a [SessionInfo], children: Vec<Vec<usize>>, current: Option<usize> }

impl Tree<'_> {
    /// Appends the rows of `session` and its subtree. The current session lists every turn with
    /// its fork point after it (forks before the first turn first); others only fork points.
    fn emit(&self, session: usize, depth: u16, out: &mut Vec<(u16, Slot)>) {
        out.push((depth, Slot::Session(session)));
        let turns = &self.sessions[session].turns;
        // Children grouped by the turn they forked from (`None` before the first turn), with that turn's sequence.
        let point = |c: usize| {
            let at = self.sessions[c].forked_at.unwrap_or(0);
            match turns.partition_point(|t| t.seq <= at) {
                0 => (None, at),
                after => (Some(after - 1), turns[after - 1].seq),
            }
        };
        let mut groups = self.children[session].chunk_by(|&a, &b| point(a) == point(b)).map(|run| (point(run[0]), run)).peekable();
        let group = |((turn, seq), run): ((Option<usize>, u64), &[usize]), out: &mut Vec<(u16, Slot)>| {
            out.push((depth + 1, Slot::ForkPoint { session, seq, turn }));
            run.iter().for_each(|&c| self.emit(c, depth + 2, out));
        };
        if self.current != Some(session) { return groups.for_each(|g| group(g, out)) }
        while let Some(g) = groups.next_if(|g| g.0.0.is_none()) { group(g, out) }
        for turn in 0..turns.len() {
            out.push((depth + 1, Slot::Turn { session, turn }));
            if let Some(g) = groups.next_if(|g| g.0.0 == Some(turn)) { group(g, out) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn turns(list: &[(u64, u64, &str)]) -> Vec<TurnInfo> { list.iter().map(|&(n, seq, l)| TurnInfo { node: NodeId(n), seq, first_line: l.into() }).collect() }
    fn session(id: &str, parent: Option<(&str, u64)>, turns: Vec<TurnInfo>) -> SessionInfo { SessionInfo { id: id.into(), parent: parent.map(|(p, _)| p.into()), forked_at: parent.map(|(_, at)| at), title: String::new(), turns } }
    fn root() -> SessionInfo { session("root", None, turns(&[(10, 1, "one"), (20, 5, "two"), (30, 9, "three")])) }
    /// Rows indented by depth: `S id` (`*` current, `+` on path), `F label`, `T line`.
    fn outline(s: &State) -> Vec<String> {
        let body = |k| match k {
            RowKind::Session { id, current, on_path, .. } => format!("S {id}{}", if current { "*" } else if on_path { "+" } else { "" }),
            RowKind::ForkPoint { seq, first_line, .. } => format!("F {}", first_line.map_or_else(|| format!("at #{seq}"), str::to_owned)),
            RowKind::Turn { first_line, .. } => format!("T {first_line}"),
        };
        s.rows().map(|r| format!("{}{}", "  ".repeat(usize::from(r.depth)), body(r.kind))).collect()
    }
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
        let mut s = State::new();
        s.set_family(vec![root(), session("orphan", Some(("gone", 3)), vec![])], "root");
        assert_eq!(s.update(Message::Rewind(2)).1, Some(Event::Rewind { session: "root".into(), turn: NodeId(20) }));
        let _ = s.update(Message::Toggle(0));
        assert_eq!(outline(&s), ["S root*", "S orphan"]);
    }
}
