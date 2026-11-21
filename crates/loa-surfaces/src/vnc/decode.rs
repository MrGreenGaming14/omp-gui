//! RFB rectangle decoders: one [`RectDecoder`] per encoding, kept in the [`Decoders`] registry.
//!
//! A pixel decoder reads one rectangle into a scratch picture the size of the
//! rectangle; the caller then blits it into the framebuffer.

use crate::pixels::{RgbaImage, region_fits};
use crate::vnc::RfbError;
use crate::vnc::proto::BPP;
use async_trait::async_trait;
use flate2::{Decompress, FlushDecompress, Status};
use loa_contracts::surface::Rect;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Upper bound for one ZRLE rectangle's compressed data.
const MAX_ZRLE: u32 = 64 << 20;

pub(crate) type Reader<'a> = dyn AsyncRead + Unpin + Send + 'a;

/// What a decoded rectangle does to the framebuffer.
#[derive(Debug, PartialEq)]
pub(crate) enum Update {
    /// The scratch picture holds the rectangle's pixels.
    Blit,
    /// Copy the rectangle's area from `(x, y)`.
    Copy { x: u32, y: u32 },
    /// The desktop is now the rectangle's size.
    Resize,
    /// Nothing to paint.
    Skip,
}

/// Decodes the rectangles of one encoding.
#[async_trait]
pub(crate) trait RectDecoder: Send {
    async fn decode(&mut self, r: Rect, rd: &mut Reader<'_>, out: &mut RgbaImage) -> Result<Update, RfbError>;
}

/// Pseudo encodings (negative numbers) describe the desktop instead of painting inside it.
pub(crate) fn is_pseudo(encoding: i32) -> bool {
    encoding < 0
}

/// The decoders by encoding number in the client's order of preference, and the scratch picture they share.
pub(crate) struct Decoders {
    list: Vec<(i32, Box<dyn RectDecoder>)>,
    pub scratch: RgbaImage,
}

impl Decoders {
    pub fn new() -> Self {
        let zrle = Zrle { inflate: Decompress::new(true), zbuf: Vec::new(), zout: Vec::new() };
        let list: Vec<(i32, Box<dyn RectDecoder>)> = vec![
            (1, Box::new(CopyRect)),
            (16, Box::new(zrle)),
            (5, Box::new(Hextile)),
            (2, Box::new(Rre)),
            (0, Box::new(Raw)),
            (-239, Box::new(CursorShape)),
            (-223, Box::new(DesktopSize)),
        ];
        Self { list, scratch: RgbaImage::default() }
    }

    pub fn get(&mut self, encoding: i32) -> Option<(&mut dyn RectDecoder, &mut RgbaImage)> {
        let (_, decoder) = self.list.iter_mut().find(|(e, _)| *e == encoding)?;
        Some((decoder.as_mut(), &mut self.scratch))
    }
}

/// The encodings the client asks for, from the registry.
pub(crate) fn encodings() -> Vec<i32> {
    Decoders::new().list.iter().map(|(e, _)| *e).collect()
}

fn reset(out: &mut RgbaImage, r: Rect) {
    (out.width, out.height) = (r.width, r.height);
    out.data.clear();
    out.data.resize(r.width as usize * r.height as usize * BPP, 0);
}

/// Reads wire pixels (`R G B X`) into `buf` as opaque RGBA.
async fn read_opaque(rd: &mut Reader<'_>, buf: &mut [u8]) -> Result<(), RfbError> {
    rd.read_exact(buf).await?;
    for px in buf.as_chunks_mut::<BPP>().0 {
        px[3] = 255;
    }
    Ok(())
}

async fn read_pixel(rd: &mut Reader<'_>) -> Result<[u8; 4], RfbError> {
    let mut px = [0u8; BPP];
    read_opaque(rd, &mut px).await?;
    Ok(px)
}

fn check_inside(s: Rect, width: u32, height: u32, what: &str) -> Result<(), RfbError> {
    if region_fits(s, width, height) { Ok(()) } else { Err(RfbError::Protocol(format!("{what} subrectangle {s:?} outside {width}x{height}"))) }
}

/// Reads a rectangle as four u16 fields.
pub(crate) async fn read_rect<R: AsyncRead + Unpin + ?Sized>(rd: &mut R) -> Result<Rect, RfbError> {
    let mut f = [0u32; 4];
    for v in &mut f {
        *v = u32::from(rd.read_u16().await?);
    }
    Ok(Rect { x: f[0], y: f[1], width: f[2], height: f[3] })
}

/// Reads and discards `len` bytes.
pub(crate) async fn skip<R: AsyncRead + Unpin + ?Sized>(rd: &mut R, len: u64) -> Result<(), RfbError> {
    let copied = tokio::io::copy(&mut rd.take(len), &mut tokio::io::sink()).await?;
    if copied < len {
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
    }
    Ok(())
}

struct Raw;

#[async_trait]
impl RectDecoder for Raw {
    async fn decode(&mut self, r: Rect, rd: &mut Reader<'_>, out: &mut RgbaImage) -> Result<Update, RfbError> {
        reset(out, r);
        read_opaque(rd, &mut out.data).await?;
        Ok(Update::Blit)
    }
}

struct CopyRect;

#[async_trait]
impl RectDecoder for CopyRect {
    async fn decode(&mut self, _: Rect, rd: &mut Reader<'_>, _: &mut RgbaImage) -> Result<Update, RfbError> {
        Ok(Update::Copy { x: u32::from(rd.read_u16().await?), y: u32::from(rd.read_u16().await?) })
    }
}

/// A background pixel, then solid subrectangles.
struct Rre;

#[async_trait]
impl RectDecoder for Rre {
    async fn decode(&mut self, r: Rect, rd: &mut Reader<'_>, out: &mut RgbaImage) -> Result<Update, RfbError> {
        reset(out, r);
        let count = rd.read_u32().await?;
        let bg = read_pixel(rd).await?;
        out.fill(out.full(), bg);
        for _ in 0..count {
            let px = read_pixel(rd).await?;
            let s = read_rect(rd).await?;
            check_inside(s, r.width, r.height, "RRE")?;
            out.fill(s, px);
        }
        Ok(Update::Blit)
    }
}

/// 16x16 tiles, left to right, top to bottom.
struct Hextile;

#[async_trait]
impl RectDecoder for Hextile {
    async fn decode(&mut self, r: Rect, rd: &mut Reader<'_>, out: &mut RgbaImage) -> Result<Update, RfbError> {
        const RAW: u8 = 1;
        const BACKGROUND: u8 = 2;
        const FOREGROUND: u8 = 4;
        const ANY_SUBRECTS: u8 = 8;
        const COLOURED: u8 = 16;
        reset(out, r);
        let (mut bg, mut fg) = ([0, 0, 0, 255], [0, 0, 0, 255]);
        for ty in (0..r.height).step_by(16) {
            for tx in (0..r.width).step_by(16) {
                let tile = Rect { x: tx, y: ty, width: (r.width - tx).min(16), height: (r.height - ty).min(16) };
                let flags = rd.read_u8().await?;
                if flags & RAW != 0 {
                    for y in tile.y..tile.y + tile.height {
                        let start = (y * r.width + tile.x) as usize * BPP;
                        read_opaque(rd, &mut out.data[start..start + tile.width as usize * BPP]).await?;
                    }
                    continue;
                }
                if flags & BACKGROUND != 0 {
                    bg = read_pixel(rd).await?;
                }
                if flags & FOREGROUND != 0 {
                    fg = read_pixel(rd).await?;
                }
                out.fill(tile, bg);
                if flags & ANY_SUBRECTS == 0 {
                    continue;
                }
                for _ in 0..rd.read_u8().await? {
                    let px = if flags & COLOURED != 0 { read_pixel(rd).await? } else { fg };
                    let (xy, wh) = (rd.read_u8().await?, rd.read_u8().await?);
                    let s = Rect { x: u32::from(xy >> 4), y: u32::from(xy & 0xf), width: u32::from(wh >> 4) + 1, height: u32::from(wh & 0xf) + 1 };
                    check_inside(s, tile.width, tile.height, "Hextile")?;
                    out.fill(Rect { x: tile.x + s.x, y: tile.y + s.y, ..s }, px);
                }
            }
        }
        Ok(Update::Blit)
    }
}

/// Zlib compressed 64x64 tiles of CPIXELs (`R G B`); one zlib stream per connection.
struct Zrle {
    inflate: Decompress,
    zbuf: Vec<u8>,
    zout: Vec<u8>,
}

#[async_trait]
impl RectDecoder for Zrle {
    async fn decode(&mut self, r: Rect, rd: &mut Reader<'_>, out: &mut RgbaImage) -> Result<Update, RfbError> {
        let len = rd.read_u32().await?;
        if len > MAX_ZRLE {
            return Err(RfbError::Protocol(format!("ZRLE rectangle of {len} bytes")));
        }
        self.zbuf.clear();
        self.zbuf.resize(len as usize, 0);
        rd.read_exact(&mut self.zbuf).await?;
        inflate(&mut self.inflate, &self.zbuf, &mut self.zout)?;
        reset(out, r);
        decode_zrle_tiles(&self.zout, out)?;
        Ok(Update::Blit)
    }
}

/// The pointer shape: pixels, then a 1 bit per pixel mask. The client asks for it only so
/// that the server leaves the pointer out of the picture, and drops it.
struct CursorShape;

#[async_trait]
impl RectDecoder for CursorShape {
    async fn decode(&mut self, r: Rect, rd: &mut Reader<'_>, _: &mut RgbaImage) -> Result<Update, RfbError> {
        let (width, height) = (u64::from(r.width), u64::from(r.height));
        skip(rd, width * height * BPP as u64 + width.div_ceil(8) * height).await?;
        Ok(Update::Skip)
    }
}

struct DesktopSize;

#[async_trait]
impl RectDecoder for DesktopSize {
    async fn decode(&mut self, _: Rect, _: &mut Reader<'_>, _: &mut RgbaImage) -> Result<Update, RfbError> {
        Ok(Update::Resize)
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
struct TileData<'a>(&'a [u8]);

impl<'a> TileData<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], RfbError> {
        if self.0.len() < n {
            return Err(RfbError::Protocol("ZRLE data ends inside a tile".to_owned()));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, RfbError> {
        Ok(self.take(1)?[0])
    }

    fn cpixel(&mut self) -> Result<[u8; 4], RfbError> {
        let p = self.take(3)?;
        Ok([p[0], p[1], p[2], 255])
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

    fn palette(&mut self, palette: &mut [[u8; 4]; 127], size: usize) -> Result<(), RfbError> {
        for p in &mut palette[..size] {
            *p = self.cpixel()?;
        }
        Ok(())
    }
}

/// Decodes the inflated tiles of one ZRLE rectangle into `out`.
fn decode_zrle_tiles(data: &[u8], out: &mut RgbaImage) -> Result<(), RfbError> {
    let (width, height) = (out.width, out.height);
    let mut rd = TileData(data);
    let mut palette = [[0u8; 4]; 127];
    let bad_index = |index: usize| RfbError::Protocol(format!("ZRLE palette index {index}"));
    for ty in (0..height).step_by(64) {
        for tx in (0..width).step_by(64) {
            let tile = Rect { x: tx, y: ty, width: (width - tx).min(64), height: (height - ty).min(64) };
            let tw = tile.width as usize;
            let count = tw * tile.height as usize;
            let at = |i: usize| ((tile.y as usize + i / tw) * width as usize + tile.x as usize + i % tw) * BPP;
            let mut put = |i: usize, px: &[u8; 4]| out.data[at(i)..at(i) + BPP].copy_from_slice(px);
            match rd.u8()? {
                0 => {
                    for i in 0..count {
                        put(i, &rd.cpixel()?);
                    }
                }
                1 => {
                    let px = rd.cpixel()?;
                    (0..count).for_each(|i| put(i, &px));
                }
                size @ 2..=16 => {
                    let size = usize::from(size);
                    rd.palette(&mut palette, size)?;
                    let bits = match size {
                        2 => 1,
                        3..=4 => 2,
                        _ => 4,
                    };
                    for y in 0..tile.height as usize {
                        let row = rd.take((tw * bits).div_ceil(8))?;
                        for x in 0..tw {
                            let bit = x * bits;
                            let index = usize::from((row[bit / 8] >> (8 - bits - bit % 8)) & ((1 << bits) - 1));
                            put(y * tw + x, palette[..size].get(index).ok_or_else(|| bad_index(index))?);
                        }
                    }
                }
                sub @ (128 | 130..=255) => {
                    let size = usize::from(sub - 128);
                    rd.palette(&mut palette, size)?;
                    let mut i = 0;
                    while i < count {
                        let (px, run) = if size == 0 {
                            (rd.cpixel()?, rd.run()?)
                        } else {
                            let b = rd.u8()?;
                            let index = usize::from(b & 0x7f);
                            let px = *palette[..size].get(index).ok_or_else(|| bad_index(index))?;
                            (px, if b & 0x80 != 0 { rd.run()? } else { 1 })
                        };
                        if i + run > count {
                            return Err(RfbError::Protocol("ZRLE run past the end of a tile".to_owned()));
                        }
                        (i..i + run).for_each(|j| put(j, &px));
                        i += run;
                    }
                }
                other => return Err(RfbError::Protocol(format!("ZRLE subencoding {other}"))),
            }
        }
    }
    Ok(())
}
