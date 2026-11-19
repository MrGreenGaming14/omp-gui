//! Terminal emulation: an `alacritty_terminal` grid fed with PTY bytes.

use std::sync::mpsc;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode, viewport_to_point};
use alacritty_terminal::vte::ansi::Processor;

use crate::GridSize;

/// Lines of scrollback kept per terminal.
pub const SCROLLBACK_LINES: usize = 10_000;

/// Forwards the terminal events the pane acts on; redraw hints are dropped
/// because the pane redraws after every chunk anyway.
#[derive(Clone)]
pub struct Listener(mpsc::Sender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        match event {
            TermEvent::Wakeup | TermEvent::MouseCursorDirty | TermEvent::CursorBlinkingChange => {}
            event => {
                // The receiver lives as long as the emulator that owns this
                // listener, so a failed send cannot lose anything.
                let _ = self.0.send(event);
            }
        }
    }
}

/// A cell position on screen: `row` 0 is the top visible line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportCell {
    /// Column from the left.
    pub col: usize,
    /// Row from the top of the visible area.
    pub row: usize,
}

/// One terminal's screen, scrollback, modes and selection.
pub struct Emulator {
    term: Term<Listener>,
    parser: Processor,
    events: mpsc::Receiver<TermEvent>,
    size: GridSize,
}

fn dimensions(size: GridSize) -> TermSize {
    TermSize::new(usize::from(size.cols.max(2)), usize::from(size.rows.max(1)))
}

impl Emulator {
    /// An empty terminal of the given size.
    pub fn new(size: GridSize) -> Self {
        let (sender, events) = mpsc::channel();
        let config = Config {
            scrolling_history: SCROLLBACK_LINES,
            ..Config::default()
        };
        let term = Term::new(config, &dimensions(size), Listener(sender));
        Self {
            term,
            parser: Processor::new(),
            events,
            size,
        }
    }

    /// Feeds program output into the terminal.
    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Events raised by the output fed so far: replies the program expects
    /// on its input, title changes, clipboard stores, the bell.
    pub fn take_events(&mut self) -> impl Iterator<Item = TermEvent> + '_ {
        self.events.try_iter()
    }

    /// Current grid size.
    pub fn size(&self) -> GridSize {
        self.size
    }

    /// Resizes the grid, reflowing text. Returns false when the size in
    /// cells did not change.
    pub fn resize(&mut self, size: GridSize) -> bool {
        if size == self.size {
            return false;
        }
        let cells_changed = size.cols != self.size.cols || size.rows != self.size.rows;
        self.size = size;
        if cells_changed {
            self.term.resize(dimensions(size));
        }
        true
    }

    /// Moves the view through the scrollback.
    pub fn scroll(&mut self, scroll: Scroll) {
        self.term.scroll_display(scroll);
    }

    /// Lines the view is scrolled up from the bottom.
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Lines held in scrollback above the screen.
    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    /// Terminal modes set by the program (application cursor keys,
    /// bracketed paste, alternate screen, mouse reporting).
    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    fn point(&self, cell: ViewportCell) -> Point {
        let row = cell.row.min(self.term.screen_lines().saturating_sub(1));
        let col = cell.col.min(self.term.columns().saturating_sub(1));
        viewport_to_point(self.display_offset(), Point::new(row, Column(col)))
    }

    /// Starts a selection at a visible cell.
    pub fn select_start(&mut self, kind: SelectionType, cell: ViewportCell, side: Side) {
        let point = self.point(cell);
        self.term.selection = Some(Selection::new(kind, point, side));
    }

    /// Extends the selection to a visible cell.
    pub fn select_update(&mut self, cell: ViewportCell, side: Side) {
        let point = self.point(cell);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    /// Drops the selection.
    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// The selected text, or `None` when nothing is selected.
    pub fn selection_text(&self) -> Option<String> {
        self.term
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    /// The visible rows as text, trailing blanks trimmed.
    pub fn screen_text(&self) -> Vec<String> {
        let offset = self.display_offset() as i32;
        let mut lines = vec![String::new(); self.term.screen_lines()];
        for indexed in self.term.grid().display_iter() {
            if indexed.cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let row = (indexed.point.line.0 + offset) as usize;
            if let Some(line) = lines.get_mut(row) {
                line.push(indexed.cell.c);
            }
        }
        for line in &mut lines {
            line.truncate(line.trim_end().len());
        }
        lines
    }

    /// The emulator state, for rendering.
    pub fn term(&self) -> &Term<Listener> {
        &self.term
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollback_keeps_lines_that_left_the_screen() {
        let mut emu = Emulator::new(GridSize::new(20, 5));
        for n in 0..50 {
            emu.advance(format!("line {n}\r\n").as_bytes());
        }
        assert_eq!(emu.screen_text()[0], "line 46");
        emu.scroll(Scroll::Delta(10));
        assert_eq!(emu.display_offset(), 10);
        assert_eq!(emu.screen_text()[0], "line 36");
        emu.scroll(Scroll::Top);
        assert_eq!(emu.screen_text()[0], "line 0");
        emu.scroll(Scroll::Bottom);
        assert_eq!(emu.screen_text()[0], "line 46");
    }

    #[test]
    fn narrowing_the_grid_rewraps_long_lines() {
        let mut emu = Emulator::new(GridSize::new(20, 4));
        emu.advance(b"abcdefghijkl");
        assert!(emu.resize(GridSize::new(6, 4)));
        // The wrapped head moves into scrollback so the cursor line stays put.
        emu.scroll(Scroll::Top);
        assert_eq!(emu.screen_text()[..2], ["abcdef", "ghijkl"]);
        assert!(!emu.resize(GridSize::new(6, 4)));
    }

    #[test]
    fn selection_copies_text_across_lines() {
        let mut emu = Emulator::new(GridSize::new(20, 4));
        emu.advance(b"first line\r\nsecond line");
        emu.select_start(
            SelectionType::Simple,
            ViewportCell { col: 6, row: 0 },
            Side::Left,
        );
        emu.select_update(ViewportCell { col: 5, row: 1 }, Side::Right);
        assert_eq!(emu.selection_text().as_deref(), Some("line\nsecond"));
        emu.clear_selection();
        assert_eq!(emu.selection_text(), None);
    }

    #[test]
    fn selection_in_scrollback_follows_the_view() {
        let mut emu = Emulator::new(GridSize::new(10, 2));
        for n in 0..6 {
            emu.advance(format!("row{n}\r\n").as_bytes());
        }
        emu.scroll(Scroll::Top);
        emu.select_start(SelectionType::Lines, ViewportCell { col: 0, row: 0 }, Side::Left);
        emu.select_update(ViewportCell { col: 0, row: 0 }, Side::Right);
        assert_eq!(emu.selection_text().as_deref(), Some("row0\n"));
    }

    #[test]
    fn device_status_query_produces_a_reply_for_the_program() {
        let mut emu = Emulator::new(GridSize::new(80, 24));
        emu.advance(b"ab\x1b[6n");
        let replies: Vec<String> = emu
            .take_events()
            .filter_map(|event| match event {
                TermEvent::PtyWrite(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(replies, ["\x1b[1;3R"]);
    }

    #[test]
    fn title_escape_raises_a_title_event() {
        let mut emu = Emulator::new(GridSize::new(80, 24));
        emu.advance(b"\x1b]0;vim main.rs\x07");
        assert!(
            emu.take_events()
                .any(|event| matches!(event, TermEvent::Title(title) if title == "vim main.rs"))
        );
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let mut emu = Emulator::new(GridSize::new(10, 2));
        emu.advance("日本x".as_bytes());
        assert_eq!(emu.screen_text()[0], "日本x");
        assert_eq!(emu.term().grid().cursor.point.column.0, 5);
    }
}
