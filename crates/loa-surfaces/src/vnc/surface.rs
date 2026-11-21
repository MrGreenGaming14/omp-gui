//! One RFB connection as a pixel [`Surface`], with reconnect.

use crate::feed::FrameFeed;
use crate::pixels::{RgbaImage, encode_png, region_fits};
use crate::vnc::decode::{Cursor, Decoders, Update, is_pseudo, read_rect, skip};
use crate::vnc::input::InputState;
use crate::vnc::proto::{self, Msg};
use crate::vnc::{PasswordSource, RfbEndpoint, RfbError, RfbOptions};
use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::surface::{
    Document, Encoding, Frame, InputEvent, Rect, Surface, SurfaceError, SurfaceInfo, SurfaceMode, SurfaceUri,
};
use loa_util::LockExt;
use loa_util::time::now_us;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

/// The local framebuffer copy and the frame numbering.
#[derive(Default)]
struct Picture {
    fb: RgbaImage,
    desktop: String,
    /// `Frame::input_seq` of the last published frame.
    input_seq: u64,
}

/// The write side of the connection and the input state it carries.
#[derive(Default)]
struct Link {
    stream: Option<OwnedWriteHalf>,
    input: InputState,
    /// `InputEvent::seq` of the last input written to the socket.
    last_input_seq: u64,
    buf: Vec<u8>,
}

struct Shared {
    uri: SurfaceUri,
    endpoint: RfbEndpoint,
    passwords: Arc<dyn PasswordSource>,
    options: RfbOptions,
    picture: std::sync::Mutex<Picture>,
    feed: FrameFeed,
    link: tokio::sync::Mutex<Link>,
    /// `Link::last_input_seq` when the latest FramebufferUpdateRequest was written.
    requested_input_seq: AtomicU64,
    cursor: watch::Sender<Option<Arc<Cursor>>>,
}

/// A connected RFB endpoint. Dropping it closes the connection.
pub struct RfbSurface {
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl Drop for RfbSurface {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RfbSurface {
    /// Connects to `endpoint` and keeps the connection up until dropped. Fails
    /// when the first connection fails; later drops are retried with backoff.
    pub(crate) async fn connect(
        uri: SurfaceUri,
        endpoint: RfbEndpoint,
        passwords: Arc<dyn PasswordSource>,
        options: RfbOptions,
    ) -> Result<Self, RfbError> {
        let shared = Arc::new(Shared {
            uri,
            endpoint,
            passwords,
            feed: FrameFeed::new(options.frame_capacity.max(1)),
            options,
            picture: Default::default(),
            link: Default::default(),
            requested_input_seq: AtomicU64::new(0),
            cursor: watch::Sender::new(None),
        });
        let reader = shared.establish().await?;
        let task = tokio::spawn(Arc::clone(&shared).supervise(reader));
        Ok(Self { shared, task })
    }

    /// The remote pointer shape, or `None` until the server sends one.
    pub fn cursor(&self) -> watch::Receiver<Option<Arc<Cursor>>> {
        self.shared.cursor.subscribe()
    }
}

#[async_trait]
impl Surface for RfbSurface {
    fn info(&self) -> SurfaceInfo {
        let pic = self.shared.picture.locked();
        let name = if pic.desktop.is_empty() { &self.shared.endpoint.name } else { &pic.desktop };
        SurfaceInfo {
            uri: self.shared.uri.clone(),
            name: name.clone(),
            width: pic.fb.width,
            height: pic.fb.height,
            mode: SurfaceMode::Pixel,
            mime: None,
        }
    }

    fn frames(&self) -> loa_contracts::surface::Result<broadcast::Receiver<Arc<Frame>>> {
        Ok(self.shared.feed.subscribe())
    }

    /// Publishes the local framebuffer copy as a keyframe right away; it is
    /// the complete picture, so no round trip to the server is needed.
    async fn request_keyframe(&self) -> loa_contracts::surface::Result<()> {
        let pic = self.shared.picture.locked();
        self.shared.send(&pic, pic.fb.full());
        Ok(())
    }

    async fn input(&self, event: InputEvent) -> loa_contracts::surface::Result<()> {
        Ok(self.shared.write(Some(&event)).await?)
    }

    async fn document(&self) -> loa_contracts::surface::Result<Document> {
        Err(SurfaceError::Mode { uri: self.shared.uri.clone(), mode: SurfaceMode::Pixel })
    }

    async fn screenshot(&self) -> loa_contracts::surface::Result<Bytes> {
        let fb = self.shared.picture.locked().fb.clone();
        let png = tokio::task::spawn_blocking(move || encode_png(fb.width, fb.height, &fb.data)).await;
        let png = png.map_err(|e| SurfaceError::Backend(e.to_string()))?;
        Ok(Bytes::from(png.map_err(|e| SurfaceError::Backend(e.to_string()))?))
    }
}

/// `a` grown to cover `b`.
fn union(a: Option<Rect>, b: Rect) -> Rect {
    let Some(a) = a else { return b };
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    Rect { x, y, width: right - x, height: bottom - y }
}

type Reader = BufReader<OwnedReadHalf>;

impl Shared {
    fn size(&self) -> (u32, u32) {
        let pic = self.picture.locked();
        (pic.fb.width, pic.fb.height)
    }

    /// Connects, runs the handshake and installs the write half.
    async fn establish(&self) -> Result<Reader, RfbError> {
        let address = &self.endpoint.address;
        let connect = async {
            let mut stream = TcpStream::connect(address.as_str()).await?;
            stream.set_nodelay(true)?;
            let init = proto::handshake(&mut stream, &self.endpoint.name, &*self.passwords, self.options.shared).await?;
            Ok::<_, RfbError>((stream, init))
        };
        let (stream, init) =
            tokio::time::timeout(self.options.connect_timeout, connect).await.map_err(|_| RfbError::Timeout(address.clone()))??;
        let (rd, wr) = stream.into_split();
        {
            let mut pic = self.picture.locked();
            let (width, height) = (u32::from(init.width), u32::from(init.height));
            if (pic.fb.width, pic.fb.height) != (width, height) {
                pic.fb = RgbaImage::filled(width, height, [0, 0, 0, 255]);
            }
            pic.desktop = init.name;
        }
        let mut link = self.link.lock().await;
        link.stream = Some(wr);
        link.input.reset();
        self.requested_input_seq.store(link.last_input_seq, Ordering::Release);
        Ok(BufReader::with_capacity(1 << 16, rd))
    }

    /// Reads until the connection fails, then reconnects with backoff, forever.
    async fn supervise(self: Arc<Self>, mut reader: Reader) {
        loop {
            let Err(error) = self.read_messages(&mut reader, &mut Decoders::new()).await;
            tracing::debug!(endpoint = %self.endpoint.name, %error, "RFB connection lost");
            self.link.lock().await.stream = None;
            let mut delay = self.options.backoff_initial;
            reader = loop {
                tokio::time::sleep(delay).await;
                match self.establish().await {
                    Ok(reader) => break reader,
                    Err(error) => {
                        tracing::debug!(endpoint = %self.endpoint.name, %error, "RFB reconnect failed");
                        delay = (delay * 2).min(self.options.backoff_max);
                    }
                }
            };
        }
    }

    /// Handles server messages until the connection fails.
    async fn read_messages(&self, rd: &mut Reader, dec: &mut Decoders) -> Result<Infallible, RfbError> {
        // The first update after connecting answers the full request the
        // handshake sent, so it is published as a keyframe.
        let mut full = true;
        loop {
            match rd.read_u8().await? {
                0 => {
                    self.read_update(rd, dec, &mut full).await?;
                    self.write(None).await?;
                }
                1 => {
                    // SetColourMapEntries: unused with a true color format.
                    skip(rd, 3).await?;
                    let count = rd.read_u16().await?;
                    skip(rd, u64::from(count) * 6).await?;
                }
                2 => {}
                3 => {
                    // ServerCutText: the clipboard is not part of the surface.
                    skip(rd, 3).await?;
                    let len = rd.read_u32().await?;
                    skip(rd, u64::from(len)).await?;
                }
                other => return Err(RfbError::Protocol(format!("unknown server message type {other}"))),
            }
        }
    }

    /// Applies one FramebufferUpdate to the framebuffer and publishes it.
    async fn read_update(&self, rd: &mut Reader, dec: &mut Decoders, full: &mut bool) -> Result<(), RfbError> {
        let mut head = [0u8; 3];
        rd.read_exact(&mut head).await?;
        let count = u16::from_be_bytes([head[1], head[2]]);
        let input_seq = self.requested_input_seq.load(Ordering::Acquire);
        let (mut width, mut height) = self.size();
        let mut dirty = None;
        for _ in 0..count {
            let r = read_rect(rd).await?;
            let encoding = rd.read_i32().await?;
            let (decoder, scratch) =
                dec.get(encoding).ok_or_else(|| RfbError::Protocol(format!("unrequested encoding {encoding}")))?;
            let inside = |r: Rect| region_fits(r, width, height);
            if !is_pseudo(encoding) && !inside(r) {
                return Err(RfbError::Protocol(format!("rectangle {r:?} outside {width}x{height}")));
            }
            match decoder.decode(r, &mut *rd, scratch).await? {
                Update::Resize => {
                    (width, height) = (r.width, r.height);
                    self.picture.locked().fb = RgbaImage::filled(width, height, [0, 0, 0, 255]);
                    *full = true;
                    dirty = None;
                    continue;
                }
                Update::Cursor(cursor) => {
                    self.cursor.send_replace(Some(Arc::new(cursor)));
                    continue;
                }
                Update::Copy { x, y } if inside(Rect { x, y, ..r }) => self.picture.locked().fb.copy_rect(x, y, r),
                Update::Copy { x, y } => return Err(RfbError::Protocol(format!("CopyRect source {x},{y} outside"))),
                Update::Blit => {
                    self.picture.locked().fb.blit(r, &scratch.data).map_err(|e| RfbError::Protocol(e.to_string()))?;
                }
            }
            if r.width > 0 && r.height > 0 {
                dirty = Some(union(dirty, r));
            }
        }

        let mut pic = self.picture.locked();
        pic.input_seq = input_seq;
        let region = if *full { Some(pic.fb.full()) } else { dirty };
        if let Some(region) = region {
            self.send(&pic, region);
        }
        *full = false;
        Ok(())
    }

    /// Publishes `region` of the framebuffer as the next frame.
    fn send(&self, pic: &Picture, region: Rect) {
        let full = pic.fb.full();
        if full.width == 0 || full.height == 0 {
            return;
        }
        self.feed.publish(Frame {
            seq: 0,
            captured_us: now_us(),
            input_seq: pic.input_seq,
            width: full.width,
            height: full.height,
            region,
            encoding: Encoding::Rgba,
            keyframe: region == full,
            data: pic.fb.crop(region),
        });
    }

    /// Writes `event` (if any) followed by an incremental update request for the whole
    /// framebuffer, so the update that shows its effect carries its sequence number.
    async fn write(&self, event: Option<&InputEvent>) -> Result<(), RfbError> {
        let (width, height) = self.size();
        let mut link = self.link.lock().await;
        let Link { stream, input, last_input_seq, buf } = &mut *link;
        let stream = stream.as_mut().ok_or(RfbError::Disconnected)?;
        buf.clear();
        if let Some(event) = event {
            input.encode(&event.input, width, height, self.options.scroll_step, buf)?;
            if buf.is_empty() {
                return Ok(());
            }
        }
        // The framebuffer size came from u16 fields.
        Msg::UpdateRequest { incremental: true, width: width as u16, height: height as u16 }.write(buf);
        stream.write_all(buf).await?;
        *last_input_seq = event.map_or(*last_input_seq, |e| e.seq);
        self.requested_input_seq.store(*last_input_seq, Ordering::Release);
        Ok(())
    }
}
