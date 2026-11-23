//! Selectable rich text. Use the same shaped paragraph for pixels, link hit tests and selection.
mod group;

pub(super) use group::Group;
use group::{Endpoint, Gesture, Press};
use super::Message;
use iced::advanced::graphics::text::{Paragraph, cosmic_text::{BidiParagraphs, Cursor}};
use iced::advanced::text::{self, Difference, Paragraph as _, Renderer as _};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Renderer as _, Shell, Widget, layout, renderer};
use iced::widget::{markdown::Uri, text::Span};
use iced::{Element, Event, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Theme, Vector, mouse};

pub(super) fn selectable<'a>(
    spans: Vec<Span<'a, Uri>>, size: Pixels, line_height: text::LineHeight, font: Font,
) -> Element<'a, Message> {
    let group = Group::default();
    let body = group.text(spans, size, line_height, font);
    group.wrap(body)
}

struct Selectable<'a> {
    spans: Vec<Span<'a, Uri>>,
    size: Pixels,
    line_height: text::LineHeight,
    font: Font,
    block: usize,
    group: Group,
}

#[derive(Default)]
struct State {
    paragraph: Paragraph,
    spans: Vec<Span<'static, Uri>>,
    source: String,
    lines: Vec<usize>,
}

impl State {
    fn hit(&self, at: Point) -> Option<Cursor> {
        self.paragraph.buffer().hit(at.x, at.y).map(|c| Cursor::new(c.line, c.index))
    }

    fn end(&self) -> Cursor { Cursor::new(self.lines.len(), 0) }

    fn slice(&self, start: Cursor, end: Cursor) -> Option<&str> {
        let offset = |c: Cursor| self.lines.get(c.line).copied().unwrap_or(self.source.len()) + c.index;
        self.source.get(offset(start)..offset(end))
    }
}

impl Selectable<'_> {
    fn drag(&self, state: &State, at: Point, in_block: bool) {
        let mut gesture = self.group.gesture();
        let (Some(anchor), Some(mut press)) = (gesture.anchor, gesture.press) else { return };
        if !in_block && !self.group.is_single() { return; }
        let distance = at - press.at;
        press.dragged |= self.block != anchor.block || distance.x * distance.x + distance.y * distance.y >= 9.0;
        gesture.press = Some(press);
        if press.dragged {
            let caret = if at.y >= state.paragraph.min_height() { Some(state.end()) } else { state.hit(at) };
            if let Some(caret) = caret {
                gesture.end = Some(Endpoint { block: self.block, caret });
            }
        }
        self.group.set(gesture);
    }
}

impl Widget<Message, Theme, Renderer> for Selectable<'_> {
    fn tag(&self) -> tree::Tag { tree::Tag::of::<State>() }
    fn state(&self) -> tree::State { tree::State::new(State::default()) }
    fn size(&self) -> Size<Length> { Size::new(Length::Shrink, Length::Shrink) }

    fn layout(&mut self, tree: &mut Tree, _: &Renderer, limits: &layout::Limits) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();
        let spec = text::Text {
            content: self.spans.as_slice(), bounds: limits.max(), size: self.size, line_height: self.line_height,
            font: self.font, align_x: text::Alignment::Left, align_y: iced::alignment::Vertical::Top,
            wrapping: text::Wrapping::WordOrGlyph, shaping: text::Shaping::Advanced,
        };
        if state.spans != self.spans {
            let source: String = self.spans.iter().map(|s| s.text.as_ref()).collect();
            // A streamed append can extend the last span without changing an existing range.
            if !source.starts_with(&state.source) {
                self.group.set(Gesture::default());
            }
            let start = source.as_ptr() as usize;
            state.lines.clear();
            state.lines.extend(BidiParagraphs::new(&source).map(|line| line.as_ptr() as usize - start));
            state.source = source;
            state.spans = self.spans.iter().cloned().map(Span::to_static).collect();
            state.paragraph = Paragraph::with_spans(spec);
        } else {
            match state.paragraph.compare(spec.with_content(())) {
                Difference::None => {}
                Difference::Bounds => state.paragraph.resize(limits.max()),
                Difference::Shape => state.paragraph = Paragraph::with_spans(spec),
            }
        }
        layout::Node::new(limits.resolve(Length::Shrink, Length::Shrink, state.paragraph.min_bounds()))
    }

    fn operate(&mut self, _: &mut Tree, layout: Layout<'_>, _: &Renderer, operation: &mut dyn Operation) {
        for span in &self.spans {
            operation.text(None, layout.bounds(), &span.text);
        }
    }

    fn update(&mut self, tree: &mut Tree, event: &Event, layout: Layout<'_>, cursor: mouse::Cursor,
        _: &Renderer, _: &mut dyn Clipboard, shell: &mut Shell<'_, Message>, viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let inside = || cursor.position_in(bounds).filter(|_| cursor.is_over(*viewport));
        let at = || cursor.land().position_from(bounds.position());
        let in_block = |p: Point| p.y >= 0.0 && p.y < bounds.height && cursor.land().is_over(*viewport);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if !shell.is_event_captured() && let Some(at) = inside() && let Some(caret) = state.hit(at) {
                    let endpoint = Endpoint { block: self.block, caret };
                    let link = state.paragraph.hit_span(at).filter(|&i| self.spans.get(i).is_some_and(|s| s.link.is_some()));
                    self.group.set(Gesture { anchor: Some(endpoint), end: Some(endpoint), press: Some(Press { at, link, dragged: false }) });
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if self.group.gesture().press.is_some() => {
                // Scrollables translate the cursor, not the raw event coordinates.
                if let Some(at) = at() { self.drag(state, at, in_block(at)); }
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if self.group.gesture().press.is_some() => {
                if let Some(at) = at() { self.drag(state, at, in_block(at)); }
                let gesture = self.group.gesture();
                if gesture.anchor.is_some_and(|a| a.block == self.block)
                    && let Some(press) = gesture.press.filter(|p| !p.dragged)
                    && let Some(index) = press.link
                    && inside().and_then(|at| state.paragraph.hit_span(at)) == Some(index)
                    && let Some(link) = self.spans.get(index).and_then(|s| s.link.clone()) {
                    shell.publish(Message::Link(link));
                }
            }
            Event::Keyboard(_) if self.group.copying() => {
                if let Some((start, end)) = self.group.gesture().range(self.block, state.end())
                    && let Some(text) = state.slice(start, end) {
                    self.group.copy(text);
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: mouse::Cursor,
        viewport: &Rectangle, _: &Renderer,
    ) -> mouse::Interaction {
        let Some(at) = cursor.position_in(layout.bounds()).filter(|_| !cursor.is_levitating() && cursor.is_over(*viewport)) else {
            return mouse::Interaction::None;
        };
        let state = tree.state.downcast_ref::<State>();
        if state.paragraph.hit_span(at).and_then(|i| self.spans.get(i)).is_some_and(|s| s.link.is_some()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::Text
        }
    }

    fn draw(&self, tree: &Tree, renderer: &mut Renderer, theme: &Theme, style: &renderer::Style,
        layout: Layout<'_>, _: mouse::Cursor, viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) { return }
        let state = tree.state.downcast_ref::<State>();
        let origin = bounds.position() - Point::ORIGIN;
        for (i, span) in self.spans.iter().enumerate().filter(|(_, s)| s.highlight.is_some() || s.link.is_some() || s.underline || s.strikethrough) {
            for region in state.paragraph.span_bounds(i) {
                if let Some(highlight) = span.highlight {
                    let start = region.position() + origin - Vector::new(span.padding.left, span.padding.top);
                    let size = region.size() + Size::new(span.padding.x(), span.padding.y());
                    renderer.fill_quad(renderer::Quad { bounds: Rectangle::new(start, size), border: highlight.border, ..Default::default() }, highlight.background);
                }
                let size = span.size.unwrap_or(self.size).0;
                let baseline = (region.height + size) / 2.0;
                let color = span.color.unwrap_or(style.text_color);
                let position = region.position() + origin;
                let mut line = |y| renderer.fill_quad(renderer::Quad { bounds: Rectangle::new(position + Vector::new(0.0, y), Size::new(region.width, 1.0)), ..Default::default() }, color);
                if span.underline || span.link.is_some() { line(baseline); }
                if span.strikethrough { line(baseline - size / 2.5); }
            }
        }
        if let Some((start, end)) = self.group.gesture().range(self.block, state.end()) {
            let color = theme.extended_palette().primary.weak.color.scale_alpha(0.45);
            for run in state.paragraph.buffer().layout_runs().filter(|r| (start.line..=end.line).contains(&r.line_i)) {
                let from = if run.line_i == start.line { start.index } else { 0 };
                let to = if run.line_i == end.line { end.index } else { run.text.len() };
                let mut extent: Option<(f32, f32)> = None;
                let mut paint = |left: f32, right: f32| {
                    let quad = Rectangle::new(Point::new(bounds.x + left, bounds.y + run.line_top), Size::new(right - left, run.line_height));
                    renderer.fill_quad(renderer::Quad { bounds: quad, ..Default::default() }, color);
                };
                for glyph in run.glyphs {
                    let (lo, hi) = (from.max(glyph.start), to.min(glyph.end));
                    if lo >= hi || glyph.w <= 0.0 { continue; }
                    let (mut left, mut right) = (0.0, 1.0);
                    if (lo, hi) != (glyph.start, glyph.end) {
                        // A caret can sit inside a ligature rather than at a glyph edge.
                        let count = run.text[glyph.start..glyph.end].chars().count() as f32;
                        left = run.text[glyph.start..lo].chars().count() as f32 / count;
                        right = 1.0 - run.text[hi..glyph.end].chars().count() as f32 / count;
                    }
                    if glyph.level.is_rtl() { (left, right) = (1.0 - right, 1.0 - left); }
                    let (left, right) = (glyph.x + glyph.w * left, glyph.x + glyph.w * right);
                    extent = match extent {
                        Some((a, b)) if left <= b + 0.1 && right >= a - 0.1 => Some((a.min(left), b.max(right))),
                        Some((a, b)) => { paint(a, b); Some((left, right)) }
                        None => Some((left, right)),
                    };
                }
                if let Some((left, right)) = extent { paint(left, right); }
            }
        }
        renderer.fill_paragraph(&state.paragraph, bounds.position(), style.text_color, *viewport);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::Modifiers;
    use iced::widget::span;
    use iced_test::{simulator, Simulator};
    use loa_ui_kit::testkit;

    #[test]
    fn a_link_click_navigates_but_dragging_from_the_link_copies_multiline_text() {
        let url: Uri = "https://example.com".parse().unwrap();
        let view = selectable(vec![span("éclair").link(url.clone()), span("\n    second")], Pixels(20.0), text::LineHeight::Absolute(Pixels(30.0)), Font::MONOSPACE);
        let mut ui = Simulator::with_size(iced::Settings::default(), Size::new(400.0, 200.0), view);
        let start = Point::new(2.0, 15.0);
        let end = Point::new(200.0, 45.0);
        let command = if cfg!(target_os = "macos") { Modifiers::LOGO } else { Modifiers::CTRL };
        let copy = || testkit::char("c", command);

        ui.point_at(start);
        ui.simulate(simulator::click());
        ui.simulate([copy()]);
        ui.simulate([Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))]);
        ui.point_at(end);
        ui.simulate([Event::Mouse(mouse::Event::CursorMoved { position: end }), Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))]);
        ui.simulate([copy()]);
        ui.point_at(Point::new(300.0, 150.0));
        ui.simulate(simulator::click());
        ui.simulate([copy()]);

        assert_eq!(ui.into_messages().collect::<Vec<_>>(), [Message::Link(url), Message::Copy("éclair\n    second".into())]);
    }

    #[test]
    fn dragging_across_paragraphs_copies_both_blocks_in_reading_order() {
        let group = Group::default();
        let paragraph = |value| group.text(vec![span(value)], Pixels(20.0), text::LineHeight::Absolute(Pixels(30.0)), Font::MONOSPACE);
        let body = iced::widget::column![paragraph("first"), paragraph("second")].spacing(10);
        let mut ui = Simulator::with_size(iced::Settings::default(), Size::new(400.0, 200.0), group.wrap(body.into()));
        let start = Point::new(2.0, 15.0);
        let end = Point::new(200.0, 55.0);
        ui.point_at(start);
        ui.simulate([Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))]);
        ui.point_at(end);
        ui.simulate([Event::Mouse(mouse::Event::CursorMoved { position: end }), Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))]);
        let command = if cfg!(target_os = "macos") { Modifiers::LOGO } else { Modifiers::CTRL };
        ui.simulate([testkit::char("c", command)]);
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), [Message::Copy("first\n\nsecond".into())]);
    }
}
