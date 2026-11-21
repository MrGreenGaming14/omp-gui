//! The retained picture of a pixel surface that region updates are applied to.

use bytes::{Bytes, BytesMut};
use iced::widget::image::Handle;
use loa_contracts::surface::Rect;

pub use loa_surfaces::pixels::{PixelError as BlitError, blit, region_fits, rgba_len};

/// A full surface RGBA picture.
///
/// Showing it hands the pixels to iced without copying. The next update takes
/// them back, copying only when the renderer still holds the shown picture.
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
        let len = rgba_len(width, height)?;
        Some(Self {
            width,
            height,
            pixels: Pixels::Owned(BytesMut::zeroed(len)),
        })
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
            let owned = shown
                .try_into_mut()
                .unwrap_or_else(|still_shown| BytesMut::from(&still_shown[..]));
            self.pixels = Pixels::Owned(owned);
        }
        match &mut self.pixels {
            Pixels::Owned(p) => p,
            // Converted to `Owned` just above.
            Pixels::Shown(_) => unreachable!("framebuffer pixels were just taken back"),
        }
    }

    /// Writes `src`, RGBA rows covering `region`, into the picture.
    pub fn blit(&mut self, src: &[u8], region: Rect) -> Result<(), BlitError> {
        let (width, height) = (self.width, self.height);
        blit(self.pixels_mut(), width, height, src, region)
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
    use super::*;

    #[test]
    fn showing_and_updating_keeps_the_picture() {
        let mut fb = Framebuffer::new(2, 2).unwrap();
        fb.blit(&[1; 16], Rect { x: 0, y: 0, width: 2, height: 2 }).unwrap();
        let shown = fb.handle();
        fb.blit(&[7; 4], Rect { x: 1, y: 1, width: 1, height: 1 }).unwrap();
        // The update sees the earlier picture even while it is still shown.
        assert_eq!(&fb.pixels()[..12], &[1; 12]);
        assert_eq!(&fb.pixels()[12..], &[7; 4]);
        drop(shown);
    }
}
