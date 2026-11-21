//! The retained picture of a pixel surface that region updates are applied to.

use bytes::{Bytes, BytesMut};
use iced::widget::image::Handle;

/// Bytes of a `width` by `height` RGBA picture, `None` on overflow.
fn rgba_len(width: u32, height: u32) -> Option<usize> {
    (width as usize).checked_mul(height as usize)?.checked_mul(4)
}

/// A full surface RGBA picture. Showing it hands the pixels to iced without copying; the next
/// update takes them back, copying only when the renderer still holds the shown picture.
#[derive(Debug)]
pub struct Framebuffer {
    width: u32,
    height: u32,
    pixels: Pixels,
}

#[derive(Debug)]
enum Pixels {
    Owned(BytesMut),
    Shown(Bytes),
}

impl Framebuffer {
    /// A transparent black picture, `None` when the size overflows memory.
    pub fn new(width: u32, height: u32) -> Option<Self> {
        Some(Self { width, height, pixels: Pixels::Owned(BytesMut::zeroed(rgba_len(width, height)?)) })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[u8] {
        match &self.pixels {
            Pixels::Owned(p) => p,
            Pixels::Shown(p) => p,
        }
    }

    /// Writable pixels, taking them back from the last shown picture.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        if let Pixels::Shown(shown) = &mut self.pixels {
            let shown = std::mem::take(shown);
            self.pixels = Pixels::Owned(shown.try_into_mut().unwrap_or_else(|still_shown| BytesMut::from(&still_shown[..])));
        }
        match &mut self.pixels {
            Pixels::Owned(p) => p,
            Pixels::Shown(_) => unreachable!("framebuffer pixels were just taken back"),
        }
    }

    /// An image handle showing the current picture.
    pub fn handle(&mut self) -> Handle {
        let shown = match std::mem::replace(&mut self.pixels, Pixels::Shown(Bytes::new())) {
            Pixels::Owned(owned) => owned.freeze(),
            Pixels::Shown(shown) => shown,
        };
        self.pixels = Pixels::Shown(shown.clone());
        Handle::from_rgba(self.width, self.height, shown)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn showing_and_updating_keeps_the_picture() {
        let mut fb = super::Framebuffer::new(2, 2).unwrap();
        fb.pixels_mut().fill(1);
        let shown = fb.handle();
        fb.pixels_mut()[12..].fill(7);
        // The update sees the earlier picture even while it is still shown.
        assert_eq!((&fb.pixels()[..12], &fb.pixels()[12..]), (&[1; 12][..], &[7; 4][..]));
        drop(shown);
    }
}
