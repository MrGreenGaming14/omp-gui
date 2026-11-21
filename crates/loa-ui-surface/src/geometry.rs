//! Mapping between panel and surface coordinates. The surface is scaled to fit the panel with
//! its aspect ratio kept and centred (letterboxing); input goes out in surface pixels.

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

/// Fits a `width` by `height` surface into `panel`; `None` when either side is empty.
pub fn fit(width: u32, height: u32, panel: Size) -> Option<Placement> {
    let surface = Size::new(width as f32, height as f32);
    if surface.width <= 0.0 || surface.height <= 0.0 || panel.width <= 0.0 || panel.height <= 0.0 {
        return None;
    }
    let scale = (panel.width / surface.width).min(panel.height / surface.height);
    let drawn = Size::new(surface.width * scale, surface.height * scale);
    let origin = Point::new((panel.width - drawn.width) / 2.0, (panel.height - drawn.height) / 2.0);
    Some(Placement { rect: Rectangle::new(origin, drawn), scale, surface })
}

impl Placement {
    /// The surface point under panel point `at`, `None` on a bar or outside the panel.
    pub fn to_surface(&self, at: Point) -> Option<Point> {
        let p = self.unclamped(at);
        (p.x >= 0.0 && p.y >= 0.0 && p.x < self.surface.width && p.y < self.surface.height).then_some(p)
    }

    /// Like [`Placement::to_surface`] but pinned to the surface edge, for drags that leave the
    /// picture with a button held.
    pub fn to_surface_clamped(&self, at: Point) -> Point {
        let p = self.unclamped(at);
        Point::new(p.x.clamp(0.0, self.surface.width.next_down()), p.y.clamp(0.0, self.surface.height.next_down()))
    }

    /// The panel point showing surface point `at`.
    pub fn to_panel(&self, at: Point) -> Point {
        Point::new(self.rect.x + at.x * self.scale, self.rect.y + at.y * self.scale)
    }

    /// A length in panel pixels expressed in surface pixels.
    pub fn to_surface_len(&self, len: f32) -> f32 {
        len / self.scale
    }

    fn unclamped(&self, at: Point) -> Point {
        Point::new((at.x - self.rect.x) / self.scale, (at.y - self.rect.y) / self.scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_points_map_through_bars_and_scale() {
        let p = |x, y| Point::new(x, y);
        // (surface, panel, scale, picture origin, panel point, surface point)
        let cases = [
            // Wide panel: bars left and right, half size.
            ((800, 600), (1000.0, 300.0), 0.5, p(300.0, 0.0), p(300.0, 0.0), Some(p(0.0, 0.0))),
            ((800, 600), (1000.0, 300.0), 0.5, p(300.0, 0.0), p(500.0, 150.0), Some(p(400.0, 300.0))),
            ((800, 600), (1000.0, 300.0), 0.5, p(300.0, 0.0), p(299.0, 150.0), None),
            ((800, 600), (1000.0, 300.0), 0.5, p(300.0, 0.0), p(700.0, 150.0), None),
            // Tall panel: bars top and bottom, upscaled.
            ((100, 50), (400.0, 600.0), 4.0, p(0.0, 200.0), p(40.0, 240.0), Some(p(10.0, 10.0))),
            ((100, 50), (400.0, 600.0), 4.0, p(0.0, 200.0), p(40.0, 100.0), None),
            ((100, 50), (400.0, 600.0), 4.0, p(0.0, 200.0), p(40.0, 400.0), None),
        ];
        for ((w, h), (pw, ph), scale, origin, at, want) in cases {
            let placed = fit(w, h, Size::new(pw, ph)).unwrap();
            assert_eq!((placed.scale, placed.rect.position()), (scale, origin), "{w}x{h} in {pw}x{ph}");
            assert_eq!(placed.to_surface(at), want, "{at:?}");
            if let Some(s) = want {
                assert_eq!(placed.to_panel(s), at);
            }
        }
    }

    #[test]
    fn drags_pin_to_the_edge_and_empty_sides_have_no_placement() {
        let p = fit(800, 600, Size::new(1000.0, 300.0)).unwrap();
        assert_eq!(p.to_surface_clamped(Point::new(0.0, 150.0)), Point::new(0.0, 300.0));
        let right = p.to_surface_clamped(Point::new(1000.0, -50.0));
        assert!(right.x < 800.0 && right.x > 799.9 && right.y == 0.0, "{right:?}");
        assert_eq!(fit(0, 10, Size::new(10.0, 10.0)), None);
        assert_eq!(fit(10, 10, Size::new(0.0, 10.0)), None);
    }
}
