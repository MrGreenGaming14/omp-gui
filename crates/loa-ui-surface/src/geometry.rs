//! Mapping between panel coordinates and surface coordinates.
//!
//! The surface is scaled to fit the panel with its aspect ratio kept and
//! centered, so bars appear on two sides (letterboxing). Input is forwarded in
//! surface pixels, the cursor is drawn in panel pixels.

use iced::{Point, Rectangle, Size};

/// Where a surface of a given size sits inside a panel.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Placement {
    /// The drawn picture, in panel coordinates.
    pub rect: Rectangle,
    /// Panel pixels per surface pixel.
    pub scale: f32,
    surface: Size,
}

/// Fits a `width` by `height` surface into `panel`, keeping its aspect ratio.
/// `None` when either side is empty.
pub fn fit(width: u32, height: u32, panel: Size) -> Option<Placement> {
    let surface = Size::new(width as f32, height as f32);
    if surface.width <= 0.0 || surface.height <= 0.0 || panel.width <= 0.0 || panel.height <= 0.0 {
        return None;
    }
    let scale = (panel.width / surface.width).min(panel.height / surface.height);
    let drawn = Size::new(surface.width * scale, surface.height * scale);
    let origin = Point::new(
        (panel.width - drawn.width) / 2.0,
        (panel.height - drawn.height) / 2.0,
    );
    Some(Placement {
        rect: Rectangle::new(origin, drawn),
        scale,
        surface,
    })
}

impl Placement {
    /// The surface point under panel point `at`, `None` when `at` is on a bar
    /// or outside the panel.
    pub fn to_surface(&self, at: Point) -> Option<Point> {
        let p = self.unclamped(at);
        let inside =
            p.x >= 0.0 && p.y >= 0.0 && p.x < self.surface.width && p.y < self.surface.height;
        inside.then_some(p)
    }

    /// Like [`Placement::to_surface`] but pinned to the surface edge, for drags
    /// that leave the picture with a button held.
    pub fn to_surface_clamped(&self, at: Point) -> Point {
        let p = self.unclamped(at);
        Point::new(
            p.x.clamp(0.0, self.surface.width.next_down()),
            p.y.clamp(0.0, self.surface.height.next_down()),
        )
    }

    /// The panel point showing surface point `at`.
    pub fn to_panel(&self, at: Point) -> Point {
        Point::new(
            self.rect.x + at.x * self.scale,
            self.rect.y + at.y * self.scale,
        )
    }

    /// A length in panel pixels expressed in surface pixels.
    pub fn to_surface_len(&self, len: f32) -> f32 {
        len / self.scale
    }

    fn unclamped(&self, at: Point) -> Point {
        Point::new(
            (at.x - self.rect.x) / self.scale,
            (at.y - self.rect.y) / self.scale,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_panel_puts_bars_left_and_right() {
        // 800x600 surface in a 1000x300 panel: height limits, scale 0.5.
        let p = fit(800, 600, Size::new(1000.0, 300.0)).unwrap();
        assert_eq!(p.scale, 0.5);
        assert_eq!(
            p.rect,
            Rectangle::new(Point::new(300.0, 0.0), Size::new(400.0, 300.0))
        );
        assert_eq!(p.to_surface(Point::new(300.0, 0.0)), Some(Point::ORIGIN));
        assert_eq!(
            p.to_surface(Point::new(500.0, 150.0)),
            Some(Point::new(400.0, 300.0))
        );
        // Left bar and right bar map to nothing.
        assert_eq!(p.to_surface(Point::new(299.0, 150.0)), None);
        assert_eq!(p.to_surface(Point::new(700.0, 150.0)), None);
    }

    #[test]
    fn tall_panel_puts_bars_top_and_bottom_and_upscales() {
        // 100x50 surface in a 400x600 panel: width limits, scale 4.
        let p = fit(100, 50, Size::new(400.0, 600.0)).unwrap();
        assert_eq!(p.scale, 4.0);
        assert_eq!(
            p.rect,
            Rectangle::new(Point::new(0.0, 200.0), Size::new(400.0, 200.0))
        );
        assert_eq!(
            p.to_surface(Point::new(40.0, 240.0)),
            Some(Point::new(10.0, 10.0))
        );
        assert_eq!(p.to_surface(Point::new(40.0, 100.0)), None);
        assert_eq!(p.to_surface(Point::new(40.0, 400.0)), None);
    }

    #[test]
    fn panel_and_surface_points_round_trip() {
        let p = fit(1280, 720, Size::new(613.0, 911.0)).unwrap();
        for s in [
            Point::new(0.0, 0.0),
            Point::new(640.5, 360.25),
            Point::new(1279.0, 719.0),
        ] {
            let back = p.to_surface(p.to_panel(s)).unwrap();
            assert!(
                (back.x - s.x).abs() < 1e-3 && (back.y - s.y).abs() < 1e-3,
                "{s:?} -> {back:?}"
            );
        }
    }

    #[test]
    fn drags_outside_the_picture_pin_to_the_edge() {
        let p = fit(800, 600, Size::new(1000.0, 300.0)).unwrap();
        let left = p.to_surface_clamped(Point::new(0.0, 150.0));
        assert_eq!(left, Point::new(0.0, 300.0));
        let right = p.to_surface_clamped(Point::new(1000.0, -50.0));
        assert!(
            right.x < 800.0 && right.x > 799.9 && right.y == 0.0,
            "{right:?}"
        );
    }

    #[test]
    fn empty_sides_have_no_placement() {
        assert_eq!(fit(0, 10, Size::new(10.0, 10.0)), None);
        assert_eq!(fit(10, 10, Size::new(0.0, 10.0)), None);
    }
}
