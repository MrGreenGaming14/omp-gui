//! RGBA and I420 (BT.601, limited range) conversion for the video codecs.

/// Converts tightly packed RGBA into I420 planes. `width` and `height` must be
/// even; plane lengths must be `w*h`, `w*h/4`, `w*h/4`.
pub fn rgba_to_i420(rgba: &[u8], width: usize, height: usize, y: &mut [u8], u: &mut [u8], v: &mut [u8]) {
    let cw = width / 2;
    for row in 0..height {
        let src = &rgba[row * width * 4..(row + 1) * width * 4];
        let dst = &mut y[row * width..(row + 1) * width];
        for (px, out) in src.as_chunks::<4>().0.iter().zip(dst.iter_mut()) {
            let (r, g, b) = (i32::from(px[0]), i32::from(px[1]), i32::from(px[2]));
            *out = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
        }
    }
    for crow in 0..height / 2 {
        let top = &rgba[2 * crow * width * 4..(2 * crow + 1) * width * 4];
        let bottom = &rgba[(2 * crow + 1) * width * 4..(2 * crow + 2) * width * 4];
        for cx in 0..cw {
            let mut sum = [0i32; 3];
            for line in [top, bottom] {
                for px in line[cx * 8..cx * 8 + 8].as_chunks::<4>().0 {
                    sum[0] += i32::from(px[0]);
                    sum[1] += i32::from(px[1]);
                    sum[2] += i32::from(px[2]);
                }
            }
            let (r, g, b) = ((sum[0] + 2) >> 2, (sum[1] + 2) >> 2, (sum[2] + 2) >> 2);
            u[crow * cw + cx] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
            v[crow * cw + cx] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
        }
    }
}

/// Planes of one I420 picture with row strides.
#[derive(Clone, Copy)]
pub struct I420<'a> {
    pub width: usize,
    pub height: usize,
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub stride_y: usize,
    pub stride_c: usize,
}

/// Converts I420 into tightly packed RGBA with opaque alpha. `out` holds
/// `width * height * 4` bytes.
pub fn i420_to_rgba(src: I420<'_>, out: &mut [u8]) {
    for row in 0..src.height {
        let ys = &src.y[row * src.stride_y..row * src.stride_y + src.width];
        let us = &src.u[(row / 2) * src.stride_c..];
        let vs = &src.v[(row / 2) * src.stride_c..];
        let dst = &mut out[row * src.width * 4..(row + 1) * src.width * 4];
        for (x, (px, &yv)) in dst.as_chunks_mut::<4>().0.iter_mut().zip(ys).enumerate() {
            let c = 298 * (i32::from(yv) - 16);
            let d = i32::from(us[x / 2]) - 128;
            let e = i32::from(vs[x / 2]) - 128;
            px[0] = clamp((c + 409 * e + 128) >> 8);
            px[1] = clamp((c - 100 * d - 208 * e + 128) >> 8);
            px[2] = clamp((c + 516 * d + 128) >> 8);
            px[3] = 255;
        }
    }
}

fn clamp(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_survive_a_round_trip_within_rounding() {
        let (w, h) = (4, 2);
        let colors = [[200u8, 30, 40, 255], [20, 220, 60, 255]];
        let mut rgba = Vec::new();
        for _row in 0..h {
            for x in 0..w {
                // 2x2 blocks of one color, so chroma subsampling loses nothing.
                rgba.extend_from_slice(&colors[(x / 2) % 2]);
            }
        }
        let (mut y, mut u, mut v) = (vec![0; w * h], vec![0; w * h / 4], vec![0; w * h / 4]);
        rgba_to_i420(&rgba, w, h, &mut y, &mut u, &mut v);
        let mut back = vec![0; w * h * 4];
        i420_to_rgba(I420 { width: w, height: h, y: &y, u: &u, v: &v, stride_y: w, stride_c: w / 2 }, &mut back);
        for (a, b) in rgba.iter().zip(&back) {
            assert!(a.abs_diff(*b) <= 3, "{rgba:?} became {back:?}");
        }
    }
}
