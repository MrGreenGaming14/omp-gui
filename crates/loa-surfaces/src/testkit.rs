//! Test doubles and helpers for surface code (feature `testkit`).

use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::surface::{
    Document, Frame, InputEvent, Rect, Result, Surface, SurfaceError, SurfaceInfo, SurfaceMode, SurfaceUri,
};
use loa_util::LockExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;

/// Upper bound for anything a test waits on.
pub const PATIENCE: Duration = Duration::from_secs(20);

/// A scripted surface: the test publishes frames through its [`Probe`] and
/// reads back the inputs and keyframe requests the code under test sent.
pub struct FakeSurface {
    pub info: SurfaceInfo,
    pub document: Option<Document>,
    frames: Mutex<Option<broadcast::Sender<Arc<Frame>>>>,
    inputs: mpsc::UnboundedSender<InputEvent>,
    keyframes: mpsc::UnboundedSender<()>,
    keyframe_count: AtomicUsize,
}

/// The test's end of a [`FakeSurface`].
pub struct Probe {
    pub frames: broadcast::Sender<Arc<Frame>>,
    pub inputs: mpsc::UnboundedReceiver<InputEvent>,
    pub keyframes: mpsc::UnboundedReceiver<()>,
}

impl FakeSurface {
    pub fn new(info: SurfaceInfo, document: Option<Document>) -> (Arc<Self>, Probe) {
        let frames = broadcast::channel(64).0;
        let (inputs_tx, inputs) = mpsc::unbounded_channel();
        let (keyframes_tx, keyframes) = mpsc::unbounded_channel();
        let surface = Self {
            info,
            document,
            frames: Mutex::new(Some(frames.clone())),
            inputs: inputs_tx,
            keyframes: keyframes_tx,
            keyframe_count: AtomicUsize::new(0),
        };
        (Arc::new(surface), Probe { frames, inputs, keyframes })
    }

    /// A pixel surface `surface://<server>/<name>`.
    pub fn pixel(server: &str, name: &str, width: u32, height: u32) -> (Arc<Self>, Probe) {
        Self::new(info(server, name, width, height, SurfaceMode::Pixel, None), None)
    }

    /// A document surface `surface://<server>/<name>` serving `document`.
    pub fn document(server: &str, name: &str, document: Document) -> (Arc<Self>, Probe) {
        let info = info(server, name, 8, 4, SurfaceMode::Document, Some(document.mime.clone()));
        Self::new(info, Some(document))
    }

    /// Ends the frame stream for viewers subscribing from now on.
    pub fn close(&self) {
        self.frames.locked().take();
    }

    pub fn keyframes_requested(&self) -> usize {
        self.keyframe_count.load(Ordering::SeqCst)
    }

    fn mode_error(&self) -> SurfaceError {
        SurfaceError::Mode { uri: self.info.uri.clone(), mode: self.info.mode }
    }
}

fn info(server: &str, name: &str, width: u32, height: u32, mode: SurfaceMode, mime: Option<String>) -> SurfaceInfo {
    SurfaceInfo { uri: SurfaceUri::new(server, name).expect("valid uri"), name: name.into(), width, height, mode, mime }
}

#[async_trait]
impl Surface for FakeSurface {
    fn info(&self) -> SurfaceInfo {
        self.info.clone()
    }

    fn frames(&self) -> Result<broadcast::Receiver<Arc<Frame>>> {
        if self.info.mode != SurfaceMode::Pixel {
            return Err(self.mode_error());
        }
        self.frames.locked().as_ref().map(broadcast::Sender::subscribe).ok_or(SurfaceError::Closed)
    }

    async fn request_keyframe(&self) -> Result<()> {
        self.keyframe_count.fetch_add(1, Ordering::SeqCst);
        let _ = self.keyframes.send(());
        Ok(())
    }

    async fn input(&self, event: InputEvent) -> Result<()> {
        let _ = self.inputs.send(event);
        Ok(())
    }

    async fn document(&self) -> Result<Document> {
        self.document.clone().ok_or_else(|| self.mode_error())
    }

    async fn screenshot(&self) -> Result<Bytes> {
        Ok(Bytes::new())
    }
}

/// Next frame within [`PATIENCE`], skipping over lag.
pub async fn next_frame(frames: &mut broadcast::Receiver<Arc<Frame>>) -> Arc<Frame> {
    loop {
        match tokio::time::timeout(PATIENCE, frames.recv()).await.expect("no frame in time") {
            Ok(frame) => return frame,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => panic!("frame stream closed"),
        }
    }
}

/// `width` x `height` RGBA pixels of one colour.
pub fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    rgba.repeat(width as usize * height as usize)
}

/// A full picture RGBA keyframe.
pub fn rgba_frame(seq: u64, width: u32, height: u32, rgba: [u8; 4]) -> Frame {
    Frame {
        seq,
        captured_us: seq,
        input_seq: 0,
        width,
        height,
        region: Rect { x: 0, y: 0, width, height },
        encoding: loa_contracts::surface::Encoding::Rgba,
        keyframe: true,
        data: solid(width, height, rgba).into(),
    }
}

/// A one-connection RFB 3.8 server: no security, `width` x `height`, one
/// solid raw update after the client's setup once the returned sender fires.
/// Frames go to whoever is subscribed when they are published, so a test
/// subscribes first and then lets the update go. Collects what the client
/// sends after the setup.
pub async fn fake_rfb(width: u16, height: u16, color: [u8; 3]) -> (String, Arc<Mutex<Vec<u8>>>, tokio::sync::oneshot::Sender<()>) {
    let (send_update, update_sent) = tokio::sync::oneshot::channel();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = received.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 12];
        stream.write_all(b"RFB 003.008\n").await.unwrap();
        stream.read_exact(&mut buf).await.unwrap();
        stream.write_all(&[1, 1]).await.unwrap();
        stream.read_exact(&mut buf[..1]).await.unwrap();
        stream.write_all(&0u32.to_be_bytes()).await.unwrap();
        stream.read_exact(&mut buf[..1]).await.unwrap();
        let mut init = Vec::new();
        init.extend_from_slice(&width.to_be_bytes());
        init.extend_from_slice(&height.to_be_bytes());
        init.extend_from_slice(&[32, 24, 1, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        init.extend_from_slice(&4u32.to_be_bytes());
        init.extend_from_slice(b"desk");
        stream.write_all(&init).await.unwrap();
        // SetPixelFormat (20), SetEncodings (4 + 7 * 4), FramebufferUpdateRequest (10).
        let mut setup = vec![0u8; 62];
        stream.read_exact(&mut setup).await.unwrap();
        if update_sent.await.is_err() {
            return;
        }
        let mut update = vec![0, 0, 0, 1, 0, 0, 0, 0];
        update.extend_from_slice(&width.to_be_bytes());
        update.extend_from_slice(&height.to_be_bytes());
        update.extend_from_slice(&0i32.to_be_bytes());
        for _ in 0..usize::from(width) * usize::from(height) {
            update.extend_from_slice(&[color[0], color[1], color[2], 0]);
        }
        stream.write_all(&update).await.unwrap();
        let mut chunk = [0u8; 256];
        while let Ok(n) = stream.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            sink.locked().extend_from_slice(&chunk[..n]);
        }
    });
    (address, received, send_update)
}

/// A tiny reference RFB encoder: pictures to wire rectangles, for codec tests.
#[cfg(feature = "vnc")]
pub mod rfb {
    use crate::pixels::RgbaImage;
    use flate2::{Compress, FlushCompress};

    /// ZRLE tile subencodings the encoder picks (single colour tiles are always solid).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Zrle {
        Raw,
        Packed,
        PlainRle,
        PaletteRle,
    }

    /// A wire pixel `R G B X` with a junk padding byte.
    pub fn pixel(p: &[u8]) -> [u8; 4] {
        [p[0], p[1], p[2], 0x77]
    }

    fn pixels(img: &RgbaImage) -> impl Iterator<Item = &[u8]> {
        img.data.chunks_exact(4)
    }

    pub fn raw(img: &RgbaImage) -> Vec<u8> {
        pixels(img).flat_map(pixel).collect()
    }

    /// The first pixel as background, every other pixel a 1x1 subrectangle.
    pub fn rre(img: &RgbaImage) -> Vec<u8> {
        let bg = &img.data[..4];
        let subs: Vec<(usize, &[u8])> = pixels(img).enumerate().filter(|(_, p)| *p != bg).collect();
        let mut out = (subs.len() as u32).to_be_bytes().to_vec();
        out.extend(pixel(bg));
        for (i, p) in subs {
            out.extend(pixel(p));
            for f in [i as u32 % img.width, i as u32 / img.width, 1, 1] {
                out.extend((f as u16).to_be_bytes());
            }
        }
        out
    }

    fn tile(img: &RgbaImage, x0: u32, y0: u32, size: u32) -> RgbaImage {
        let (w, h) = (size.min(img.width - x0), size.min(img.height - y0));
        RgbaImage { width: w, height: h, data: img.crop(loa_contracts::surface::Rect { x: x0, y: y0, width: w, height: h }).to_vec() }
    }

    fn tiles(img: &RgbaImage, size: u32) -> impl Iterator<Item = RgbaImage> + '_ {
        (0..img.height).step_by(size as usize).flat_map(move |y| (0..img.width).step_by(size as usize).map(move |x| tile(img, x, y, size)))
    }

    fn colours(img: &RgbaImage) -> Vec<&[u8]> {
        let mut seen: Vec<&[u8]> = Vec::new();
        for p in pixels(img) {
            if !seen.contains(&p) {
                seen.push(p);
            }
        }
        seen
    }

    /// Solid tiles send only a changed background; two-colour tiles use foreground subrects,
    /// others alternate between raw and coloured subrects.
    pub fn hextile(img: &RgbaImage) -> Vec<u8> {
        let (mut out, mut bg): (Vec<u8>, Option<Vec<u8>>) = (Vec::new(), None);
        for (n, t) in tiles(img, 16).enumerate() {
            let c = colours(&t);
            let subs: Vec<(usize, &[u8])> = pixels(&t).enumerate().filter(|(_, p)| *p != c[0]).collect();
            if c.len() > 2 && (n % 2 == 0 || subs.len() > 255) {
                out.push(1);
                out.extend(raw(&t));
                bg = None;
                continue;
            }
            let mut flags = if bg.as_deref() == Some(c[0]) { 0 } else { 2 };
            flags |= match c.len() {
                1 => 0,
                2 => 4 | 8,
                _ => 8 | 16,
            };
            out.push(flags);
            if flags & 2 != 0 {
                out.extend(pixel(c[0]));
            }
            if flags & 4 != 0 {
                out.extend(pixel(c[1]));
            }
            if flags & 8 != 0 {
                out.push(subs.len() as u8);
                for (i, p) in subs {
                    if flags & 16 != 0 {
                        out.extend(pixel(p));
                    }
                    out.extend([((i as u32 % t.width) << 4 | i as u32 / t.width) as u8, 0]);
                }
            }
            bg = Some(c[0].to_vec());
        }
        out
    }

    fn run(out: &mut Vec<u8>, len: usize) {
        out.extend(std::iter::repeat_n(255, (len - 1) / 255));
        out.push(((len - 1) % 255) as u8);
    }

    /// 64x64 tiles with subencoding `mode`, compressed on the connection's stream `z`.
    pub fn zrle(img: &RgbaImage, mode: Zrle, z: &mut Compress) -> Vec<u8> {
        let mut tiles_out = Vec::new();
        for t in tiles(img, 64) {
            let c = colours(&t);
            let index = |p: &[u8]| c.iter().position(|q| *q == p).unwrap();
            let cpixels = |out: &mut Vec<u8>, ps: &[&[u8]]| ps.iter().for_each(|p| out.extend(&p[..3]));
            let runs: Vec<(&[u8], usize)> = pixels(&t).fold(Vec::new(), |mut runs, p| {
                match runs.last_mut() {
                    Some((q, n)) if *q == p => *n += 1,
                    _ => runs.push((p, 1)),
                }
                runs
            });
            match mode {
                _ if c.len() == 1 => {
                    tiles_out.push(1);
                    cpixels(&mut tiles_out, &c);
                }
                Zrle::Packed if c.len() <= 16 => {
                    tiles_out.push(c.len() as u8);
                    cpixels(&mut tiles_out, &c);
                    let bits = match c.len() {
                        2 => 1,
                        3..=4 => 2,
                        _ => 4,
                    };
                    for row in t.data.chunks_exact(t.width as usize * 4) {
                        let mut packed = vec![0u8; (t.width as usize * bits).div_ceil(8)];
                        for (x, p) in row.chunks_exact(4).enumerate() {
                            packed[x * bits / 8] |= (index(p) as u8) << (8 - bits - x * bits % 8);
                        }
                        tiles_out.extend(packed);
                    }
                }
                Zrle::PaletteRle if c.len() <= 127 => {
                    tiles_out.push(128 + c.len() as u8);
                    cpixels(&mut tiles_out, &c);
                    for (p, n) in runs {
                        if n == 1 {
                            tiles_out.push(index(p) as u8);
                        } else {
                            tiles_out.push(index(p) as u8 | 0x80);
                            run(&mut tiles_out, n);
                        }
                    }
                }
                Zrle::PlainRle => {
                    tiles_out.push(128);
                    for (p, n) in runs {
                        tiles_out.extend(&p[..3]);
                        run(&mut tiles_out, n);
                    }
                }
                _ => {
                    tiles_out.push(0);
                    cpixels(&mut tiles_out, &pixels(&t).collect::<Vec<_>>());
                }
            }
        }
        let mut compressed = Vec::with_capacity(tiles_out.len() * 2 + 64);
        z.compress_vec(&tiles_out, &mut compressed, FlushCompress::Sync).unwrap();
        assert!(compressed.len() < compressed.capacity(), "compressed output was cut short");
        [(compressed.len() as u32).to_be_bytes().to_vec(), compressed].concat()
    }

    /// A rectangle header and its payload.
    pub fn rect(x: u16, y: u16, w: u16, h: u16, encoding: i32, payload: &[u8]) -> Vec<u8> {
        let head = [x, y, w, h].into_iter().flat_map(u16::to_be_bytes).chain(encoding.to_be_bytes());
        head.chain(payload.iter().copied()).collect()
    }

    /// A FramebufferUpdate of `rects`.
    pub fn update(rects: &[Vec<u8>]) -> Vec<u8> {
        [vec![0, 0], (rects.len() as u16).to_be_bytes().to_vec(), rects.concat()].concat()
    }
}

/// `data:` URL of an HTML document.
pub fn html_data_url(body: &str) -> String {
    format!("data:text/html,{}", loa_util::text::percent_encode(body))
}

/// The Chrome for real-browser tests, which run only when `LOA_CHROME` is
/// set: to the browser's path, or to anything else to find the installed one.
#[cfg(feature = "browser")]
pub fn chrome() -> Option<std::path::PathBuf> {
    let wanted = std::env::var_os("LOA_CHROME")?;
    let path = std::path::PathBuf::from(&wanted);
    if path.is_file() {
        return Some(path);
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let found = crate::browser::find_chrome(std::env::var_os("PATH").as_deref(), home.as_deref(), false);
    assert!(found.is_some(), "LOA_CHROME is set but no Chrome or Chromium is installed");
    found
}

/// The Chrome for a real-browser test, or returns from the test with a
/// loud skip note when `LOA_CHROME` is unset.
#[cfg(feature = "browser")]
#[macro_export]
macro_rules! chrome_or_skip {
    () => {
        match $crate::testkit::chrome() {
            Some(chrome) => chrome,
            None => {
                eprintln!("SKIPPED: real-Chrome test; set LOA_CHROME=1 (or to a browser path) to run it");
                return;
            }
        }
    };
}
