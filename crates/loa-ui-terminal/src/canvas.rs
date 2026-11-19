//! The terminal grid drawn on an iced canvas, and the input it captures.

use std::cell::Cell as StdCell;
use std::time::{Duration, Instant};

use alacritty_terminal::index::{Point as GridPoint, Side};
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

use crate::GridSize;
use crate::emulator::{Emulator, ViewportCell};
use crate::palette::TermPalette;
use crate::state::{Message, TabId};

/// Two clicks closer than this on the same cell are a double click.
const MULTI_CLICK: Duration = Duration::from_millis(400);
/// Line height relative to the font size.
const LINE_HEIGHT: f32 = 1.25;

/// Font and cell geometry of the terminal grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Font for terminal text; should be monospaced.
    pub font: Font,
    /// Font size in logical pixels.
    pub font_size: f32,
    /// Size of one character cell in logical pixels.
    pub cell: Size,
}

impl Metrics {
    /// Measures the cell size of `font` at `font_size` with the text
    /// renderer's font system.
    pub fn measure(font: Font, font_size: f32) -> Self {
        const SAMPLE: &str = "MMMMMMMMMMMMMMMMMMMM";
        let paragraph = Paragraph::with_text(text::Text {
            content: SAMPLE,
            bounds: Size::INFINITE,
            size: Pixels(font_size),
            line_height: text::LineHeight::Relative(LINE_HEIGHT),
            font,
            align_x: text::Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: text::Shaping::Basic,
            wrapping: text::Wrapping::None,
        });
        let bounds = paragraph.min_bounds();
        Self {
            font,
            font_size,
            cell: Size::new(bounds.width / SAMPLE.len() as f32, bounds.height),
        }
    }

    /// The grid that fits in `area`, at least 2 columns by 1 row.
    pub fn grid_for(&self, area: Size) -> GridSize {
        let fit = |space: f32, cell: f32| -> u16 {
            if cell <= 0.0 {
                return 0;
            }
            (space / cell).floor().clamp(0.0, f32::from(u16::MAX)) as u16
        };
        GridSize {
            cols: fit(area.width, self.cell.width).max(2),
            rows: fit(area.height, self.cell.height).max(1),
            cell_width: self.cell.width.round() as u16,
            cell_height: self.cell.height.round() as u16,
        }
    }

    /// The cell under a position relative to the canvas, and which half of
    /// it the position is in. Positions outside the canvas clamp to the edge.
    pub fn cell_at(&self, position: Point, grid: GridSize) -> (ViewportCell, Side) {
        let x = (position.x / self.cell.width).max(0.0);
        let y = (position.y / self.cell.height).max(0.0);
        let col = (x as usize).min(usize::from(grid.cols.saturating_sub(1)));
        let row = (y as usize).min(usize::from(grid.rows.saturating_sub(1)));
        let side = if x.fract() > 0.5 { Side::Right } else { Side::Left };
        (ViewportCell { col, row }, side)
    }
}

/// What the canvas reports to [`crate::State`].
#[derive(Debug, Clone, PartialEq)]
pub enum CanvasEvent {
    /// The canvas was laid out at a new grid size.
    Resized(GridSize),
    /// A key was pressed while the terminal had focus.
    Key {
        /// Logical key.
        key: Key,
        /// Modifiers held.
        modifiers: Modifiers,
        /// Text the key produced, if any.
        text: Option<String>,
    },
    /// The mouse wheel moved by this many lines; positive is up.
    Scroll(i32),
    /// A mouse press started a selection.
    SelectStart {
        /// Cell under the pointer.
        cell: ViewportCell,
        /// Half of the cell under the pointer.
        side: Side,
        /// Simple, semantic (double click) or lines (triple click).
        kind: SelectionType,
    },
    /// The pointer moved while selecting.
    SelectUpdate {
        /// Cell under the pointer.
        cell: ViewportCell,
        /// Half of the cell under the pointer.
        side: Side,
    },
    /// The mouse button was released.
    SelectEnd,
}

/// Input state the canvas keeps between events.
#[derive(Debug, Default)]
pub struct CanvasState {
    focused: bool,
    selecting: bool,
    grid: Option<GridSize>,
    last_click: Option<(Instant, ViewportCell, u8)>,
    scroll_remainder: f32,
}

/// Draws one terminal and turns canvas events into [`Message`]s.
pub struct TerminalCanvas<'a> {
    pub(crate) tab: TabId,
    pub(crate) emulator: &'a Emulator,
    pub(crate) cache: &'a Cache,
    pub(crate) drawn_with: &'a StdCell<Option<TermPalette>>,
    pub(crate) metrics: Metrics,
}

impl TerminalCanvas<'_> {
    fn publish(&self, event: CanvasEvent) -> Action<Message> {
        Action::publish(Message::Canvas(self.tab, event))
    }

    fn draw_content(&self, frame: &mut Frame, palette: &TermPalette) {
        let size = frame.size();
        frame.fill_rectangle(Point::ORIGIN, size, palette.background);

        let cw = self.metrics.cell.width;
        let ch = self.metrics.cell.height;
        let content = self.emulator.term().renderable_content();
        let offset = content.display_offset as i32;
        let colors = content.colors;
        let selection = content.selection;
        let thickness = (self.metrics.font_size / 14.0).max(1.0);

        let mut backgrounds: Vec<(usize, usize, usize, Color)> = Vec::new();
        let mut decorations: Vec<(usize, usize, usize, Color, f32)> = Vec::new();
        let mut runs: Vec<Run> = Vec::new();

        for indexed in content.display_iter {
            let cell = indexed.cell;
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let row = (indexed.point.line.0 + offset) as usize;
            let col = indexed.point.column.0;
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };

            let mut fg = palette.resolve(cell.fg, colors);
            let mut bg = palette.resolve(cell.bg, colors);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if selection.is_some_and(|range| range.contains(indexed.point)) {
                fg = palette.selection_text;
                bg = palette.selection;
            }
            if cell.flags.contains(Flags::DIM) {
                fg = mix(fg, bg, 0.33);
            }

            if bg != palette.background {
                match backgrounds.last_mut() {
                    Some((r, _, end, color)) if *r == row && *end == col && *color == bg => {
                        *end = col + width;
                    }
                    _ => backgrounds.push((row, col, col + width, bg)),
                }
            }
            if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                decorations.push((row, col, width, fg, ch - thickness));
            }
            if cell.flags.contains(Flags::STRIKEOUT) {
                decorations.push((row, col, width, fg, ch * 0.5));
            }
            let continues = |run: &Run| run.row == row && run.end == col && run.single_width;
            if cell.c == ' ' || cell.c == '\t' || cell.flags.contains(Flags::HIDDEN) {
                // Blanks inside a run keep it going instead of splitting it.
                if let Some(run) = runs.last_mut().filter(|run| continues(run)) {
                    run.text.push(' ');
                    run.end = col + width;
                }
                continue;
            }
            let style = (
                cell.flags.contains(Flags::BOLD),
                cell.flags.contains(Flags::ITALIC),
            );
            let extends = width == 1
                && runs
                    .last()
                    .is_some_and(|run| continues(run) && run.color == fg && run.style == style);
            if !extends {
                runs.push(Run {
                    row,
                    start: col,
                    end: col,
                    color: fg,
                    style,
                    single_width: width == 1,
                    text: String::new(),
                });
            }
            if let Some(run) = runs.last_mut() {
                run.text.push(cell.c);
                if let Some(zerowidth) = cell.zerowidth() {
                    run.text.extend(zerowidth);
                }
                run.end = col + width;
            }
        }

        for (row, start, end, color) in backgrounds {
            frame.fill_rectangle(
                Point::new(start as f32 * cw, row as f32 * ch),
                Size::new((end - start) as f32 * cw, ch),
                color,
            );
        }
        for (row, col, width, color, y) in decorations {
            frame.fill_rectangle(
                Point::new(col as f32 * cw, row as f32 * ch + y),
                Size::new(width as f32 * cw, thickness),
                color,
            );
        }
        for mut run in runs {
            run.text.truncate(run.text.trim_end().len());
            let font = self.font(run.style);
            frame.fill_text(self.text(run.text, run.row, run.start, run.color, font));
        }
    }

    fn font(&self, (bold, italic): (bool, bool)) -> Font {
        let mut font = self.metrics.font;
        if bold {
            font.weight = iced::font::Weight::Bold;
        }
        if italic {
            font.style = iced::font::Style::Italic;
        }
        font
    }

    fn text(&self, content: String, row: usize, col: usize, color: Color, font: Font) -> canvas::Text {
        let shaping = if content.is_ascii() {
            text::Shaping::Basic
        } else {
            text::Shaping::Advanced
        };
        canvas::Text {
            content,
            position: Point::new(
                col as f32 * self.metrics.cell.width,
                row as f32 * self.metrics.cell.height,
            ),
            color,
            size: Pixels(self.metrics.font_size),
            line_height: text::LineHeight::Absolute(Pixels(self.metrics.cell.height)),
            font,
            shaping,
            ..canvas::Text::default()
        }
    }

    fn draw_cursor(&self, frame: &mut Frame, palette: &TermPalette, focused: bool) {
        let term = self.emulator.term();
        let content = term.renderable_content();
        let shape = content.cursor.shape;
        if shape == CursorShape::Hidden {
            return;
        }
        let point: GridPoint = content.cursor.point;
        let row = point.line.0 + content.display_offset as i32;
        let screen_rows = i32::from(self.emulator.size().rows.max(1));
        if !(0..screen_rows).contains(&row) {
            return;
        }
        let cell = &term.grid()[point];
        let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2.0 } else { 1.0 };
        let cw = self.metrics.cell.width;
        let ch = self.metrics.cell.height;
        let origin = Point::new(point.column.0 as f32 * cw, row as f32 * ch);
        let size = Size::new(cw * width, ch);
        let thickness = (self.metrics.font_size / 8.0).max(1.0);
        match shape {
            CursorShape::Block if focused => {
                frame.fill_rectangle(origin, size, palette.cursor);
                if cell.c != ' ' {
                    let font = self.font((
                        cell.flags.contains(Flags::BOLD),
                        cell.flags.contains(Flags::ITALIC),
                    ));
                    frame.fill_text(self.text(
                        cell.c.to_string(),
                        row as usize,
                        point.column.0,
                        palette.background,
                        font,
                    ));
                }
            }
            CursorShape::Beam => {
                frame.fill_rectangle(origin, Size::new(thickness, ch), palette.cursor);
            }
            CursorShape::Underline => frame.fill_rectangle(
                Point::new(origin.x, origin.y + ch - thickness),
                Size::new(size.width, thickness),
                palette.cursor,
            ),
            _ => frame.stroke_rectangle(
                origin,
                size,
                Stroke::default()
                    .with_color(palette.cursor)
                    .with_width(thickness / 2.0),
            ),
        }
    }
}

struct Run {
    row: usize,
    start: usize,
    end: usize,
    color: Color,
    style: (bool, bool),
    single_width: bool,
    text: String,
}

impl canvas::Program<Message> for TerminalCanvas<'_> {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut CanvasState,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let grid = self.metrics.grid_for(bounds.size());
        if let Some(action) = self.input(state, event, bounds, cursor, grid) {
            return Some(action);
        }
        // A canvas action carries one message, so a size change is reported
        // on the next event that is not input.
        if state.grid != Some(grid) {
            state.grid = Some(grid);
            return Some(self.publish(CanvasEvent::Resized(grid)));
        }
        None
    }

    fn draw(
        &self,
        state: &CanvasState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let palette = TermPalette::from_theme(theme);
        if self.drawn_with.get() != Some(palette) {
            self.cache.clear();
            self.drawn_with.set(Some(palette));
        }
        let content = self.cache.draw(renderer, bounds.size(), |frame| {
            self.draw_content(frame, &palette);
        });
        let mut overlay = Frame::new(renderer, bounds.size());
        self.draw_cursor(&mut overlay, &palette, state.focused);
        vec![content, overlay.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &CanvasState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::default()
        }
    }
}

impl TerminalCanvas<'_> {
    /// Turns a mouse or keyboard event into a message, if it is one the
    /// terminal handles.
    fn input(
        &self,
        state: &mut CanvasState,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        grid: GridSize,
    ) -> Option<Action<Message>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(position) = cursor.position_in(bounds) else {
                    state.focused = false;
                    return None;
                };
                state.focused = true;
                state.selecting = true;
                let (cell, side) = self.metrics.cell_at(position, grid);
                let now = Instant::now();
                let clicks = match state.last_click {
                    Some((at, last, count)) if last == cell && now - at < MULTI_CLICK => {
                        count % 3 + 1
                    }
                    _ => 1,
                };
                state.last_click = Some((now, cell, clicks));
                let kind = match clicks {
                    1 => SelectionType::Simple,
                    2 => SelectionType::Semantic,
                    _ => SelectionType::Lines,
                };
                Some(
                    self.publish(CanvasEvent::SelectStart { cell, side, kind })
                        .and_capture(),
                )
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.selecting => {
                let position = cursor.position_from(bounds.position())?;
                let (cell, side) = self.metrics.cell_at(position, grid);
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
                let whole = lines.trunc();
                state.scroll_remainder = lines - whole;
                if whole == 0.0 {
                    return Some(Action::capture());
                }
                Some(self.publish(CanvasEvent::Scroll(whole as i32)).and_capture())
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                text,
                ..
            }) if state.focused => Some(
                self.publish(CanvasEvent::Key {
                    key: key.clone(),
                    modifiers: *modifiers,
                    text: text.as_ref().map(ToString::to_string),
                })
                .and_capture(),
            ),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics {
            font: Font::MONOSPACE,
            font_size: 14.0,
            cell: Size::new(8.0, 16.0),
        }
    }

    #[test]
    fn grid_fills_the_pane_with_whole_cells() {
        let grid = metrics().grid_for(Size::new(803.0, 410.0));
        assert_eq!((grid.cols, grid.rows), (100, 25));
        assert_eq!((grid.cell_width, grid.cell_height), (8, 16));
        let tiny = metrics().grid_for(Size::new(3.0, 3.0));
        assert_eq!((tiny.cols, tiny.rows), (2, 1));
    }

    #[test]
    fn pointer_maps_to_cell_and_side_and_clamps_outside() {
        let m = metrics();
        let grid = GridSize::new(10, 5);
        let (cell, side) = m.cell_at(Point::new(17.0, 33.0), grid);
        assert_eq!(cell, ViewportCell { col: 2, row: 2 });
        assert_eq!(side, Side::Left);
        let (_, side) = m.cell_at(Point::new(22.0, 33.0), grid);
        assert_eq!(side, Side::Right);
        let (cell, _) = m.cell_at(Point::new(-40.0, 900.0), grid);
        assert_eq!(cell, ViewportCell { col: 0, row: 4 });
    }

    #[test]
    fn measured_cells_are_positive() {
        let m = Metrics::measure(Font::MONOSPACE, 14.0);
        assert!(m.cell.width > 1.0 && m.cell.height > 10.0, "{m:?}");
    }
}
