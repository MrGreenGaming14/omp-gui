//! Chat transcript rendered from the session DOM. [`State`] is fed every committed patch and
//! keeps a cache per row: assistant markdown is parsed the first time its row is drawn, then
//! incrementally, so a token append touches one row. The view is a keyed virtual list: only rows
//! around the viewport are built, sized by measured heights where known and estimates elsewhere,
//! and each row keeps its widget state while the window slides. It follows new content only
//! while scrolled to the bottom; opening another session follows its newest rows.

mod items;
mod prose;
mod rows;
mod virtual_list;

pub use items::{COMMANDS_EXT, Item, ItemKind};
pub use rows::{RowCtx, RowRenderer, renderer};

use iced::widget::{self, button, container, markdown, scrollable, sensor, space, stack, text};
use iced::{Element, Fill, Task, Theme};
use items::Emitter;
use loa_core::{Dom, ElementKind, NodeId, Op, Patch, attr, schema::TEXT};
use loa_ui_kit::style::{self as kit, ButtonKind};
use loa_ui_kit::theme::{LayoutRole, SpaceRole as Sp, TextRole as T, for_theme, generation, sp, text_size as ts, tokens, width};
use loa_ui_kit::widgets::Heights;
use loa_view::session::transcript::Intent;
use prose::Streamed;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use virtual_list::VirtualList;

/// Pixels of rows built above and below the viewport.
const OVERSCAN: f32 = 600.0;
/// Viewport height assumed until the scrollable reports one.
const DEFAULT_VIEWPORT: f32 = 1000.0;
/// Upper bound on rows built per frame, whatever their heights.
pub(crate) const MAX_WINDOW: usize = 200;
/// Distance from the end that still counts as "at the bottom".
const BOTTOM_SLACK: f32 = 8.0;
/// Measurement changes smaller than this are ignored.
const HEIGHT_EPSILON: f32 = 0.5;
pub(crate) const LINE: f32 = 22.0;
pub(crate) const ROW: f32 = 30.0;
pub(crate) const PADDING: f32 = 12.0;

/// Where a decision card goes: the app renders the card for `node` in its place.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Slot {
    pub kind: ItemKind,
    pub node: NodeId,
}

/// The scroll position as the scrollable reports it, in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollMetrics {
    /// Distance from the top of the content to the top of the viewport.
    pub offset: f32,
    pub viewport: f32,
    pub content: f32,
}

impl From<scrollable::Viewport> for ScrollMetrics {
    fn from(v: scrollable::Viewport) -> Self { Self { offset: v.absolute_offset().y, viewport: v.bounds().height, content: v.content_bounds().height } }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Scrolled(ScrollMetrics),
    /// A row was laid out at `height`.
    Measured { key: NodeId, height: f32 },
    /// Expand or collapse the row starting at this node.
    Toggle(NodeId),
    /// A user message was clicked.
    Rewind(NodeId),
    Link(markdown::Uri),
    JumpToLatest,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Scroll {
    pub metrics: ScrollMetrics,
    /// The scrollable has reported a viewport at least once.
    known: bool,
    /// Keep the newest content in view.
    follow: bool,
}

/// The rows of one child of `<session>`, which are always contiguous.
#[derive(Clone, Copy, Debug)]
struct Span {
    first: NodeId,
    len: usize,
}

/// The transcript component.
pub struct State {
    session: Option<NodeId>,
    pub(crate) items: Vec<Item>,
    pub(crate) heights: Heights,
    /// Row index by row key.
    pub(crate) pos: HashMap<NodeId, usize>,
    /// Row key of every node a row shows. Entries of removed nodes are inert: ids never repeat.
    owner: HashMap<NodeId, NodeId>,
    spans: HashMap<NodeId, Span>,
    measured: HashMap<NodeId, f32>,
    /// Parsed markdown of assistant rows, made the first time a row is drawn.
    pub(crate) streamed: HashMap<NodeId, OnceCell<Streamed>>,
    expanded: HashSet<NodeId>,
    pub(crate) scroll: Scroll,
    /// `id` of the session the rows show; loading another one starts it following at the bottom.
    shown: Option<String>,
    look: std::cell::Cell<prose::Look>,
    scroll_id: widget::Id,
    /// Room under the last row for what floats over the bottom edge (the composer).
    bottom_inset: f32,
}

impl State {
    pub fn new(theme: &Theme) -> Self {
        Self {
            session: None,
            items: Vec::new(),
            heights: Heights::default(),
            pos: HashMap::new(),
            owner: HashMap::new(),
            spans: HashMap::new(),
            measured: HashMap::new(),
            streamed: HashMap::new(),
            expanded: HashSet::new(),
            scroll: Scroll { metrics: ScrollMetrics::default(), known: false, follow: true },
            shown: None,
            look: prose::Look::new(&for_theme(theme)).into(),
            scroll_id: widget::Id::unique(),
            bottom_inset: 0.0,
        }
    }

    pub fn from_dom(dom: &Dom, theme: &Theme) -> Self {
        let mut state = Self::new(theme);
        state.load(dom);
        state
    }

    pub fn set_bottom_inset(&mut self, inset: f32) { self.bottom_inset = inset }
    pub fn set_theme(&mut self, theme: &Theme) { self.look.set(prose::Look::new(&for_theme(theme))) }

    /// The look of the active token generation, rebuilt after a theme change.
    pub(crate) fn look(&self) -> prose::Look {
        let look = self.look.get();
        if look.generation == generation() { return look }
        let look = prose::Look::new(&tokens());
        self.look.set(look);
        look
    }

    pub fn scroll_id(&self) -> &widget::Id { &self.scroll_id }
    /// True while the view follows new content.
    pub fn is_following(&self) -> bool { self.scroll.follow }

    /// Rebuilds every row from `dom`. The scroll state is kept for the same session; another
    /// session starts following its newest rows.
    pub fn load(&mut self, dom: &Dom) {
        self.items.clear();
        self.heights = Heights::default();
        self.pos.clear();
        self.owner.clear();
        self.spans.clear();
        self.measured.clear();
        self.streamed.clear();
        self.expanded.clear();
        let root = dom.root().filter(|r| r.kind == ElementKind::Session);
        self.session = root.map(|r| r.id);
        let shown = root.and_then(|r| r.str_attr(attr::ID));
        if shown != self.shown.as_deref() {
            self.shown = shown.map(str::to_owned);
            // The old offset belongs to the other session: forget it, so the shorter content it
            // reports next doesn't read as a scroll up.
            (self.scroll.follow, self.scroll.known) = (true, false);
        }
        let Some(session) = self.session else { return };
        let tops: Vec<NodeId> = dom.children(session).map(|n| n.id).collect();
        for top in tops {
            let at = self.items.len();
            self.rebuild_top(dom, top, at..at);
        }
    }

    /// Updates the rows after `patch` was applied; `dom` is the state after it.
    pub fn apply(&mut self, patch: &Patch, dom: &Dom) {
        match &patch.op {
            Op::Insert { kind: ElementKind::Session, .. } => self.load(dom),
            Op::Insert { .. } => self.inserted(dom, patch.node),
            Op::Set { name, .. } => self.set(dom, patch.node, name),
            Op::Append { text } => {
                // A row not drawn yet parses the whole text when it is.
                if let (Some(streamed), Some(node)) = (self.streamed.get_mut(&patch.node).and_then(OnceCell::get_mut), dom.get(patch.node)) {
                    streamed.append(&node.text, text);
                }
                self.reestimate(dom, patch.node);
            }
            Op::Remove if Some(patch.node) == self.session => self.load(dom),
            Op::Remove => {
                let top = if self.spans.contains_key(&patch.node) { Some(patch.node) } else { self.owner.get(&patch.node).and_then(|k| self.pos.get(k)).map(|i| self.items[*i].top) };
                if let Some(top) = top {
                    self.restructure(dom, top);
                }
            }
        }
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Intent>) {
        match message {
            Message::Scrolled(metrics) => (self.scrolled(metrics), None),
            Message::Measured { key, height } => (self.measure(key, height), None),
            Message::Toggle(key) => {
                if !self.expanded.remove(&key) {
                    self.expanded.insert(key);
                }
                (Task::none(), None)
            }
            Message::Rewind(node) => (Task::none(), Some(Intent::Rewind(node))),
            Message::Link(uri) => (Task::none(), Some(Intent::OpenLink(uri))),
            Message::JumpToLatest => {
                self.scroll.follow = true;
                (widget::operation::snap_to_end(self.scroll_id.clone()), None)
            }
        }
    }

    /// The context of `item`'s row; `parse` fills an assistant row's markdown cache, as drawing
    /// does, while estimates use what is parsed already.
    fn cx<'a>(&'a self, dom: &'a Dom, item: &Item, parse: bool) -> Option<RowCtx<'a>> {
        let node = dom.get(item.key)?;
        let cell = self.streamed.get(&item.key);
        let streamed = if parse { cell.map(|c| c.get_or_init(|| Streamed::new(&node.text))) } else { cell.and_then(OnceCell::get) };
        Some(RowCtx { dom, node, expanded: self.expanded.contains(&item.key), streamed, look: self.look() })
    }

    /// The transcript: rows built only around the viewport. `map` lifts messages into the
    /// app's and `slot` renders the decision card of a [`Slot`].
    pub fn view<'a, M: 'a>(&'a self, dom: &'a Dom, map: impl Fn(Message) -> M + Clone + 'a, slot: impl Fn(Slot) -> Element<'a, M>) -> Element<'a, M> {
        let window = self.window();
        let above = self.heights.prefix(window.start) as f32;
        let below = (self.heights.total() - self.heights.prefix(window.end)) as f32;
        let (keys, children) = self.items[window]
            .iter()
            .map(|item| {
                let key = item.key;
                let content: Element<'a, M> = match self.cx(dom, item, item.kind != ItemKind::Decision) {
                    _ if item.kind == ItemKind::Decision => slot(Slot { kind: item.kind, node: key }),
                    Some(cx) => renderer(item.kind).view(item, &cx).map(map.clone()),
                    None => space().into(),
                };
                let (on_show, on_resize) = (map.clone(), map.clone());
                let row = container(container(content).width(Fill).max_width(width(LayoutRole::ComposerWidth))).center_x(Fill).padding([PADDING / 2.0, sp(Sp::Gutter)]);
                let row = sensor(row)
                    .key(key.0)
                    .anticipate(OVERSCAN)
                    .on_show(move |size| on_show(Message::Measured { key, height: size.height }))
                    .on_resize(move |size| on_resize(Message::Measured { key, height: size.height }));
                (key.0, Element::from(row))
            })
            .unzip();
        // Keyed by row, so each row keeps its widget state while the window slides; the rows
        // outside the window are padding, not spacers.
        let list = VirtualList { top: above, bottom: below + self.bottom_inset, keys, children };
        let on_scroll = map.clone();
        let bar = scrollable::Scrollbar::new().width(6).scroller_width(6);
        let transcript = scrollable(list)
            .id(self.scroll_id.clone())
            .direction(scrollable::Direction::Vertical(bar))
            .width(Fill)
            .height(Fill)
            .on_scroll(move |viewport| on_scroll(Message::Scrolled(viewport.into())));
        // The root stays a stack whether or not the button shows, so the scrollable keeps its
        // state (and its offset) when following stops.
        let root = stack![transcript];
        if self.scroll.follow {
            return root.into();
        }
        let jump = button(text("Jump to latest ↓").size(ts(T::Label))).style(kit::button(ButtonKind::Primary)).padding([sp(Sp::S), sp(Sp::L)]).on_press(Message::JumpToLatest);
        let pad = iced::Padding { bottom: sp(Sp::L) + self.bottom_inset, ..iced::Padding::new(sp(Sp::L)) };
        root.push(container(Element::from(jump).map(map)).padding(pad).center_x(Fill).align_bottom(Fill)).into()
    }

    /// Rows intersecting the viewport plus [`OVERSCAN`] on each side, at most [`MAX_WINDOW`].
    pub(crate) fn window(&self) -> Range<usize> {
        let n = self.items.len();
        let viewport = Some(self.scroll.metrics.viewport).filter(|v| self.scroll.known && *v > 0.0).unwrap_or(DEFAULT_VIEWPORT) as f64;
        let bottom = (self.heights.total() - viewport).max(0.0);
        let top = if self.scroll.follow { bottom } else { f64::from(self.scroll.metrics.offset).clamp(0.0, bottom) };
        let overscan = f64::from(OVERSCAN);
        let (Some(first), Some(last)) = (self.heights.find((top - overscan).max(0.0)), self.heights.find(top + viewport + overscan)) else { return 0..0 };
        if self.scroll.follow && last + 1 == n { n - (n - first).min(MAX_WINDOW)..n } else { first..(last + 1).min(first + MAX_WINDOW) }
    }

    fn scrolled(&mut self, metrics: ScrollMetrics) -> Task<Message> {
        let at_bottom = metrics.offset + metrics.viewport >= metrics.content - BOTTOM_SLACK;
        if at_bottom {
            self.scroll.follow = true;
        } else if self.scroll.known && metrics.offset + HEIGHT_EPSILON < self.scroll.metrics.offset {
            // The user scrolled up; content growing under a still viewport keeps following.
            self.scroll.follow = false;
        }
        (self.scroll.metrics, self.scroll.known) = (metrics, true);
        if self.scroll.follow && !at_bottom { widget::operation::snap_to_end(self.scroll_id.clone()) } else { Task::none() }
    }

    fn measure(&mut self, key: NodeId, height: f32) -> Task<Message> {
        let Some((index, old)) = self.pos.get(&key).and_then(|&i| Some((i, self.heights.get(i)?))) else { return Task::none() };
        self.measured.insert(key, height);
        let delta = height - old;
        if delta.abs() < HEIGHT_EPSILON {
            return Task::none();
        }
        let above_viewport = self.heights.prefix(index + 1) <= f64::from(self.scroll.metrics.offset);
        self.heights.set(index, height);
        if self.scroll.follow || !above_viewport {
            return Task::none();
        }
        // A row above the viewport changed size: shift the offset so the reading position stays.
        self.scroll.metrics.offset += delta;
        self.scroll.metrics.content += delta;
        widget::operation::scroll_by(self.scroll_id.clone(), widget::operation::AbsoluteOffset { x: 0.0, y: delta })
    }

    fn inserted(&mut self, dom: &Dom, id: NodeId) {
        let Some(top) = items::top_of(dom, id) else { return };
        if self.appends_at_end(dom, id, top) { self.append_rows(dom, id, top) } else { self.restructure(dom, top) }
    }

    /// True when the rows of `id` go after every row: `id` is last within `top`, and no row
    /// follows the rows of `top`.
    fn appends_at_end(&self, dom: &Dom, id: NodeId, top: NodeId) -> bool {
        let mut child = id;
        while child != top {
            let Some(parent) = dom.get(child).and_then(|n| dom.get(n.parent)) else { return false };
            if parent.children.last() != Some(&child) {
                return false;
            }
            child = parent.id;
        }
        match self.items.last() {
            None => true,
            Some(last) if last.top == top => true,
            Some(_) if self.spans.contains_key(&top) => false,
            Some(_) => {
                let Some(siblings) = self.session.and_then(|s| dom.get(s)).map(|s| &s.children) else { return false };
                siblings.iter().rev().take_while(|s| **s != top).all(|s| !self.spans.contains_key(s))
            }
        }
    }

    fn emit(&mut self, dom: &Dom, top: NodeId, node: NodeId, out: &mut Vec<Item>) {
        let owner = &mut self.owner;
        if let Some(node) = dom.get(node) {
            Emitter { dom, top, own: |node, key| { owner.insert(node, key); } }.emit(out, node);
        }
    }

    /// Appends the rows of the subtree at `id`, after every row; a call may join the last group.
    fn append_rows(&mut self, dom: &Dom, id: NodeId, top: NodeId) {
        let start = self.items.len();
        let last_calls = self.items.last().map_or(0, |last| last.calls.len());
        let mut items = std::mem::take(&mut self.items);
        self.emit(dom, top, id, &mut items);
        self.items = items;
        if start > 0 && self.items[start - 1].calls.len() != last_calls {
            self.reestimate(dom, self.items[start - 1].key);
        }
        for index in start..self.items.len() {
            self.prepare_row(index);
            self.heights.push(self.height_of(dom, &self.items[index]));
            self.pos.insert(self.items[index].key, index);
        }
        let added = self.items.len() - start;
        if added > 0 {
            let first = self.items[start].key;
            self.spans.entry(top).and_modify(|span| span.len += added).or_insert(Span { first, len: added });
        }
    }

    /// Rebuilds the rows of `top` in place, or inserts them after the nearest earlier sibling.
    fn restructure(&mut self, dom: &Dom, top: NodeId) {
        let range = self.span_range(top).unwrap_or_else(|| {
            let siblings = self.session.and_then(|s| dom.get(s)).map_or(&[][..], |s| &s.children);
            let at = siblings.iter().rev().skip_while(|s| **s != top).skip(1).find_map(|s| self.span_range(*s)).map_or(0, |r| r.end);
            at..at
        });
        self.rebuild_top(dom, top, range);
    }

    fn span_range(&self, top: NodeId) -> Option<Range<usize>> {
        let span = self.spans.get(&top)?;
        let start = *self.pos.get(&span.first)?;
        Some(start..start + span.len)
    }

    /// Replaces the rows in `range` with the current rows of `top`.
    fn rebuild_top(&mut self, dom: &Dom, top: NodeId, range: Range<usize>) {
        let mut fresh = Vec::new();
        self.emit(dom, top, top, &mut fresh);
        let kept: HashSet<NodeId> = fresh.iter().map(|r| r.key).collect();
        for old in &self.items[range.clone()] {
            if !kept.contains(&old.key) {
                self.pos.remove(&old.key);
                self.measured.remove(&old.key);
                self.streamed.remove(&old.key);
                self.expanded.remove(&old.key);
            }
        }
        let (start, added) = (range.start, fresh.len());
        self.items.splice(range.clone(), fresh);
        (start..start + added).for_each(|index| self.prepare_row(index));
        let heights: Vec<f32> = self.items[start..start + added].iter().map(|r| self.height_of(dom, r)).collect();
        self.heights.splice(range, heights);
        for (index, row) in self.items.iter().enumerate().skip(start) {
            self.pos.insert(row.key, index);
        }
        match self.items.get(start).filter(|_| added > 0) {
            Some(first) => self.spans.insert(top, Span { first: first.key, len: added }),
            None => self.spans.remove(&top),
        };
    }

    /// Gives a new assistant row an empty parse cell, filled the first time the row is drawn;
    /// rows that existed before keep their parse.
    fn prepare_row(&mut self, index: usize) {
        let row = &self.items[index];
        if row.kind == ItemKind::Assistant {
            self.streamed.entry(row.key).or_default();
        } else {
            self.streamed.remove(&row.key);
        }
    }

    fn set(&mut self, dom: &Dom, id: NodeId, name: &str) {
        let Some(node) = dom.get(id) else { return };
        if matches!((node.kind, name), (ElementKind::Call, attr::TOOL) | (ElementKind::Message, attr::ROLE | attr::KIND)) {
            if let Some(top) = items::top_of(dom, id) {
                self.restructure(dom, top);
            }
        } else if name == TEXT {
            if let Some(cell) = self.streamed.get_mut(&id) {
                // Parsed again when the row is next drawn.
                cell.take();
            }
            self.reestimate(dom, id);
        }
    }

    /// Refreshes the estimate of the row showing `id` while it is unmeasured.
    fn reestimate(&mut self, dom: &Dom, id: NodeId) {
        let Some(key) = self.owner.get(&id).copied().filter(|k| !self.measured.contains_key(k)) else { return };
        let Some(&index) = self.pos.get(&key) else { return };
        let estimate = self.estimate(dom, &self.items[index]);
        self.heights.set(index, estimate);
    }

    fn height_of(&self, dom: &Dom, row: &Item) -> f32 { self.measured.get(&row.key).copied().unwrap_or_else(|| self.estimate(dom, row)) }
    fn estimate(&self, dom: &Dom, row: &Item) -> f32 { self.cx(dom, row, false).map_or(ROW, |cx| renderer(row.kind).estimate(row, &cx)) + PADDING }
}
