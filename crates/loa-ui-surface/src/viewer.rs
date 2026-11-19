//! The pixel mode canvas: draws the framebuffer and the local cursor, and
//! turns iced events into [`Message`]s in panel coordinates.

use crate::input::{KeyInput, convert_button, convert_modifiers, scroll_pixels};
use crate::state::{Message, PointerInput, State};
use iced::advanced::image::FilterMethod;
use iced::widget::canvas::{self, Action, Event, Path, Stroke};
use iced::{Color, Point, Rectangle, Renderer, Theme, keyboard, mouse};

/// Borrowed view of a [`State`] as a canvas program.
pub(crate) struct Viewer<'a> {
    pub state: &'a State,
}

/// Widget local tracking of the pointer between events.
#[derive(Debug, Default)]
pub(crate) struct Tracking {
    inside: bool,
    /// Buttons pressed inside and not yet released.
    dragging: u32,
}

impl canvas::Program<Message> for Viewer<'_> {
    type State = Tracking;

    fn update(
        &self,
        tracking: &mut Tracking,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let panel = bounds.size();
        let local = |p: Point| Point::new(p.x - bounds.x, p.y - bounds.y);
        let pointer = |input| Action::publish(Message::Pointer { panel, input });
        match event {
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let over = cursor.is_over(bounds);
                let was_inside = std::mem::replace(&mut tracking.inside, over);
                if over || tracking.dragging > 0 {
                    Some(pointer(PointerInput::Moved(local(*position))))
                } else {
                    was_inside.then(|| pointer(PointerInput::Left))
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                let was_inside = std::mem::take(&mut tracking.inside);
                (was_inside && tracking.dragging == 0).then(|| pointer(PointerInput::Left))
            }
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                let button = convert_button(*button)?;
                match cursor.position_in(bounds) {
                    Some(at) => {
                        tracking.dragging += 1;
                        Some(
                            pointer(PointerInput::Button {
                                at,
                                button,
                                pressed: true,
                            })
                            .and_capture(),
                        )
                    }
                    None => self.state.focused().then(|| Action::publish(Message::Blur)),
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(button)) => {
                let button = convert_button(*button)?;
                if tracking.dragging == 0 {
                    return None;
                }
                tracking.dragging -= 1;
                let at = local(cursor.position()?);
                Some(
                    pointer(PointerInput::Button {
                        at,
                        button,
                        pressed: false,
                    })
                    .and_capture(),
                )
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let at = cursor.position_in(bounds)?;
                let (dx, dy) = scroll_pixels(*delta);
                Some(pointer(PointerInput::Scroll { at, dx, dy }).and_capture())
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => Some(Action::publish(
                Message::Modifiers(convert_modifiers(*modifiers)),
            )),
            Event::Keyboard(key) if self.state.focused() => KeyInput::from_event(key)
                .map(|key| Action::publish(Message::Key(key)).and_capture()),
            Event::Window(_) if panel != self.state.panel() => {
                Some(Action::publish(Message::Panel(panel)))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _tracking: &Tracking,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        if let (Some(handle), Some(placement)) =
            (self.state.picture(), self.state.placement_in(bounds.size()))
        {
            frame.draw_image(
                placement.rect,
                canvas::Image::new(handle.clone()).filter_method(FilterMethod::Linear),
            );
        }
        if let Some(local) = self.state.cursor()
            && local.on_surface.is_some()
        {
            let palette = theme.extended_palette();
            let ring = Path::circle(local.at, HOVER_RADIUS);
            if local.pressed {
                frame.fill(
                    &ring,
                    Color {
                        a: 0.35,
                        ..palette.primary.base.color
                    },
                );
            }
            frame.stroke(
                &ring,
                Stroke::default().with_width(2.0).with_color(Color {
                    a: 0.7,
                    ..palette.primary.base.color
                }),
            );
            let arrow = arrow(local.at);
            frame.fill(&arrow, palette.background.base.text);
            frame.stroke(
                &arrow,
                Stroke::default()
                    .with_width(1.0)
                    .with_color(palette.background.base.color),
            );
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _tracking: &Tracking,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        // The cursor is drawn by `draw`; the system one would double it.
        let drawn = self.state.cursor().is_some_and(|c| c.on_surface.is_some());
        if drawn && cursor.is_over(bounds) {
            mouse::Interaction::Hidden
        } else {
            mouse::Interaction::default()
        }
    }
}

/// Radius of the hover ring around the pointer, in panel pixels.
const HOVER_RADIUS: f32 = 12.0;

/// An arrow pointer with its tip at `tip`.
fn arrow(tip: Point) -> Path {
    const OUTLINE: [(f32, f32); 7] = [
        (0.0, 0.0),
        (0.0, 16.0),
        (4.5, 12.0),
        (7.5, 18.5),
        (10.0, 17.5),
        (7.0, 11.0),
        (12.0, 11.0),
    ];
    Path::new(|b| {
        b.move_to(tip);
        for &(x, y) in &OUTLINE[1..] {
            b.line_to(Point::new(tip.x + x, tip.y + y));
        }
        b.close();
    })
}
