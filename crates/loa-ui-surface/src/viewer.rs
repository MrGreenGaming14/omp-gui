//! The cursor overlay for pixel surfaces; also forwards iced input in panel coordinates.

use crate::keys::{KeyInput, convert_button, convert_modifiers, scroll_pixels};
use crate::tab::{Message, PixelMode, PointerInput};
use iced::widget::canvas::{self, Action, Event, Path, Stroke};
use iced::{Point, Rectangle, Renderer, Theme, keyboard, mouse};
use loa_ui_kit::theme::{AlphaRole, ColorRole as C, SpaceRole, for_theme};

/// A [`PixelMode`] as a canvas program.
pub(crate) struct Viewer<'a>(pub &'a PixelMode);

/// Widget local tracking of the pointer between events.
#[derive(Debug, Default)]
pub(crate) struct Tracking {
    inside: bool,
    /// Buttons pressed inside and not yet released.
    dragging: u32,
}

impl canvas::Program<Message> for Viewer<'_> {
    type State = Tracking;

    fn update(&self, tracking: &mut Tracking, event: &Event, bounds: Rectangle, cursor: mouse::Cursor) -> Option<Action<Message>> {
        let panel = bounds.size();
        let pointer = |input| Action::publish(Message::Pointer { panel, input });
        match event {
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let over = cursor.is_over(bounds);
                let was_inside = std::mem::replace(&mut tracking.inside, over);
                if over || tracking.dragging > 0 {
                    Some(pointer(PointerInput::Moved(Point::new(position.x - bounds.x, position.y - bounds.y))))
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
                        Some(pointer(PointerInput::Button { at, button, pressed: true }).and_capture())
                    }
                    None => self.0.focused().then(|| Action::publish(Message::Blur)),
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(button)) => {
                let button = convert_button(*button)?;
                tracking.dragging = tracking.dragging.checked_sub(1)?;
                let at = cursor.position()?;
                let at = Point::new(at.x - bounds.x, at.y - bounds.y);
                Some(pointer(PointerInput::Button { at, button, pressed: false }).and_capture())
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let at = cursor.position_in(bounds)?;
                let (dx, dy) = scroll_pixels(*delta);
                Some(pointer(PointerInput::Scroll { at, dx, dy }).and_capture())
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => Some(Action::publish(Message::Modifiers(convert_modifiers(*modifiers)))),
            Event::Keyboard(key) if self.0.focused() => KeyInput::from_event(key).map(|key| Action::publish(Message::Key(key)).and_capture()),
            Event::Window(_) if panel != self.0.panel() => Some(Action::publish(Message::Panel(panel))),
            _ => None,
        }
    }

    fn draw(&self, _: &Tracking, renderer: &Renderer, theme: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        if let Some(local) = self.0.cursor().filter(|c| c.on_surface.is_some()) {
            // A ring the size of the medium space around the tip, filled while a button is held.
            let t = for_theme(theme);
            let (ring_color, line) = (t.color(C::CursorRing), t.space(SpaceRole::Xs) / 2.0);
            let ring = Path::circle(local.at, t.space(SpaceRole::M));
            if local.pressed {
                frame.fill(&ring, ring_color.scale_alpha(t.alpha(AlphaRole::Tint)));
            }
            frame.stroke(&ring, Stroke::default().with_width(line).with_color(ring_color.scale_alpha(t.alpha(AlphaRole::Muted))));
            let arrow = arrow(local.at);
            frame.fill(&arrow, t.color(C::Text));
            frame.stroke(&arrow, Stroke::default().with_width(line / 2.0).with_color(t.color(C::Ground)));
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(&self, _: &Tracking, bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        // The cursor is drawn by `draw`; the system one would double it.
        let drawn = self.0.cursor().is_some_and(|c| c.on_surface.is_some());
        if drawn && cursor.is_over(bounds) { mouse::Interaction::Hidden } else { mouse::Interaction::default() }
    }
}

/// An arrow pointer with its tip at `tip`.
fn arrow(tip: Point) -> Path {
    const OUTLINE: [(f32, f32); 6] = [(0.0, 16.0), (4.5, 12.0), (7.5, 18.5), (10.0, 17.5), (7.0, 11.0), (12.0, 11.0)];
    Path::new(|b| {
        b.move_to(tip);
        for (x, y) in OUTLINE {
            b.line_to(Point::new(tip.x + x, tip.y + y));
        }
        b.close();
    })
}
