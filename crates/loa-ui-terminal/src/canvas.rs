//! The terminal grid on an iced canvas (background, decoration and text passes over resolved cells) and its input.

use crate::{GridSize, emulator::{Emulator, ViewportCell}, palette::TermPalette, state::{Message, TabId}};
use alacritty_terminal::{index::Side, selection::SelectionType, term::cell::Flags, vte::ansi::CursorShape};
use iced::{advanced::graphics::text::Paragraph, advanced::text::{self, Paragraph as _}, alignment, keyboard::{self, Key, Modifiers}, mouse::{self, ScrollDelta}};
use iced::{theme::palette::mix, widget::canvas::{self, Action, Cache, Event, Frame, Geometry, Stroke}, Color, Font, Pixels, Point, Rectangle, Renderer, Size, Theme, Vector};
use std::{cell::Cell as StdCell, time::{Duration, Instant}};

/// Two clicks on one cell closer than this are a double click.
const MULTI_CLICK: Duration = Duration::from_millis(400);
const LINE_HEIGHT: f32 = 1.25;

/// Font (monospaced) and cell geometry of the grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics { pub font: Font, pub font_size: f32, pub cell: Size }
impl Metrics {
    pub fn measure(font: Font, font_size: f32) -> Self {
        const SAMPLE: &str = "MMMMMMMMMMMMMMMMMMMM";
        let (align_x, align_y, line_height) = (text::Alignment::Left, alignment::Vertical::Top, text::LineHeight::Relative(LINE_HEIGHT));
        let p = Paragraph::with_text(text::Text { content: SAMPLE, bounds: Size::INFINITE, size: Pixels(font_size), line_height, font, align_x, align_y, shaping: text::Shaping::Basic, wrapping: text::Wrapping::None });
        Self { font, font_size, cell: Size::new(p.min_bounds().width / SAMPLE.len() as f32, p.min_bounds().height) }
    }
    /// The grid that fits in `area`, at least 2 by 1.
    pub fn grid_for(&self, area: Size) -> GridSize {
        let fit = |space: f32, cell: f32| if cell <= 0.0 { 0 } else { (space / cell).floor().clamp(0.0, f32::from(u16::MAX)) as u16 };
        GridSize { cols: fit(area.width, self.cell.width).max(2), rows: fit(area.height, self.cell.height).max(1), cell_width: self.cell.width.round() as u16, cell_height: self.cell.height.round() as u16 }
    }
    /// The cell under a canvas position and which half of it; outside positions clamp.
    pub fn cell_at(&self, p: Point, grid: GridSize) -> (ViewportCell, Side) {
        let (x, y) = ((p.x / self.cell.width).max(0.0), (p.y / self.cell.height).max(0.0));
        let cell = ViewportCell { col: (x as usize).min(usize::from(grid.cols.saturating_sub(1))), row: (y as usize).min(usize::from(grid.rows.saturating_sub(1))) };
        (cell, if x.fract() > 0.5 { Side::Right } else { Side::Left })
    }
    fn origin(&self, row: usize, col: usize) -> Point { Point::new(col as f32 * self.cell.width, row as f32 * self.cell.height) }
    fn span(&self, cells: usize, height: f32) -> Size { Size::new(cells as f32 * self.cell.width, height) }
}

/// Canvas input for the state: `Scroll` lines are positive up; `SelectStart` is simple, semantic (2 clicks) or lines (3).
#[derive(Debug, Clone, PartialEq)]
pub enum CanvasEvent {
    Resized(GridSize),
    Key { key: Key, modifiers: Modifiers, text: Option<String> },
    Scroll(i32),
    SelectStart { cell: ViewportCell, side: Side, kind: SelectionType },
    SelectUpdate { cell: ViewportCell, side: Side },
    SelectEnd,
}

#[derive(Debug, Default)]
pub struct CanvasState { focused: bool, selecting: bool, grid: Option<GridSize>, last_click: Option<(Instant, ViewportCell, u8)>, scroll_remainder: f32 }
/// One terminal, drawn and turned into [`Message`]s.
pub struct TerminalCanvas<'a> { pub(crate) tab: TabId, pub(crate) emulator: &'a Emulator, pub(crate) cache: &'a Cache, pub(crate) drawn_with: &'a StdCell<Option<TermPalette>>, pub(crate) metrics: Metrics }

/// A visible cell, colours resolved (inverse, selection, dim), `c` blank for spaces/tabs/hidden text; `run` is the text colour
/// and bold/italic, blanks continue the run before them and wide cells (`None`) stand alone.
struct Painted<'a> { row: usize, col: usize, width: usize, fg: Color, bg: Color, flags: Flags, c: char, zerowidth: Option<&'a [char]>, run: Option<(Color, Flags)> }
fn adjacent(a: &Painted, b: &Painted) -> bool { a.row == b.row && a.col + a.width == b.col }

impl<'a> TerminalCanvas<'a> {
    fn publish(&self, e: CanvasEvent) -> Action<Message> { Action::publish(Message::Canvas(self.tab, e)) }
    fn cells(&self, palette: &TermPalette) -> Vec<Painted<'a>> {
        let content = self.emulator.term.renderable_content();
        let (offset, colors, selection) = (content.display_offset as i32, content.colors, content.selection);
        let mut run = None;
        content.display_iter.filter(|i| !i.cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)).map(|i| {
            let (mut fg, mut bg, flags) = (palette.resolve(i.cell.fg, colors), palette.resolve(i.cell.bg, colors), i.cell.flags);
            if flags.contains(Flags::INVERSE) { std::mem::swap(&mut fg, &mut bg) }
            if selection.is_some_and(|r| r.contains(i.point)) { (fg, bg) = (palette.foreground, palette.selection) }
            if flags.contains(Flags::DIM) { fg = mix(fg, bg, 0.33) }
            let blank = matches!(i.cell.c, ' ' | '\t') || flags.contains(Flags::HIDDEN);
            let wide = flags.contains(Flags::WIDE_CHAR);
            run = if wide { None } else if blank { run } else { Some((fg, flags & (Flags::BOLD | Flags::ITALIC))) };
            let c = if blank { ' ' } else { i.cell.c };
            Painted { row: (i.point.line.0 + offset) as usize, col: i.point.column.0, width: 1 + usize::from(wide), fg, bg, flags, c, zerowidth: i.cell.zerowidth(), run }
        }).collect()
    }
    fn backgrounds(&self, frame: &mut Frame, cells: &[Painted], palette: &TermPalette) {
        for run in cells.chunk_by(|a, b| adjacent(a, b) && a.bg == b.bg).filter(|r| r[0].bg != palette.background) {
            let (first, last) = (&run[0], &run[run.len() - 1]);
            frame.fill_rectangle(self.metrics.origin(first.row, first.col), self.metrics.span(last.col + last.width - first.col, self.metrics.cell.height), first.bg);
        }
    }
    /// Underlines and strikeouts.
    fn decorations(&self, frame: &mut Frame, cells: &[Painted]) {
        let (thickness, ch) = ((self.metrics.font_size / 14.0).max(1.0), self.metrics.cell.height);
        for c in cells {
            for (_, y) in [(c.flags.intersects(Flags::ALL_UNDERLINES), ch - thickness), (c.flags.contains(Flags::STRIKEOUT), ch * 0.5)].into_iter().filter(|(on, _)| *on) {
                frame.fill_rectangle(self.metrics.origin(c.row, c.col) + Vector::new(0.0, y), self.metrics.span(c.width, thickness), c.fg);
            }
        }
    }
    fn text_runs(&self, frame: &mut Frame, cells: &[Painted]) {
        for run in cells.chunk_by(|a, b| adjacent(a, b) && a.run.is_some() && a.run == b.run) {
            let text: String = run.iter().flat_map(|c| std::iter::once(c.c).chain(c.zerowidth.into_iter().flatten().copied())).collect();
            let (first, text) = (&run[0], text.trim_end());
            let (color, style) = first.run.unwrap_or((first.fg, first.flags));
            if !text.is_empty() { frame.fill_text(self.text(text.to_owned(), first.row, first.col, color, style)) }
        }
    }
    fn text(&self, content: String, row: usize, col: usize, color: Color, flags: Flags) -> canvas::Text {
        let mut font = self.metrics.font;
        if flags.contains(Flags::BOLD) { font.weight = iced::font::Weight::Bold }
        if flags.contains(Flags::ITALIC) { font.style = iced::font::Style::Italic }
        let shaping = if content.is_ascii() { text::Shaping::Basic } else { text::Shaping::Advanced };
        let line_height = text::LineHeight::Absolute(Pixels(self.metrics.cell.height));
        canvas::Text { content, position: self.metrics.origin(row, col), color, size: Pixels(self.metrics.font_size), line_height, font, shaping, ..canvas::Text::default() }
    }
    fn draw_cursor(&self, frame: &mut Frame, palette: &TermPalette, focused: bool) {
        let (term, metrics, content) = (&self.emulator.term, self.metrics, self.emulator.term.renderable_content());
        let (shape, point) = (content.cursor.shape, content.cursor.point);
        let row = point.line.0 + content.display_offset as i32;
        if shape == CursorShape::Hidden || !(0..i32::from(self.emulator.size().rows.max(1))).contains(&row) { return }
        let (cell, ch, thickness) = (&term.grid()[point], metrics.cell.height, (metrics.font_size / 8.0).max(1.0));
        let (origin, size) = (metrics.origin(row as usize, point.column.0), metrics.span(1 + usize::from(cell.flags.contains(Flags::WIDE_CHAR)), ch));
        match shape {
            CursorShape::Block if focused => {
                frame.fill_rectangle(origin, size, palette.cursor);
                if cell.c != ' ' { frame.fill_text(self.text(cell.c.to_string(), row as usize, point.column.0, palette.background, cell.flags)) }
            }
            CursorShape::Beam => frame.fill_rectangle(origin, Size::new(thickness, ch), palette.cursor),
            CursorShape::Underline => frame.fill_rectangle(origin + Vector::new(0.0, ch - thickness), Size::new(size.width, thickness), palette.cursor),
            _ => frame.stroke_rectangle(origin, size, Stroke::default().with_color(palette.cursor).with_width(thickness / 2.0)),
        }
    }
    /// A mouse or key event the terminal handles, as a captured message.
    fn input(&self, state: &mut CanvasState, event: &Event, bounds: Rectangle, cursor: mouse::Cursor, grid: GridSize) -> Option<Action<Message>> {
        let event = match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                (state.focused, state.selecting) = (cursor.is_over(bounds), cursor.is_over(bounds));
                let ((cell, side), now) = (self.metrics.cell_at(cursor.position_in(bounds)?, grid), Instant::now());
                let clicks = state.last_click.filter(|&(at, last, _)| last == cell && now - at < MULTI_CLICK).map_or(1, |(_, _, n)| n % 3 + 1);
                state.last_click = Some((now, cell, clicks));
                CanvasEvent::SelectStart { cell, side, kind: [SelectionType::Simple, SelectionType::Semantic, SelectionType::Lines][usize::from(clicks - 1)] }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.selecting => {
                let (cell, side) = self.metrics.cell_at(cursor.position_from(bounds.position())?, grid);
                CanvasEvent::SelectUpdate { cell, side }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.selecting => {
                state.selecting = false;
                CanvasEvent::SelectEnd
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) => {
                let (ScrollDelta::Lines { y, .. } | ScrollDelta::Pixels { y, .. }) = *delta;
                let lines = state.scroll_remainder + if matches!(delta, ScrollDelta::Lines { .. }) { y * 3.0 } else { y / self.metrics.cell.height.max(1.0) };
                state.scroll_remainder = lines.fract();
                if lines.trunc() == 0.0 { return Some(Action::capture()) }
                CanvasEvent::Scroll(lines.trunc() as i32)
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, text, .. }) if state.focused => CanvasEvent::Key { key: key.clone(), modifiers: *modifiers, text: text.as_ref().map(ToString::to_string) },
            _ => return None,
        };
        Some(self.publish(event).and_capture())
    }
}

impl canvas::Program<Message> for TerminalCanvas<'_> {
    type State = CanvasState;
    fn update(&self, state: &mut CanvasState, event: &Event, bounds: Rectangle, cursor: mouse::Cursor) -> Option<Action<Message>> {
        let grid = self.metrics.grid_for(bounds.size());
        let input = self.input(state, event, bounds, cursor, grid);
        // An action carries one message, so a size change waits for an event that isn't input.
        if input.is_some() || state.grid == Some(grid) { return input }
        state.grid = Some(grid);
        Some(self.publish(CanvasEvent::Resized(grid)))
    }
    fn draw(&self, state: &CanvasState, renderer: &Renderer, theme: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let palette = TermPalette::from_theme(theme);
        if self.drawn_with.replace(Some(palette)) != Some(palette) { self.cache.clear() }
        let content = self.cache.draw(renderer, bounds.size(), |frame| {
            frame.fill_rectangle(Point::ORIGIN, frame.size(), palette.background);
            let cells = self.cells(&palette);
            self.backgrounds(frame, &cells, &palette);
            self.decorations(frame, &cells);
            self.text_runs(frame, &cells);
        });
        let mut overlay = Frame::new(renderer, bounds.size());
        self.draw_cursor(&mut overlay, &palette, state.focused);
        vec![content, overlay.into_geometry()]
    }
    fn mouse_interaction(&self, _: &CanvasState, bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        if cursor.is_over(bounds) { mouse::Interaction::Text } else { mouse::Interaction::default() }
    }
}

#[cfg(test)]
#[test]
fn grid_and_pointer_geometry() {
    let m = Metrics { font: Font::MONOSPACE, font_size: 14.0, cell: Size::new(8.0, 16.0) };
    assert_eq!([m.grid_for(Size::new(803.0, 410.0)), m.grid_for(Size::new(3.0, 3.0))].map(|g| (g.cols, g.rows, g.cell_width)), [(100, 25, 8), (2, 1, 8)]);
    let cases = [((17.0, 33.0), (2, 2, Side::Left)), ((22.0, 33.0), (2, 2, Side::Right)), ((-40.0, 900.0), (0, 4, Side::Left))];
    for ((x, y), (col, row, side)) in cases { assert_eq!(m.cell_at(Point::new(x, y), GridSize::new(10, 5)), (ViewportCell { col, row }, side)) }
}
