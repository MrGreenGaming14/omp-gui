//! The terminal grid on an iced canvas, drawn in a background, a decoration and a text pass
//! over the resolved cells, and the input it captures.

use crate::GridSize;
use crate::emulator::{Emulator, ViewportCell};
use crate::palette::TermPalette;
use crate::state::{Message, TabId};
use alacritty_terminal::index::Side;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::CursorShape;
use iced::advanced::graphics::text::Paragraph;
use iced::advanced::text::{self, Paragraph as _};
use iced::keyboard::{self, Key, Modifiers};
use iced::mouse::{self, ScrollDelta};
use iced::theme::palette::mix;
use iced::widget::canvas::{self, Action, Cache, Event, Frame, Geometry, Stroke};
use iced::{Color, Font, Pixels, Point, Rectangle, Renderer, Size, Theme, alignment};
use std::cell::Cell as StdCell;
use std::time::{Duration, Instant};

/// Two clicks on one cell closer than this are a double click.
const MULTI_CLICK: Duration = Duration::from_millis(400);
const LINE_HEIGHT: f32 = 1.25;

/// Font and cell geometry of the grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Should be monospaced.
    pub font: Font,
    pub font_size: f32,
    pub cell: Size,
}

impl Metrics {
    /// Measures `font`'s cell at `font_size` with the text renderer.
    pub fn measure(font: Font, font_size: f32) -> Self {
        const SAMPLE: &str = "MMMMMMMMMMMMMMMMMMMM";
        let (align_x, align_y) = (text::Alignment::Left, alignment::Vertical::Top);
        let line_height = text::LineHeight::Relative(LINE_HEIGHT);
        let p = Paragraph::with_text(text::Text { content: SAMPLE, bounds: Size::INFINITE, size: Pixels(font_size), line_height, font, align_x, align_y, shaping: text::Shaping::Basic, wrapping: text::Wrapping::None });
        let b = p.min_bounds();
        Self { font, font_size, cell: Size::new(b.width / SAMPLE.len() as f32, b.height) }
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
}

/// What the canvas reports to the terminal state.
#[derive(Debug, Clone, PartialEq)]
pub enum CanvasEvent {
    Resized(GridSize),
    Key { key: Key, modifiers: Modifiers, text: Option<String> },
    /// The wheel moved this many lines; positive is up.
    Scroll(i32),
    /// Simple, semantic (double click) or lines (triple click) selection.
    SelectStart { cell: ViewportCell, side: Side, kind: SelectionType },
    SelectUpdate { cell: ViewportCell, side: Side },
    SelectEnd,
}

#[derive(Debug, Default)]
pub struct CanvasState {
    focused: bool,
    selecting: bool,
    grid: Option<GridSize>,
    last_click: Option<(Instant, ViewportCell, u8)>,
    scroll_remainder: f32,
}

/// One terminal, drawn and turned into [`Message`]s.
pub struct TerminalCanvas<'a> {
    pub(crate) tab: TabId,
    pub(crate) emulator: &'a Emulator,
    pub(crate) cache: &'a Cache,
    pub(crate) drawn_with: &'a StdCell<Option<TermPalette>>,
    pub(crate) metrics: Metrics,
}

/// A visible cell with its colours resolved (inverse, selection and dim applied).
struct Painted<'a> {
    row: usize,
    col: usize,
    width: usize,
    fg: Color,
    bg: Color,
    flags: Flags,
    c: char,
    zerowidth: Option<&'a [char]>,
}

impl<'a> TerminalCanvas<'a> {
    fn publish(&self, e: CanvasEvent) -> Action<Message> { Action::publish(Message::Canvas(self.tab, e)) }

    fn cells(&self, palette: &TermPalette) -> Vec<Painted<'a>> {
        let content = self.emulator.term().renderable_content();
        let (offset, colors, selection) = (content.display_offset as i32, content.colors, content.selection);
        let spacers = Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER;
        content.display_iter.filter(|i| !i.cell.flags.intersects(spacers)).map(|i| {
            let (mut fg, mut bg, flags) = (palette.resolve(i.cell.fg, colors), palette.resolve(i.cell.bg, colors), i.cell.flags);
            if flags.contains(Flags::INVERSE) { std::mem::swap(&mut fg, &mut bg) }
            if selection.is_some_and(|r| r.contains(i.point)) { (fg, bg) = (palette.selection_text, palette.selection) }
            if flags.contains(Flags::DIM) { fg = mix(fg, bg, 0.33) }
            let width = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            Painted { row: (i.point.line.0 + offset) as usize, col: i.point.column.0, width, fg, bg, flags, c: i.cell.c, zerowidth: i.cell.zerowidth() }
        }).collect()
    }

    /// Runs of non-default backgrounds per row.
    fn backgrounds(&self, frame: &mut Frame, cells: &[Painted], palette: &TermPalette) {
        let mut runs: Vec<(usize, usize, usize, Color)> = Vec::new();
        for c in cells.iter().filter(|c| c.bg != palette.background) {
            match runs.last_mut() {
                Some((row, _, end, color)) if (*row, *end, *color) == (c.row, c.col, c.bg) => *end = c.col + c.width,
                _ => runs.push((c.row, c.col, c.col + c.width, c.bg)),
            }
        }
        for (row, start, end, color) in runs { frame.fill_rectangle(self.metrics.origin(row, start), Size::new((end - start) as f32 * self.metrics.cell.width, self.metrics.cell.height), color) }
    }

    /// Underlines and strikeouts.
    fn decorations(&self, frame: &mut Frame, cells: &[Painted]) {
        let (thickness, ch) = ((self.metrics.font_size / 14.0).max(1.0), self.metrics.cell.height);
        for c in cells {
            let ys = [(c.flags.intersects(Flags::ALL_UNDERLINES), ch - thickness), (c.flags.contains(Flags::STRIKEOUT), ch * 0.5)];
            for (_, y) in ys.into_iter().filter(|(on, _)| *on) {
                let at = self.metrics.origin(c.row, c.col);
                frame.fill_rectangle(Point::new(at.x, at.y + y), Size::new(c.width as f32 * self.metrics.cell.width, thickness), c.fg);
            }
        }
    }

    /// Text in runs of one colour and style; blanks inside a run keep it going.
    fn text_runs(&self, frame: &mut Frame, cells: &[Painted]) {
        struct Run { row: usize, start: usize, end: usize, color: Color, style: (bool, bool), single: bool, text: String }
        let mut runs: Vec<Run> = Vec::new();
        for c in cells {
            let continues = |r: &Run| r.row == c.row && r.end == c.col && r.single;
            if c.c == ' ' || c.c == '\t' || c.flags.contains(Flags::HIDDEN) {
                if let Some(r) = runs.last_mut().filter(|r| continues(r)) {
                    r.text.push(' ');
                    r.end = c.col + c.width;
                }
                continue;
            }
            let style = (c.flags.contains(Flags::BOLD), c.flags.contains(Flags::ITALIC));
            if !(c.width == 1 && runs.last().is_some_and(|r| continues(r) && r.color == c.fg && r.style == style)) {
                runs.push(Run { row: c.row, start: c.col, end: c.col, color: c.fg, style, single: c.width == 1, text: String::new() });
            }
            if let Some(r) = runs.last_mut() {
                r.text.push(c.c);
                r.text.extend(c.zerowidth.into_iter().flatten());
                r.end = c.col + c.width;
            }
        }
        for mut r in runs {
            r.text.truncate(r.text.trim_end().len());
            frame.fill_text(self.text(r.text, r.row, r.start, r.color, r.style));
        }
    }

    fn text(&self, content: String, row: usize, col: usize, color: Color, (bold, italic): (bool, bool)) -> canvas::Text {
        let mut font = self.metrics.font;
        if bold { font.weight = iced::font::Weight::Bold }
        if italic { font.style = iced::font::Style::Italic }
        let shaping = if content.is_ascii() { text::Shaping::Basic } else { text::Shaping::Advanced };
        let line_height = text::LineHeight::Absolute(Pixels(self.metrics.cell.height));
        canvas::Text { content, position: self.metrics.origin(row, col), color, size: Pixels(self.metrics.font_size), line_height, font, shaping, ..canvas::Text::default() }
    }

    fn draw_cursor(&self, frame: &mut Frame, palette: &TermPalette, focused: bool) {
        let term = self.emulator.term();
        let content = term.renderable_content();
        let (shape, point) = (content.cursor.shape, content.cursor.point);
        let row = point.line.0 + content.display_offset as i32;
        if shape == CursorShape::Hidden || !(0..i32::from(self.emulator.size().rows.max(1))).contains(&row) { return }
        let cell = &term.grid()[point];
        let (cw, ch, thickness) = (self.metrics.cell.width, self.metrics.cell.height, (self.metrics.font_size / 8.0).max(1.0));
        let origin = self.metrics.origin(row as usize, point.column.0);
        let size = Size::new(cw * if cell.flags.contains(Flags::WIDE_CHAR) { 2.0 } else { 1.0 }, ch);
        match shape {
            CursorShape::Block if focused => {
                frame.fill_rectangle(origin, size, palette.cursor);
                if cell.c != ' ' { frame.fill_text(self.text(cell.c.to_string(), row as usize, point.column.0, palette.background, (cell.flags.contains(Flags::BOLD), cell.flags.contains(Flags::ITALIC)))) }
            }
            CursorShape::Beam => frame.fill_rectangle(origin, Size::new(thickness, ch), palette.cursor),
            CursorShape::Underline => frame.fill_rectangle(Point::new(origin.x, origin.y + ch - thickness), Size::new(size.width, thickness), palette.cursor),
            _ => frame.stroke_rectangle(origin, size, Stroke::default().with_color(palette.cursor).with_width(thickness / 2.0)),
        }
    }

    /// A mouse or key event the terminal handles, as a message.
    fn input(&self, state: &mut CanvasState, event: &Event, bounds: Rectangle, cursor: mouse::Cursor, grid: GridSize) -> Option<Action<Message>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(position) = cursor.position_in(bounds) else {
                    state.focused = false;
                    return None;
                };
                (state.focused, state.selecting) = (true, true);
                let (cell, side) = self.metrics.cell_at(position, grid);
                let now = Instant::now();
                let clicks = match state.last_click {
                    Some((at, last, n)) if last == cell && now - at < MULTI_CLICK => n % 3 + 1,
                    _ => 1,
                };
                state.last_click = Some((now, cell, clicks));
                let kind = [SelectionType::Simple, SelectionType::Semantic, SelectionType::Lines][usize::from(clicks - 1)];
                Some(self.publish(CanvasEvent::SelectStart { cell, side, kind }).and_capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.selecting => {
                let (cell, side) = self.metrics.cell_at(cursor.position_from(bounds.position())?, grid);
                Some(self.publish(CanvasEvent::SelectUpdate { cell, side }))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.selecting => {
                state.selecting = false;
                Some(self.publish(CanvasEvent::SelectEnd))
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) => {
                let lines = match *delta {
                    ScrollDelta::Lines { y, .. } => y * 3.0,
                    ScrollDelta::Pixels { y, .. } => y / self.metrics.cell.height.max(1.0),
                } + state.scroll_remainder;
                state.scroll_remainder = lines.fract();
                Some(if lines.trunc() == 0.0 { Action::capture() } else { self.publish(CanvasEvent::Scroll(lines.trunc() as i32)).and_capture() })
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, text, .. }) if state.focused => {
                Some(self.publish(CanvasEvent::Key { key: key.clone(), modifiers: *modifiers, text: text.as_ref().map(ToString::to_string) }).and_capture())
            }
            _ => None,
        }
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
mod tests {
    use super::*;

    #[test]
    fn grid_and_pointer_geometry() {
        let m = Metrics { font: Font::MONOSPACE, font_size: 14.0, cell: Size::new(8.0, 16.0) };
        let g = m.grid_for(Size::new(803.0, 410.0));
        assert_eq!((g.cols, g.rows, g.cell_width, g.cell_height), (100, 25, 8, 16));
        assert_eq!((m.grid_for(Size::new(3.0, 3.0)).cols, m.grid_for(Size::new(3.0, 3.0)).rows), (2, 1));
        let grid = GridSize::new(10, 5);
        let cases = [((17.0, 33.0), (2, 2, Side::Left)), ((22.0, 33.0), (2, 2, Side::Right)), ((-40.0, 900.0), (0, 4, Side::Left))];
        for ((x, y), (col, row, side)) in cases { assert_eq!(m.cell_at(Point::new(x, y), grid), (ViewportCell { col, row }, side)) }
        let measured = Metrics::measure(Font::MONOSPACE, 14.0);
        assert!(measured.cell.width > 1.0 && measured.cell.height > 10.0, "{measured:?}");
    }
}
