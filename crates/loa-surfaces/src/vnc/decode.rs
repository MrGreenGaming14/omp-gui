//! Framebuffer and rectangle decoders for the encodings the client accepts.
//!
//! Every decoder writes one rectangle as 8 bit RGBA into a scratch buffer the
//! size of the rectangle, which the caller then blits into the framebuffer.

use crate::vnc::RfbError;
use crate::pixels::{RgbaImage, region_fits};
use crate::vnc::proto::BPP;
use flate2::{Decompress, FlushDecompress, Status};
use loa_contracts::surface::Rect;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Upper bound for one ZRLE rectangle's compressed data.
const MAX_ZRLE: u32 = 64 << 20;

/// The remote pointer shape from the Cursor pseudo encoding, for drawing the
/// cursor locally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub width: u32,
    pub height: u32,
    /// 8 bit RGBA, transparent where the server's mask is clear.
    pub rgba: Vec<u8>,
}

/// Per connection decoder state: scratch buffers and the ZRLE zlib stream,
/// which runs across all ZRLE rectangles of a connection.
pub(crate) struct Decoder {
    pub scratch: RgbaImage,
    zbuf: Vec<u8>,
    zout: Vec<u8>,
    inflate: Decompress,
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            scratch: RgbaImage::default(),
            zbuf: Vec::new(),
            zout: Vec::new(),
            inflate: Decompress::new(true),
        }
    }

    fn reset_scratch(&mut self, r: Rect) {
        (self.scratch.width, self.scratch.height) = (r.width, r.height);
        self.scratch.data.clear();
        self.scratch.data.resize(r.width as usize * r.height as usize * BPP, 0);
    }

    /// Raw: `w * h` pixels.
    pub async fn raw<R: AsyncRead + Unpin>(&mut self, rd: &mut R, r: Rect) -> Result<(), RfbError> {
        self.reset_scratch(r);
        rd.read_exact(&mut self.scratch.data).await?;
        for px in self.scratch.data.as_chunks_mut::<BPP>().0 {
            px[3] = 255;
        }
        Ok(())
    }

    /// RRE: a background pixel, then solid subrectangles.
    pub async fn rre<R: AsyncRead + Unpin>(&mut self, rd: &mut R, r: Rect) -> Result<(), RfbError> {
        self.reset_scratch(r);
        let count = rd.read_u32().await?;
        let bg = read_pixel(rd).await?;
        let full = self.scratch.full();
        self.scratch.fill(full, bg);
        let mut sub = [0u8; BPP + 8];
        for _ in 0..count {
            rd.read_exact(&mut sub).await?;
            let px = to_rgba(&sub[..BPP]);
            let field =
                |i: usize| u32::from(u16::from_be_bytes([sub[BPP + 2 * i], sub[BPP + 2 * i + 1]]));
            let s = Rect {
                x: field(0),
                y: field(1),
                width: field(2),
                height: field(3),
            };
            check_inside(s, r.width, r.height, "RRE")?;
            self.scratch.fill(s, px);
        }
        Ok(())
    }

    /// Hextile: 16x16 tiles, left to right, top to bottom.
    pub async fn hextile<R: AsyncRead + Unpin>(
        &mut self,
        rd: &mut R,
        r: Rect,
    ) -> Result<(), RfbError> {
        const RAW: u8 = 1;
        const BACKGROUND: u8 = 2;
        const FOREGROUND: u8 = 4;
        const ANY_SUBRECTS: u8 = 8;
        const SUBRECTS_COLOURED: u8 = 16;

        self.reset_scratch(r);
        let mut bg = [0, 0, 0, 255];
        let mut fg = [0, 0, 0, 255];
        for ty in (0..r.height).step_by(16) {
            for tx in (0..r.width).step_by(16) {
                let tile = Rect {
                    x: tx,
                    y: ty,
                    width: (r.width - tx).min(16),
                    height: (r.height - ty).min(16),
                };
                let flags = rd.read_u8().await?;
                if flags & RAW != 0 {
                    let mut row = [0u8; 16 * BPP];
                    let row = &mut row[..tile.width as usize * BPP];
                    for y in 0..tile.height {
                        rd.read_exact(row).await?;
                        for px in row.as_chunks_mut::<BPP>().0 {
                            px[3] = 255;
                        }
                        let start = ((tile.y + y) * r.width + tile.x) as usize * BPP;
                        self.scratch.data[start..start + row.len()].copy_from_slice(row);
                    }
                    continue;
                }
                if flags & BACKGROUND != 0 {
                    bg = read_pixel(rd).await?;
                }
                if flags & FOREGROUND != 0 {
                    fg = read_pixel(rd).await?;
                }
                self.scratch.fill(tile, bg);
                if flags & ANY_SUBRECTS == 0 {
                    continue;
                }
                let count = rd.read_u8().await?;
                for _ in 0..count {
                    let px = if flags & SUBRECTS_COLOURED != 0 {
                        read_pixel(rd).await?
                    } else {
                        fg
                    };
                    let xy = rd.read_u8().await?;
                    let wh = rd.read_u8().await?;
                    let s = Rect {
                        x: u32::from(xy >> 4),
                        y: u32::from(xy & 0xf),
                        width: u32::from(wh >> 4) + 1,
                        height: u32::from(wh & 0xf) + 1,
                    };
                    check_inside(s, tile.width, tile.height, "Hextile")?;
                    let s = Rect {
                        x: tile.x + s.x,
                        y: tile.y + s.y,
                        ..s
                    };
                    self.scratch.fill(s, px);
                }
            }
        }
        Ok(())
    }

    /// ZRLE: zlib compressed 64x64 tiles of CPIXELs (3 bytes, `R G B`).
    pub async fn zrle<R: AsyncRead + Unpin>(
        &mut self,
        rd: &mut R,
        r: Rect,
    ) -> Result<(), RfbError> {
        let len = rd.read_u32().await?;
        if len > MAX_ZRLE {
            return Err(RfbError::Protocol(format!("ZRLE rectangle of {len} bytes")));
        }
        self.zbuf.clear();
        self.zbuf.resize(len as usize, 0);
        rd.read_exact(&mut self.zbuf).await?;
        inflate(&mut self.inflate, &self.zbuf, &mut self.zout)?;
        self.reset_scratch(r);
        decode_zrle_tiles(&self.zout, &mut self.scratch)
    }

    /// Cursor pseudo encoding: pixels, then a 1 bit per pixel mask.
    pub async fn cursor<R: AsyncRead + Unpin>(
        &mut self,
        rd: &mut R,
        r: Rect,
    ) -> Result<Cursor, RfbError> {
        self.raw(rd, r).await?;
        let mask_row = (r.width as usize).div_ceil(8);
        let mut mask = vec![0u8; mask_row * r.height as usize];
        rd.read_exact(&mut mask).await?;
        let mut rgba = std::mem::take(&mut self.scratch).data;
        for (i, px) in rgba.as_chunks_mut::<BPP>().0.iter_mut().enumerate() {
            let (x, y) = (i % r.width as usize, i / r.width as usize);
            let bit = mask[y * mask_row + x / 8] & (0x80 >> (x % 8));
            px[3] = if bit != 0 { 255 } else { 0 };
        }
        Ok(Cursor {
            hotspot_x: r.x,
            hotspot_y: r.y,
            width: r.width,
            height: r.height,
            rgba,
        })
    }
}

async fn read_pixel<R: AsyncRead + Unpin>(rd: &mut R) -> Result<[u8; 4], RfbError> {
    let mut px = [0u8; BPP];
    rd.read_exact(&mut px).await?;
    Ok(to_rgba(&px))
}

/// A wire pixel (`R G B X`) or CPIXEL (`R G B`) as opaque RGBA.
fn to_rgba(px: &[u8]) -> [u8; 4] {
    [px[0], px[1], px[2], 255]
}

fn check_inside(s: Rect, width: u32, height: u32, what: &str) -> Result<(), RfbError> {
    if region_fits(s, width, height) {
        Ok(())
    } else {
        Err(RfbError::Protocol(format!(
            "{what} subrectangle {s:?} outside {width}x{height}"
        )))
    }
}

/// Inflates all of `input` (one sync flushed chunk of the stream) into `out`.
fn inflate(z: &mut Decompress, input: &[u8], out: &mut Vec<u8>) -> Result<(), RfbError> {
    out.clear();
    let mut consumed = 0;
    loop {
        if out.len() == out.capacity() {
            out.reserve(out.capacity().max(input.len() * 4).max(4096));
        }
        let (in_before, out_before) = (z.total_in(), out.len());
        let status = z.decompress_vec(&input[consumed..], out, FlushDecompress::Sync)?;
        consumed += (z.total_in() - in_before) as usize;
        let room_left = out.len() < out.capacity();
        if status == Status::StreamEnd || (consumed == input.len() && room_left) {
            return Ok(());
        }
        if z.total_in() == in_before && out.len() == out_before && room_left {
            return Err(RfbError::Protocol("ZRLE zlib stream stalled".to_owned()));
        }
    }
}

/// Bounds checked reader over inflated ZRLE data.
struct TileData<'a> {
    data: &'a [u8],
}

impl<'a> TileData<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], RfbError> {
        if self.data.len() < n {
            return Err(RfbError::Protocol(
                "ZRLE data ends inside a tile".to_owned(),
            ));
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, RfbError> {
        Ok(self.take(1)?[0])
    }

    fn cpixel(&mut self) -> Result<[u8; 4], RfbError> {
        Ok(to_rgba(self.take(3)?))
    }

    /// A run length: bytes summed until one is not 255, plus one.
    fn run(&mut self) -> Result<usize, RfbError> {
        let mut len = 1;
        loop {
            let b = self.u8()?;
            len += usize::from(b);
            if b != 255 {
                return Ok(len);
            }
        }
    }
}

/// Decodes the inflated tiles of one ZRLE rectangle into `out`.
fn decode_zrle_tiles(data: &[u8], out: &mut RgbaImage) -> Result<(), RfbError> {
    let (width, height) = (out.width, out.height);
    let mut rd = TileData { data };
    let mut palette = [[0u8; 4]; 127];
    for ty in (0..height).step_by(64) {
        for tx in (0..width).step_by(64) {
            let tile = Rect {
                x: tx,
                y: ty,
                width: (width - tx).min(64),
                height: (height - ty).min(64),
            };
            let tw = tile.width as usize;
            let count = tw * tile.height as usize;
            let at = |i: usize| {
                ((tile.y as usize + i / tw) * width as usize + tile.x as usize + i % tw) * BPP
            };
            let sub = rd.u8()?;
            match sub {
                0 => {
                    for i in 0..count {
                        out.data[at(i)..at(i) + BPP].copy_from_slice(&rd.cpixel()?);
                    }
                }
                1 => out.fill(tile, rd.cpixel()?),
                2..=16 => {
                    let size = usize::from(sub);
                    for p in &mut palette[..size] {
                        *p = rd.cpixel()?;
                    }
                    let bits = match size {
                        2 => 1,
                        3..=4 => 2,
                        _ => 4,
                    };
                    let row_bytes = (tw * bits).div_ceil(8);
                    for y in 0..tile.height as usize {
                        let row = rd.take(row_bytes)?;
                        for x in 0..tw {
                            let bit = x * bits;
                            let index = (row[bit / 8] >> (8 - bits - bit % 8)) & ((1 << bits) - 1);
                            let px = palette
                                .get(usize::from(index))
                                .filter(|_| usize::from(index) < size)
                                .ok_or_else(|| {
                                    RfbError::Protocol(format!("ZRLE palette index {index}"))
                                })?;
                            let i = at(y * tw + x);
                            out.data[i..i + BPP].copy_from_slice(px);
                        }
                    }
                }
                128 => {
                    let mut i = 0;
                    while i < count {
                        let px = rd.cpixel()?;
                        let run = rd.run()?;
                        put_run(&mut out.data, &at, &mut i, count, run, px)?;
                    }
                }
                130..=255 => {
                    let size = usize::from(sub - 128);
                    for p in &mut palette[..size] {
                        *p = rd.cpixel()?;
                    }
                    let mut i = 0;
                    while i < count {
                        let b = rd.u8()?;
                        let index = usize::from(b & 0x7f);
                        if index >= size {
                            return Err(RfbError::Protocol(format!("ZRLE palette index {index}")));
                        }
                        let run = if b & 0x80 != 0 { rd.run()? } else { 1 };
                        put_run(&mut out.data, &at, &mut i, count, run, palette[index])?;
                    }
                }
                other => return Err(RfbError::Protocol(format!("ZRLE subencoding {other}"))),
            }
        }
    }
    Ok(())
}

/// Writes a run of `run` pixels starting at tile pixel `*i`.
fn put_run(
    out: &mut [u8],
    at: &impl Fn(usize) -> usize,
    i: &mut usize,
    count: usize,
    run: usize,
    px: [u8; 4],
) -> Result<(), RfbError> {
    if *i + run > count {
        return Err(RfbError::Protocol(
            "ZRLE run past the end of a tile".to_owned(),
        ));
    }
    for j in *i..*i + run {
        out[at(j)..at(j) + BPP].copy_from_slice(&px);
    }
    *i += run;
    Ok(())
}
