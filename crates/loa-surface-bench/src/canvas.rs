//! The viewer side picture: frames are decoded into it, and the benchmark
//! looks at it to decide whether an input's effect is visible.

use crate::{BenchError, Result};
use loa_surface::Rect;

/// An RGBA picture, row major, no padding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Canvas {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, rgba: vec![0; width as usize * height as usize * 4] }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    pub fn rgba_mut(&mut self) -> &mut [u8] {
        &mut self.rgba
    }

    /// Follows a surface resize. The old content is dropped.
    pub fn ensure_size(&mut self, width: u32, height: u32) {
        if (width, height) != (self.width, self.height) {
            *self = Self::new(width, height);
        }
    }

    /// Whether `region` lies inside the canvas.
    pub fn contains(&self, region: Rect) -> bool {
        u64::from(region.x) + u64::from(region.width) <= u64::from(self.width)
            && u64::from(region.y) + u64::from(region.height) <= u64::from(self.height)
    }

    /// Copies tightly packed RGBA `pixels` covering `region` into the canvas.
    pub fn blit(&mut self, region: Rect, pixels: &[u8]) -> Result<()> {
        let row_len = region.width as usize * 4;
        if !self.contains(region) || pixels.len() != row_len * region.height as usize {
            return Err(BenchError::Invalid(format!(
                "{} bytes for region {region:?} of a {}x{} canvas",
                pixels.len(),
                self.width,
                self.height
            )));
        }
        let stride = self.width as usize * 4;
        for (row, src) in pixels.chunks_exact(row_len.max(1)).enumerate() {
            let start = (region.y as usize + row) * stride + region.x as usize * 4;
            self.rgba[start..start + row_len].copy_from_slice(src);
        }
        Ok(())
    }

    /// The pixels of `region`, tightly packed. `region` must lie inside.
    pub fn crop(&self, region: Rect) -> Vec<u8> {
        let row_len = region.width as usize * 4;
        let stride = self.width as usize * 4;
        let mut out = Vec::with_capacity(row_len * region.height as usize);
        for row in 0..region.height as usize {
            let start = (region.y as usize + row) * stride + region.x as usize * 4;
            out.extend_from_slice(&self.rgba[start..start + row_len]);
        }
        out
    }

    /// Mean absolute difference per color channel between `region` now and
    /// `baseline` (an earlier [`crop`](Self::crop) of the same region).
    /// Alpha is ignored. 0 means identical, 255 means fully inverted.
    pub fn mean_abs_diff(&self, region: Rect, baseline: &[u8]) -> f64 {
        let row_len = region.width as usize * 4;
        let stride = self.width as usize * 4;
        let mut sum = 0u64;
        for (row, base) in baseline.chunks_exact(row_len.max(1)).enumerate() {
            let start = (region.y as usize + row) * stride + region.x as usize * 4;
            for (now, then) in self.rgba[start..start + row_len].as_chunks::<4>().0.iter().zip(base.as_chunks::<4>().0.iter()) {
                sum += u64::from(now[0].abs_diff(then[0]))
                    + u64::from(now[1].abs_diff(then[1]))
                    + u64::from(now[2].abs_diff(then[2]));
            }
        }
        let channels = u64::from(region.width) * u64::from(region.height) * 3;
        if channels == 0 { 0.0 } else { sum as f64 / channels as f64 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blitted_region_lands_where_its_rect_says() {
        let mut canvas = Canvas::new(4, 3);
        let region = Rect { x: 1, y: 1, width: 2, height: 2 };
        let before = canvas.crop(region);
        canvas.blit(region, &[200; 16]).unwrap();
        assert_eq!(canvas.mean_abs_diff(region, &before), 200.0);
        // Pixels outside the region are untouched.
        assert_eq!(&canvas.rgba()[0..4], &[0, 0, 0, 0]);
        assert_eq!(&canvas.rgba()[(4 + 3) * 4..(4 + 3) * 4 + 4], &[0, 0, 0, 0]);
        assert_eq!(&canvas.rgba()[(4 + 1) * 4..(4 + 1) * 4 + 4], &[200, 200, 200, 200]);
    }

    #[test]
    fn a_region_past_the_edge_is_refused() {
        let mut canvas = Canvas::new(4, 3);
        let err = canvas.blit(Rect { x: 3, y: 0, width: 2, height: 1 }, &[0; 8]);
        assert!(err.is_err());
    }
}
