//! The retained picture of a pixel surface that region updates are applied to.

use bytes::{Bytes, BytesMut};
use iced::widget::image::Handle;
use loa_contracts::surface::Rect;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BlitError {
    #[error("region {region:?} is outside the {width}x{height} surface")]
    OutOfBounds {
        region: Rect,
        width: u32,
        height: u32,
    },
    #[error("region needs {expected} bytes of RGBA, got {actual}")]
    Length { expected: usize, actual: usize },
    #[error("a {width}x{height} framebuffer needs {expected} bytes, it has {actual}")]
    Target {
        width: u32,
        height: u32,
        expected: usize,
        actual: usize,
    },
}

/// Bytes of a `width` by `height` RGBA picture, `None` on overflow.
pub fn rgba_len(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
}

/// Whether `region` lies inside a `width` by `height` surface.
pub fn region_fits(region: Rect, width: u32, height: u32) -> bool {
    u64::from(region.x) + u64::from(region.width) <= u64::from(width)
        && u64::from(region.y) + u64::from(region.height) <= u64::from(height)
}

/// Copies the RGBA rows of `src` into the `region` of `dst`, a `width` by
/// `height` RGBA picture. Nothing is written when the sizes don't match.
pub fn blit(
    dst: &mut [u8],
    width: u32,
    height: u32,
    src: &[u8],
    region: Rect,
) -> Result<(), BlitError> {
    let target = rgba_len(width, height).unwrap_or(usize::MAX);
    if dst.len() != target {
        return Err(BlitError::Target {
            width,
            height,
            expected: target,
            actual: dst.len(),
        });
    }
    if !region_fits(region, width, height) {
        return Err(BlitError::OutOfBounds {
            region,
            width,
            height,
        });
    }
    // The region fits inside a picture whose byte length fits in usize.
    let row = region.width as usize * 4;
    let expected = row * region.height as usize;
    if src.len() != expected {
        return Err(BlitError::Length {
            expected,
            actual: src.len(),
        });
    }
    if row == 0 {
        return Ok(());
    }
    let stride = width as usize * 4;
    let left = region.x as usize * 4;
    for (i, line) in src.chunks_exact(row).enumerate() {
        let start = (region.y as usize + i) * stride + left;
        dst[start..start + row].copy_from_slice(line);
    }
    Ok(())
}

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

    fn px(fb: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * width + x) * 4) as usize;
        fb[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn region_update_changes_only_the_region() {
        let mut fb = vec![9u8; 4 * 4 * 4];
        let src: Vec<u8> = (0..2 * 3).flat_map(|i| [i as u8, 0, 0, 255]).collect();
        blit(
            &mut fb,
            4,
            4,
            &src,
            Rect {
                x: 1,
                y: 1,
                width: 2,
                height: 3,
            },
        )
        .unwrap();
        assert_eq!(px(&fb, 4, 1, 1), [0, 0, 0, 255]);
        assert_eq!(px(&fb, 4, 2, 1), [1, 0, 0, 255]);
        assert_eq!(px(&fb, 4, 2, 3), [5, 0, 0, 255]);
        for (x, y) in [(0, 0), (3, 1), (0, 1), (1, 0), (3, 3)] {
            assert_eq!(px(&fb, 4, x, y), [9; 4], "({x}, {y}) changed");
        }
    }

    #[test]
    fn region_past_the_edge_is_refused_untouched() {
        let mut fb = vec![9u8; 4 * 4 * 4];
        let src = vec![0u8; 2 * 2 * 4];
        let err = blit(
            &mut fb,
            4,
            4,
            &src,
            Rect {
                x: 3,
                y: 0,
                width: 2,
                height: 2,
            },
        )
        .unwrap_err();
        assert!(matches!(err, BlitError::OutOfBounds { .. }));
        assert!(fb.iter().all(|&b| b == 9));
    }

    #[test]
    fn short_data_is_refused_untouched() {
        let mut fb = vec![9u8; 4 * 4 * 4];
        let err = blit(
            &mut fb,
            4,
            4,
            &[0; 12],
            Rect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
        )
        .unwrap_err();
        assert_eq!(
            err,
            BlitError::Length {
                expected: 16,
                actual: 12
            }
        );
        assert!(fb.iter().all(|&b| b == 9));
    }

    #[test]
    fn showing_and_updating_keeps_the_picture() {
        let mut fb = Framebuffer::new(2, 2).unwrap();
        fb.blit(
            &[1; 16],
            Rect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
        )
        .unwrap();
        let shown = fb.handle();
        fb.blit(
            &[7; 4],
            Rect {
                x: 1,
                y: 1,
                width: 1,
                height: 1,
            },
        )
        .unwrap();
        // The update sees the earlier picture even while it is still shown.
        assert_eq!(&fb.pixels()[..12], &[1; 12]);
        assert_eq!(&fb.pixels()[12..], &[7; 4]);
        drop(shown);
    }
}
