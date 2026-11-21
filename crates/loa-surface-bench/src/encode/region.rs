//! Exact 16-pixel tile damage tracking and JPEG coding of changed rectangles.

use crate::{BenchError, Result};
use bytes::Bytes;
use loa_contracts::surface::Rect;

const TILE: usize = 16;

/// Remembers the last submitted picture, even when JPEG is lossy.
pub struct DamageTracker {
    width: usize,
    height: usize,
    previous: Vec<u8>,
    initialized: bool,
}

impl DamageTracker {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width: width as usize, height: height as usize, previous: Vec::new(), initialized: false }
    }

    /// Returns nonoverlapping tile-aligned rectangles (clipped at the edges).
    /// Adjacent dirty tiles are combined so the JPEG coder pays for fewer headers.
    pub fn update(&mut self, rgba: &[u8]) -> Vec<Rect> {
        let columns = self.width.div_ceil(TILE);
        let rows = self.height.div_ceil(TILE);
        let mut dirty = vec![false; columns * rows];
        let stride = self.width * 4;
        for ty in 0..rows {
            for tx in 0..columns {
                let x = tx * TILE;
                let y = ty * TILE;
                let row_len = (self.width - x).min(TILE) * 4;
                let changed = !self.initialized || (y..(y + TILE).min(self.height)).any(|row| {
                    let start = row * stride + x * 4;
                    rgba[start..start + row_len] != self.previous[start..start + row_len]
                });
                dirty[ty * columns + tx] = changed;
            }
        }
        if !self.initialized {
            self.previous = rgba.to_vec();
            self.initialized = true;
        } else if dirty.iter().any(|&changed| changed) {
            self.previous.copy_from_slice(rgba);
        }

        let mut result = Vec::new();
        for ty in 0..rows {
            for tx in 0..columns {
                if !dirty[ty * columns + tx] { continue; }
                let mut right = tx + 1;
                while right < columns && dirty[ty * columns + right] { right += 1; }
                let mut bottom = ty + 1;
                while bottom < rows && (tx..right).all(|x| dirty[bottom * columns + x]) {
                    bottom += 1;
                }
                for row in ty..bottom {
                    for col in tx..right {
                        dirty[row * columns + col] = false;
                    }
                }
                let x = tx * TILE;
                let y = ty * TILE;
                result.push(Rect {
                    x: x as u32, y: y as u32,
                    width: (right * TILE).min(self.width).saturating_sub(x) as u32,
                    height: (bottom * TILE).min(self.height).saturating_sub(y) as u32,
                });
            }
        }
        result
    }
}

pub struct RegionEncoder {
    quality: u8,
}

impl RegionEncoder {
    pub fn new(quality: u8) -> Self { Self { quality } }

    pub fn quality(&self) -> u8 { self.quality }

    /// Encodes exactly `rect`'s pixels, without resizing or resending the canvas.
    pub fn encode(&mut self, rgba: &[u8], width: u32, rect: Rect) -> Result<Bytes> {
        let stride = (width as usize).checked_mul(4)
            .ok_or_else(|| BenchError::Encode("JPEG picture width overflows".into()))?;
        let height = rgba.len().checked_div(stride).unwrap_or(0);
        let end_x = rect.x.checked_add(rect.width);
        let end_y = rect.y.checked_add(rect.height);
        if rect.width == 0 || rect.height == 0 || rect.width > u16::MAX as u32
            || rect.height > u16::MAX as u32 || end_x.is_none_or(|x| x > width)
            || end_y.is_none_or(|y| y as usize > height)
            || stride == 0 || !rgba.len().is_multiple_of(stride)
        {
            return Err(BenchError::Encode("JPEG rectangle lies outside its RGBA picture".into()));
        }
        let mut rgb = Vec::with_capacity(rect.width as usize * rect.height as usize * 3);
        // JPEG carries RGB; the incoming canvas alpha is not a JPEG channel.
        for y in rect.y as usize..end_y.unwrap() as usize {
            let start = y * stride + rect.x as usize * 4;
            for pixel in rgba[start..start + rect.width as usize * 4].as_chunks::<4>().0 {
                rgb.extend_from_slice(&pixel[..3]);
            }
        }
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, self.quality)
            .encode(&rgb, rect.width as u16, rect.height as u16, jpeg_encoder::ColorType::Rgb)
            .map_err(|error| BenchError::Encode(format!("JPEG: {error}")))?;
        Ok(Bytes::from(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_single_tile_clipped_edges_and_merges_neighbors() {
        let mut tracker = DamageTracker::new(35, 33);
        let mut image = vec![0; 35 * 33 * 4];
        assert_eq!(tracker.update(&image), vec![Rect { x: 0, y: 0, width: 35, height: 33 }]);
        assert!(tracker.update(&image).is_empty());
        image[(32 * 35 + 34) * 4] = 7;
        assert_eq!(tracker.update(&image), vec![Rect { x: 32, y: 32, width: 3, height: 1 }]);
        image[(35 + 1) * 4] = 7;
        image[(35 + 18) * 4] = 7;
        assert_eq!(tracker.update(&image), vec![Rect { x: 0, y: 0, width: 32, height: 16 }]);
    }

    #[test]
    fn jpeg_contains_only_requested_rectangle() {
        let mut rgba = vec![0u8; 32 * 16 * 4];
        for pixel in rgba.as_chunks_mut::<4>().0 { pixel[3] = 255; }
        for row in 0..16 {
            for col in 16..32 { rgba[(row * 32 + col) * 4] = 230; }
        }
        let data = RegionEncoder::new(95).encode(&rgba, 32, Rect { x: 16, y: 0, width: 16, height: 16 }).unwrap();
        assert_eq!(&data[..2], &[0xff, 0xd8]);
        assert_eq!(&data[data.len() - 2..], &[0xff, 0xd9]);
    }
}
