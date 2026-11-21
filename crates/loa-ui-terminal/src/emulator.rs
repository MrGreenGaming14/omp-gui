//! Terminal emulation: an `alacritty_terminal` grid fed with PTY bytes.

use crate::GridSize;
use alacritty_terminal::{Term, event::{Event as TermEvent, EventListener, WindowSize}, grid::Dimensions, index::{Column, Point, Side}, selection::{Selection, SelectionType}};
use alacritty_terminal::{term::{Config, test::TermSize, viewport_to_point}, vte::ansi::Processor};
use std::sync::mpsc;

/// Forwards the terminal's events to the pane, which drains them after each chunk.
pub struct Listener(mpsc::Sender<TermEvent>);
impl EventListener for Listener { fn send_event(&self, event: TermEvent) { let _ = self.0.send(event); } }

/// A visible cell: row 0 is the top visible line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportCell { pub col: usize, pub row: usize }

/// One terminal's screen, scrollback, modes and selection.
pub struct Emulator { pub term: Term<Listener>, parser: Processor, events: mpsc::Receiver<TermEvent>, size: GridSize }
fn dimensions(s: GridSize) -> TermSize { TermSize::new(usize::from(s.cols.max(2)), usize::from(s.rows.max(1))) }
impl From<GridSize> for WindowSize { fn from(s: GridSize) -> Self { Self { num_lines: s.rows, num_cols: s.cols, cell_width: s.cell_width, cell_height: s.cell_height } } }

impl Emulator {
    pub fn new(size: GridSize) -> Self {
        let (sender, events) = mpsc::channel();
        let term = Term::new(Config { scrolling_history: 10_000, ..Config::default() }, &dimensions(size), Listener(sender));
        Self { term, parser: Processor::new(), events, size }
    }
    pub fn advance(&mut self, bytes: &[u8]) { self.parser.advance(&mut self.term, bytes) }
    /// Replies the program expects, title changes, clipboard stores, raised by output so far.
    pub fn take_events(&mut self) -> Vec<TermEvent> { self.events.try_iter().collect() }
    pub fn size(&self) -> GridSize { self.size }
    /// Resizes, reflowing text; false when nothing changed.
    pub fn resize(&mut self, size: GridSize) -> bool {
        if size == self.size { return false }
        if (size.cols, size.rows) != (self.size.cols, self.size.rows) { self.term.resize(dimensions(size)) }
        self.size = size;
        true
    }
    fn point(&self, cell: ViewportCell) -> Point {
        let (row, col) = (cell.row.min(self.term.screen_lines().saturating_sub(1)), cell.col.min(self.term.columns().saturating_sub(1)));
        viewport_to_point(self.term.grid().display_offset(), Point::new(row, Column(col)))
    }
    pub fn select_start(&mut self, kind: SelectionType, cell: ViewportCell, side: Side) { self.term.selection = Some(Selection::new(kind, self.point(cell), side)) }
    pub fn select_update(&mut self, cell: ViewportCell, side: Side) {
        let point = self.point(cell);
        if let Some(s) = self.term.selection.as_mut() { s.update(point, side) }
    }
    pub fn selection_text(&self) -> Option<String> { self.term.selection_to_string().filter(|t| !t.is_empty()) }
    pub fn screen_text(&self) -> String { self.term.bounds_to_string(self.point(ViewportCell { col: 0, row: 0 }), self.point(ViewportCell { col: usize::MAX, row: usize::MAX })) }
}
