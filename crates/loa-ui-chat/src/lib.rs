//! Chat transcript rendered from the session DOM.
//!
//! [`State`] is fed every committed patch with [`State::apply`] and keeps a
//! cache per row: assistant markdown is parsed incrementally, so a token
//! append parses only the new text and touches one row. The transcript is a
//! virtual list: [`State::view`] builds only the rows that intersect the
//! viewport plus an overscan margin, sized by measured heights where known
//! and estimates elsewhere. The view follows new content only while it is
//! scrolled to the bottom.

mod heights;
mod items;
mod prose;
mod rows;
mod stream;
mod style;

pub use items::{COMMANDS_EXT, ItemKind};

use heights::Heights;
use iced::widget::{
    self, button, container, keyed_column, markdown, scrollable, sensor, space, stack, text,
};
use iced::{Element, Fill, Task, Theme};
use items::{Emitter, Item};
use loa_core::{Dom, ElementKind, Node, NodeId, Op, Patch, attr, schema::TEXT};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use stream::Streamed;

/// Pixels of rows built above and below the viewport.
const OVERSCAN: f32 = 600.0;
/// Viewport height assumed until the scrollable reports one.
const DEFAULT_VIEWPORT: f32 = 1000.0;
/// Upper bound on rows built per frame, whatever their heights.
const MAX_WINDOW: usize = 200;
/// Distance from the end that still counts as "at the bottom".
const BOTTOM_SLACK: f32 = 8.0;
/// Measurement changes smaller than this are ignored.
const HEIGHT_EPSILON: f32 = 0.5;

const LINE: f32 = 22.0;
const CHARS_PER_LINE: usize = 90;
const ROW: f32 = 30.0;
const PADDING: f32 = 12.0;
/// Widest the transcript column grows; wider windows center it.
const COLUMN_WIDTH: f32 = 700.0;
/// Space between the column and the edges of a narrow transcript.
const SIDE_PADDING: f32 = 24.0;

/// Where a decision card goes: the app renders the card for `node` in its
/// place, so this crate never draws decisions itself.
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
    fn from(viewport: scrollable::Viewport) -> Self {
        Self {
            offset: viewport.absolute_offset().y,
            viewport: viewport.bounds().height,
            content: viewport.content_bounds().height,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Scrolled(ScrollMetrics),
    /// A row was laid out at `height`.
    Measured {
        key: NodeId,
        height: f32,
    },
    /// Expand or collapse the row starting at this node.
    Toggle(NodeId),
    /// A user message was clicked.
    Rewind(NodeId),
    Link(markdown::Uri),
    JumpToLatest,
}

/// What the app must act on.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Rewind the session to this user message.
    Rewind(NodeId),
    /// A link in assistant markdown was clicked.
    OpenLink(String),
}

#[derive(Clone, Copy, Debug)]
struct Scroll {
    metrics: ScrollMetrics,
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
    items: Vec<Item>,
    heights: Heights,
    /// Row index by row key.
    pos: HashMap<NodeId, usize>,
    /// Row key of every node a row shows. Entries of removed nodes stay
    /// behind but are inert: ids are never reused and their rows are gone
    /// from `pos`.
    owner: HashMap<NodeId, NodeId>,
    spans: HashMap<NodeId, Span>,
    measured: HashMap<NodeId, f32>,
    streamed: HashMap<NodeId, Streamed>,
    expanded: HashSet<NodeId>,
    scroll: Scroll,
    look: style::Look,
    scroll_id: widget::Id,
}

impl State {
    /// An empty transcript styled for `theme`.
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
            scroll: Scroll {
                metrics: ScrollMetrics::default(),
                known: false,
                follow: true,
            },
            look: style::Look::new(theme),
            scroll_id: widget::Id::unique(),
        }
    }

    /// A transcript of every row of `dom`.
    pub fn from_dom(dom: &Dom, theme: &Theme) -> Self {
        let mut state = Self::new(theme);
        state.load(dom);
        state
    }

    /// Restyles the transcript for `theme`. Colors set through style
    /// functions follow the theme the app renders with.
    pub fn set_theme(&mut self, theme: &Theme) {
        self.look = style::Look::new(theme);
    }

    /// The id of the transcript scrollable.
    pub fn scroll_id(&self) -> &widget::Id {
        &self.scroll_id
    }

    /// True while the view follows new content.
    pub fn is_following(&self) -> bool {
        self.scroll.follow
    }

    /// Rebuilds every row from `dom`, keeping the scroll position state.
    pub fn load(&mut self, dom: &Dom) {
        self.items.clear();
        self.heights = Heights::default();
        self.pos.clear();
        self.owner.clear();
        self.spans.clear();
        self.measured.clear();
        self.streamed.clear();
        self.expanded.clear();
        self.session = dom
            .root()
            .filter(|r| r.kind == ElementKind::Session)
            .map(|r| r.id);
        let Some(session) = self.session else {
            return;
        };
        let tops: Vec<NodeId> = dom.children(session).map(|n| n.id).collect();
        for top in tops {
            let at = self.items.len();
            self.rebuild_top(dom, top, at..at);
        }
    }

    /// Updates the rows after `patch` was applied to `dom`. `dom` is the
    /// state after the patch.
    pub fn apply(&mut self, patch: &Patch, dom: &Dom) {
        match &patch.op {
            Op::Insert {
                kind: ElementKind::Session,
                ..
            } => self.load(dom),
            Op::Insert { .. } => self.inserted(dom, patch.node),
            Op::Set { name, .. } => self.set(dom, patch.node, name),
            Op::Append { text } => self.appended(dom, patch.node, text),
            Op::Remove => self.removed(dom, patch.node),
        }
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Scrolled(metrics) => (self.scrolled(metrics), None),
            Message::Measured { key, height } => (self.measure(key, height), None),
            Message::Toggle(key) => {
                if !self.expanded.remove(&key) {
                    self.expanded.insert(key);
                }
                (Task::none(), None)
            }
            Message::Rewind(node) => (Task::none(), Some(Event::Rewind(node))),
            Message::Link(uri) => (Task::none(), Some(Event::OpenLink(uri))),
            Message::JumpToLatest => {
                self.scroll.follow = true;
                (widget::operation::snap_to_end(self.scroll_id.clone()), None)
            }
        }
    }

    /// The transcript. Rows are built only around the viewport. `map` lifts
    /// this component's messages into the app's, and `slot` renders the
    /// decision card for a [`Slot`].
    pub fn view<'a, M: 'a>(
        &'a self,
        dom: &'a Dom,
        map: impl Fn(Message) -> M + Clone + 'a,
        slot: impl Fn(Slot) -> Element<'a, M>,
    ) -> Element<'a, M> {
        let window = self.window();
        let above = self.heights.prefix(window.start) as f32;
        let below = (self.heights.total() - self.heights.prefix(window.end)) as f32;

        let built = self.items[window.clone()].iter().map(|item| {
            let key = item.key;
            let content: Element<'a, M> = if item.kind == ItemKind::Decision {
                slot(Slot {
                    kind: item.kind,
                    node: key,
                })
            } else {
                let expanded = self.expanded.contains(&key);
                rows::view(dom, item, expanded, self.streamed.get(&key), self.look)
                    .map(map.clone())
            };
            let (on_show, on_resize) = (map.clone(), map.clone());
            let measured = sensor(
                container(container(content).width(Fill).max_width(COLUMN_WIDTH))
                    .center_x(Fill)
                    .padding([PADDING / 2.0, SIDE_PADDING]),
            )
            .key(key.0)
            .anticipate(OVERSCAN)
            .on_show(move |size| {
                on_show(Message::Measured {
                    key,
                    height: size.height,
                })
            })
            .on_resize(move |size| {
                on_resize(Message::Measured {
                    key,
                    height: size.height,
                })
            });
            (key.0, Element::from(measured))
        });

        // Spacers stand in for the rows outside the window. Keys 0 (the
        // document) and `u64::MAX` are never row keys.
        let list = keyed_column(
            std::iter::once((0, space().height(above).into()))
                .chain(built)
                .chain(std::iter::once((u64::MAX, space().height(below).into()))),
        )
        .width(Fill);

        let on_scroll = map.clone();
        let scrollbar = scrollable::Scrollbar::new().width(6).scroller_width(6);
        let transcript = scrollable(list)
            .id(self.scroll_id.clone())
            .direction(scrollable::Direction::Vertical(scrollbar))
            .width(Fill)
            .height(Fill)
            .on_scroll(move |viewport| on_scroll(Message::Scrolled(viewport.into())));

        if self.scroll.follow {
            return transcript.into();
        }
        let jump = button(text("Jump to latest ↓").size(style::SMALL_SIZE))
            .style(style::jump)
            .padding([6, 14])
            .on_press(Message::JumpToLatest);
        let overlay = container(Element::from(jump).map(map))
            .padding(16)
            .center_x(Fill)
            .align_bottom(Fill);
        stack![transcript, overlay].into()
    }

    /// Indices of the rows to build: those intersecting the viewport plus
    /// [`OVERSCAN`] on each side, at most [`MAX_WINDOW`].
    fn window(&self) -> Range<usize> {
        let n = self.items.len();
        let viewport = if self.scroll.known {
            f64::from(self.scroll.metrics.viewport)
        } else {
            0.0
        };
        let viewport = if viewport > 0.0 {
            viewport
        } else {
            f64::from(DEFAULT_VIEWPORT)
        };
        let total = self.heights.total();
        let bottom = (total - viewport).max(0.0);
        let top = if self.scroll.follow {
            bottom
        } else {
            f64::from(self.scroll.metrics.offset).clamp(0.0, bottom)
        };
        let overscan = f64::from(OVERSCAN);
        let (Some(first), Some(last)) = (
            self.heights.find((top - overscan).max(0.0)),
            self.heights.find(top + viewport + overscan),
        ) else {
            return 0..0;
        };
        if self.scroll.follow && last + 1 == n {
            n - (n - first).min(MAX_WINDOW)..n
        } else {
            first..(last + 1).min(first + MAX_WINDOW)
        }
    }

    fn scrolled(&mut self, metrics: ScrollMetrics) -> Task<Message> {
        let at_bottom = metrics.offset + metrics.viewport >= metrics.content - BOTTOM_SLACK;
        if at_bottom {
            self.scroll.follow = true;
        } else if self.scroll.known && metrics.offset + HEIGHT_EPSILON < self.scroll.metrics.offset
        {
            // The user scrolled up. Content growing under a still viewport
            // leaves the offset alone and keeps following.
            self.scroll.follow = false;
        }
        self.scroll.metrics = metrics;
        self.scroll.known = true;
        if self.scroll.follow && !at_bottom {
            return widget::operation::snap_to_end(self.scroll_id.clone());
        }
        Task::none()
    }

    fn measure(&mut self, key: NodeId, height: f32) -> Task<Message> {
        let Some(&index) = self.pos.get(&key) else {
            return Task::none();
        };
        let Some(old) = self.heights.get(index) else {
            return Task::none();
        };
        self.measured.insert(key, height);
        let delta = height - old;
        if delta.abs() < HEIGHT_EPSILON {
            return Task::none();
        }
        let above_viewport =
            self.heights.prefix(index + 1) <= f64::from(self.scroll.metrics.offset);
        self.heights.set(index, height);
        if self.scroll.follow || !above_viewport {
            return Task::none();
        }
        // A row above the viewport changed size: shift the offset with it so
        // the rows the user is reading stay put.
        self.scroll.metrics.offset += delta;
        self.scroll.metrics.content += delta;
        widget::operation::scroll_by(
            self.scroll_id.clone(),
            widget::operation::AbsoluteOffset { x: 0.0, y: delta },
        )
    }

    fn inserted(&mut self, dom: &Dom, id: NodeId) {
        let Some(top) = items::top_of(dom, id) else {
            return;
        };
        if self.appends_at_end(dom, id, top) {
            self.append_rows(dom, id, top);
        } else {
            self.restructure(dom, top);
        }
    }

    /// True when the rows of `id` go after every existing row: `id` is last
    /// within `top`, and no row comes after the rows of `top`.
    fn appends_at_end(&self, dom: &Dom, id: NodeId, top: NodeId) -> bool {
        let mut child = id;
        while child != top {
            let Some(parent) = dom.get(child).and_then(|n| dom.get(n.parent)) else {
                return false;
            };
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
                let Some(session) = self.session else {
                    return false;
                };
                let siblings = dom.get(session).map_or(&[][..], |s| &s.children);
                let after = siblings.iter().rev().take_while(|s| **s != top);
                after.into_iter().all(|s| !self.spans.contains_key(s))
            }
        }
    }

    /// Appends the rows of the subtree at `id`, which comes after every
    /// existing row. A call may join the last row's group instead.
    fn append_rows(&mut self, dom: &Dom, id: NodeId, top: NodeId) {
        let Some(node) = dom.get(id) else {
            return;
        };
        let start = self.items.len();
        let last_calls = self.items.last().map(|last| last.calls.len());
        let owner = &mut self.owner;
        let mut emitter = Emitter {
            dom,
            top,
            own: |node, key| {
                owner.insert(node, key);
            },
        };
        emitter.emit(&mut self.items, node);

        if start > 0 && self.items[start - 1].calls.len() != last_calls.unwrap_or(0) {
            let key = self.items[start - 1].key;
            if !self.measured.contains_key(&key) {
                let estimate = self.estimate(dom, &self.items[start - 1]);
                self.heights.set(start - 1, estimate);
            }
        }
        for index in start..self.items.len() {
            self.prepare_row(dom, index);
            let height = self.height_of(dom, &self.items[index]);
            self.heights.push(height);
            self.pos.insert(self.items[index].key, index);
        }
        let added = self.items.len() - start;
        if added > 0 {
            let first = self.items[start].key;
            self.spans
                .entry(top)
                .and_modify(|span| span.len += added)
                .or_insert(Span { first, len: added });
        }
    }

    /// Rebuilds the rows of `top` in place.
    fn restructure(&mut self, dom: &Dom, top: NodeId) {
        let range = match self.span_range(top) {
            Some(range) => range,
            None => {
                let at = self.insertion_point(dom, top);
                at..at
            }
        };
        self.rebuild_top(dom, top, range);
    }

    fn span_range(&self, top: NodeId) -> Option<Range<usize>> {
        let span = self.spans.get(&top)?;
        let start = *self.pos.get(&span.first)?;
        Some(start..start + span.len)
    }

    /// Where the rows of `top`, which has none yet, go: after the rows of the
    /// nearest earlier child of the session that has rows.
    fn insertion_point(&self, dom: &Dom, top: NodeId) -> usize {
        let Some(siblings) = self.session.and_then(|s| dom.get(s)).map(|s| &s.children) else {
            return self.items.len();
        };
        siblings
            .iter()
            .rev()
            .skip_while(|s| **s != top)
            .skip(1)
            .find_map(|s| self.span_range(*s))
            .map_or(0, |range| range.end)
    }

    /// Replaces the rows in `range` with the current rows of `top`.
    fn rebuild_top(&mut self, dom: &Dom, top: NodeId, range: Range<usize>) {
        let mut fresh = Vec::new();
        if let Some(node) = dom.get(top) {
            let owner = &mut self.owner;
            let mut emitter = Emitter {
                dom,
                top,
                own: |node, key| {
                    owner.insert(node, key);
                },
            };
            emitter.emit(&mut fresh, node);
        }
        let kept: HashSet<NodeId> = fresh.iter().map(|r| r.key).collect();
        for old in &self.items[range.clone()] {
            if !kept.contains(&old.key) {
                self.pos.remove(&old.key);
                self.measured.remove(&old.key);
                self.streamed.remove(&old.key);
                self.expanded.remove(&old.key);
            }
        }
        let start = range.start;
        let added = fresh.len();
        self.items.splice(range.clone(), fresh);
        for index in start..start + added {
            self.prepare_row(dom, index);
        }
        let heights: Vec<f32> = self.items[start..start + added]
            .iter()
            .map(|r| self.height_of(dom, r))
            .collect();
        self.heights.splice(range, heights);
        for (index, row) in self.items.iter().enumerate().skip(start) {
            self.pos.insert(row.key, index);
        }
        match self.items.get(start).filter(|_| added > 0) {
            Some(first) => {
                self.spans.insert(
                    top,
                    Span {
                        first: first.key,
                        len: added,
                    },
                );
            }
            None => {
                self.spans.remove(&top);
            }
        }
    }

    /// Parses the markdown of a new assistant row once; rows that existed
    /// before keep their parse.
    fn prepare_row(&mut self, dom: &Dom, index: usize) {
        let row = &self.items[index];
        if row.kind != ItemKind::Assistant {
            self.streamed.remove(&row.key);
            return;
        }
        if let Some(node) = dom.get(row.key) {
            self.streamed
                .entry(row.key)
                .or_insert_with(|| Streamed::new(&node.text));
        }
    }

    fn set(&mut self, dom: &Dom, id: NodeId, name: &str) {
        let Some(node) = dom.get(id) else {
            return;
        };
        let structural = matches!(
            (node.kind, name),
            (ElementKind::Call, attr::TOOL) | (ElementKind::Message, attr::ROLE | attr::KIND)
        );
        if structural {
            if let Some(top) = items::top_of(dom, id) {
                self.restructure(dom, top);
            }
            return;
        }
        if name == TEXT {
            if let Some(streamed) = self.streamed.get_mut(&id) {
                *streamed = Streamed::new(&node.text);
            }
            self.reestimate(dom, id);
        }
    }

    fn appended(&mut self, dom: &Dom, id: NodeId, chunk: &str) {
        if let (Some(streamed), Some(node)) = (self.streamed.get_mut(&id), dom.get(id)) {
            streamed.append(&node.text, chunk);
        }
        self.reestimate(dom, id);
    }

    /// Refreshes the height estimate of the row showing `id` while it has not
    /// been measured.
    fn reestimate(&mut self, dom: &Dom, id: NodeId) {
        let Some(key) = self.owner.get(&id).copied() else {
            return;
        };
        let Some(&index) = self.pos.get(&key) else {
            return;
        };
        if self.measured.contains_key(&key) {
            return;
        }
        let estimate = self.estimate(dom, &self.items[index]);
        self.heights.set(index, estimate);
    }

    fn removed(&mut self, dom: &Dom, id: NodeId) {
        if Some(id) == self.session {
            self.load(dom);
            return;
        }
        if self.spans.contains_key(&id) {
            self.restructure(dom, id);
            return;
        }
        let top = self
            .owner
            .get(&id)
            .and_then(|key| self.pos.get(key))
            .map(|index| self.items[*index].top);
        if let Some(top) = top {
            self.restructure(dom, top);
        }
    }

    fn height_of(&self, dom: &Dom, row: &Item) -> f32 {
        self.measured
            .get(&row.key)
            .copied()
            .unwrap_or_else(|| self.estimate(dom, row))
    }

    /// A guess at the height of a row that has not been laid out. Constant
    /// time, so streaming can refresh it on every append.
    fn estimate(&self, dom: &Dom, row: &Item) -> f32 {
        let node = dom.get(row.key);
        let text_lines =
            |node: Option<&Node>| node.map_or(1, |n| n.text.len() / CHARS_PER_LINE + 1) as f32;
        let expanded = self.expanded.contains(&row.key);
        let body = match row.kind {
            ItemKind::User | ItemKind::System => LINE * text_lines(node) + PADDING,
            ItemKind::Assistant => match self.streamed.get(&row.key) {
                Some(s) => LINE * (s.lines() + s.fed() / CHARS_PER_LINE + 1) as f32,
                None => LINE * text_lines(node),
            },
            ItemKind::Thinking if expanded => ROW + LINE * text_lines(node),
            ItemKind::Calls | ItemKind::Job | ItemKind::Compaction | ItemKind::Ext if expanded => {
                ROW + row.calls.len().max(1) as f32 * 12.0 * LINE
            }
            ItemKind::Decision => 6.0 * LINE,
            ItemKind::Todo | ItemKind::Surface => LINE,
            ItemKind::Thinking
            | ItemKind::Calls
            | ItemKind::Job
            | ItemKind::Compaction
            | ItemKind::Ext => ROW,
        };
        body + PADDING
    }
}

#[cfg(test)]
mod tests;
