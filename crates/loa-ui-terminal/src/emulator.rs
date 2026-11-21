//! Terminal emulation: an `alacritty_terminal` grid fed with PTY bytes.

use crate::GridSize;
use alacritty_terminal::Term;
use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode, viewport_to_point};
use alacritty_terminal::vte::ansi::Processor;
use std::sync::mpsc;

pub const SCROLLBACK_LINES: usize = 10_000;

/// Forwards the events the pane acts on; redraw hints are dropped, the pane redraws per chunk.
#[derive(Clone)]
pub struct Listener(mpsc::Sender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        if !matches!(event, TermEvent::Wakeup | TermEvent::MouseCursorDirty | TermEvent::CursorBlinkingChange) { let _ = self.0.send(event); }
    }
}

/// A visible cell: row 0 is the top visible line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportCell {
    pub col: usize,
    pub row: usize,
}

/// One terminal's screen, scrollback, modes and selection.
pub struct Emulator {
    term: Term<Listener>,
    parser: Processor,
    events: mpsc::Receiver<TermEvent>,
    size: GridSize,
}

fn dimensions(s: GridSize) -> TermSize { TermSize::new(usize::from(s.cols.max(2)), usize::from(s.rows.max(1))) }

impl Emulator {
    pub fn new(size: GridSize) -> Self {
        let (sender, events) = mpsc::channel();
        let term = Term::new(Config { scrolling_history: SCROLLBACK_LINES, ..Config::default() }, &dimensions(size), Listener(sender));
        Self { term, parser: Processor::new(), events, size }
    }

    pub fn advance(&mut self, bytes: &[u8]) { self.parser.advance(&mut self.term, bytes) }

    /// Replies the program expects, title changes, clipboard stores, raised by output so far.
    pub fn take_events(&mut self) -> impl Iterator<Item = TermEvent> + '_ { self.events.try_iter() }

    pub fn size(&self) -> GridSize { self.size }

    /// Resizes, reflowing text; false when nothing changed.
    pub fn resize(&mut self, size: GridSize) -> bool {
        if size == self.size { return false }
        if (size.cols, size.rows) != (self.size.cols, self.size.rows) { self.term.resize(dimensions(size)) }
        self.size = size;
        true
    }

    pub fn scroll(&mut self, scroll: Scroll) { self.term.scroll_display(scroll) }

    /// Lines the view is scrolled up from the bottom.
    pub fn display_offset(&self) -> usize { self.term.grid().display_offset() }

    /// Modes the program set: cursor keys, bracketed paste, alternate screen, mouse.
    pub fn mode(&self) -> TermMode { *self.term.mode() }

    fn point(&self, cell: ViewportCell) -> Point {
        let (row, col) = (cell.row.min(self.term.screen_lines().saturating_sub(1)), cell.col.min(self.term.columns().saturating_sub(1)));
        viewport_to_point(self.display_offset(), Point::new(row, Column(col)))
    }

    pub fn select_start(&mut self, kind: SelectionType, cell: ViewportCell, side: Side) { self.term.selection = Some(Selection::new(kind, self.point(cell), side)) }

    pub fn select_update(&mut self, cell: ViewportCell, side: Side) {
        let point = self.point(cell);
        if let Some(s) = self.term.selection.as_mut() { s.update(point, side) }
    }

    pub fn selection_text(&self) -> Option<String> { self.term.selection_to_string().filter(|t| !t.is_empty()) }

    /// The visible rows as text, trailing blanks trimmed.
    pub fn screen_text(&self) -> Vec<String> {
        let offset = self.display_offset() as i32;
        let mut lines = vec![String::new(); self.term.screen_lines()];
        for cell in self.term.grid().display_iter().filter(|c| !c.cell.flags.contains(Flags::WIDE_CHAR_SPACER)) {
            if let Some(line) = lines.get_mut((cell.point.line.0 + offset) as usize) { line.push(cell.cell.c) }
        }
        lines.iter_mut().for_each(|l| l.truncate(l.trim_end().len()));
        lines
    }

    pub fn term(&self) -> &Term<Listener> { &self.term }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollback_reflow_and_wide_characters() {
        let mut emu = Emulator::new(GridSize::new(20, 5));
        (0..50).for_each(|n| emu.advance(format!("line {n}\r\n").as_bytes()));
        for (scroll, top) in [(Scroll::Delta(10), "line 36"), (Scroll::Top, "line 0"), (Scroll::Bottom, "line 46")] {
            emu.scroll(scroll);
            assert_eq!(emu.screen_text()[0], top);
        }
        let mut emu = Emulator::new(GridSize::new(20, 4));
        emu.advance("abcdefghijkl\r\n日本x".as_bytes());
        assert!(emu.resize(GridSize::new(6, 4)) && !emu.resize(GridSize::new(6, 4)));
        emu.scroll(Scroll::Top);
        assert_eq!(emu.screen_text()[..3], ["abcdef", "ghijkl", "日本x"]);
    }

    #[test]
    fn selection_queries_and_titles() {
        let mut emu = Emulator::new(GridSize::new(20, 4));
        emu.advance(b"first line\r\nsecond line\x1b[6n\x1b]0;vim main.rs\x07");
        emu.select_start(SelectionType::Simple, ViewportCell { col: 6, row: 0 }, Side::Left);
        emu.select_update(ViewportCell { col: 5, row: 1 }, Side::Right);
        assert_eq!(emu.selection_text().as_deref(), Some("line\nsecond"));
        let events: Vec<String> = emu.take_events().filter_map(|e| match e {
            TermEvent::PtyWrite(t) | TermEvent::Title(t) => Some(t),
            _ => None,
        }).collect();
        assert_eq!(events, ["\x1b[2;12R", "vim main.rs"]);
    }
}
