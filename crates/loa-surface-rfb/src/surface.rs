//! One RFB connection as a pixel [`Surface`], with reconnect.

use loa_util::time::now_us;
use loa_util::LockExt;
use crate::decode::{self, Cursor, Decoder, Framebuffer};
use crate::input::InputState;
use crate::proto::{
    self, ENC_COPY_RECT, ENC_CURSOR, ENC_DESKTOP_SIZE, ENC_HEXTILE, ENC_RAW, ENC_RRE, ENC_ZRLE,
};
use crate::{PasswordSource, RfbEndpoint, RfbError, RfbOptions};
use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::surface::{
    Document, Encoding, Frame, InputEvent, Rect, Surface, SurfaceError, SurfaceInfo, SurfaceMode,
    SurfaceUri,
};
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

/// Whether an RFB surface currently has a live connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    /// The connection dropped; `attempt` reconnects have failed so far, the
    /// last with `error`.
    Reconnecting {
        attempt: u32,
        error: String,
    },
}

/// The local framebuffer copy and the frame numbering.
struct Picture {
    fb: Framebuffer,
    desktop: String,
    seq: u64,
    /// `Frame::input_seq` of the last published frame.
    input_seq: u64,
}

/// The write side of the connection and the input state it carries.
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
    frames: broadcast::Sender<Arc<Frame>>,
    link: tokio::sync::Mutex<Link>,
    /// `Link::last_input_seq` when the latest FramebufferUpdateRequest was written.
    requested_input_seq: AtomicU64,
    cursor: watch::Sender<Option<Arc<Cursor>>>,
    state: watch::Sender<ConnectionState>,
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
        let (frames, _) = broadcast::channel(options.frame_capacity.max(1));
        let shared = Arc::new(Shared {
            uri,
            endpoint,
            passwords,
            options,
            picture: std::sync::Mutex::new(Picture {
                fb: Framebuffer::new(0, 0),
                desktop: String::new(),
                seq: 0,
                input_seq: 0,
            }),
            frames,
            link: tokio::sync::Mutex::new(Link {
                stream: None,
                input: InputState::default(),
                last_input_seq: 0,
                buf: Vec::with_capacity(256),
            }),
            requested_input_seq: AtomicU64::new(0),
            cursor: watch::Sender::new(None),
            state: watch::Sender::new(ConnectionState::Connected),
        });
        let reader = shared.establish().await?;
        let task = tokio::spawn(Arc::clone(&shared).supervise(reader));
        Ok(Self { shared, task })
    }

    /// The remote pointer shape, for drawing the cursor locally. `None` until
    /// the server sends one.
    pub fn cursor(&self) -> watch::Receiver<Option<Arc<Cursor>>> {
        self.shared.cursor.subscribe()
    }

    /// Connection state changes, for a "reconnecting" indicator.
    pub fn state(&self) -> watch::Receiver<ConnectionState> {
        self.shared.state.subscribe()
    }
}

#[async_trait]
impl Surface for RfbSurface {
    fn info(&self) -> SurfaceInfo {
        let pic = self.shared.picture.locked();
        let name = if pic.desktop.is_empty() {
            &self.shared.endpoint.name
        } else {
            &pic.desktop
        };
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
        Ok(self.shared.frames.subscribe())
    }

    /// Publishes the local framebuffer copy as a keyframe right away; it is
    /// the complete picture, so no round trip to the server is needed.
    async fn request_keyframe(&self) -> loa_contracts::surface::Result<()> {
        let mut pic = self.shared.picture.locked();
        let full = pic.fb.full();
        self.shared.send(&mut pic, full);
        Ok(())
    }

    async fn input(&self, event: InputEvent) -> loa_contracts::surface::Result<()> {
        Ok(self.shared.input(&event).await?)
    }

    async fn document(&self) -> loa_contracts::surface::Result<Document> {
        Err(SurfaceError::Mode {
            uri: self.shared.uri.clone(),
            mode: SurfaceMode::Pixel,
        })
    }

    async fn screenshot(&self) -> loa_contracts::surface::Result<Bytes> {
        let (width, height, rgba) = {
            let pic = self.shared.picture.locked();
            (pic.fb.width, pic.fb.height, pic.fb.pixels.clone())
        };
        let png = tokio::task::spawn_blocking(move || encode_png(width, height, &rgba))
            .await
            .map_err(|e| SurfaceError::Backend(e.to_string()))?;
        Ok(Bytes::from(png.map_err(RfbError::from)?))
    }
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(out)
}

/// `a` grown to cover `b`.
fn union(a: Option<Rect>, b: Rect) -> Rect {
    let Some(a) = a else { return b };
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

/// Reads and discards `len` bytes.
async fn skip<R: AsyncRead + Unpin>(rd: &mut R, len: u64) -> Result<(), RfbError> {
    let copied = tokio::io::copy(&mut rd.take(len), &mut tokio::io::sink()).await?;
    if copied < len {
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
    }
    Ok(())
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
            let init = proto::handshake(
                &mut stream,
                &self.endpoint.name,
                &*self.passwords,
                self.options.shared,
            )
            .await?;
            Ok::<_, RfbError>((stream, init))
        };
        let (stream, init) = tokio::time::timeout(self.options.connect_timeout, connect)
            .await
            .map_err(|_| RfbError::Timeout(address.clone()))??;
        let (rd, wr) = stream.into_split();
        {
            let mut pic = self.picture.locked();
            let (width, height) = (u32::from(init.width), u32::from(init.height));
            if (pic.fb.width, pic.fb.height) != (width, height) {
                pic.fb = Framebuffer::new(width, height);
            }
            pic.desktop = init.name;
        }
        let mut link = self.link.lock().await;
        link.stream = Some(wr);
        link.input.reset();
        self.requested_input_seq
            .store(link.last_input_seq, Ordering::Release);
        self.state.send_replace(ConnectionState::Connected);
        Ok(BufReader::with_capacity(1 << 16, rd))
    }

    /// Reads until the connection fails, then reconnects with backoff, forever.
    async fn supervise(self: Arc<Self>, mut reader: Reader) {
        loop {
            let Err(error) = self.read_messages(&mut reader, &mut Decoder::new()).await;
            tracing::debug!(endpoint = %self.endpoint.name, %error, "RFB connection lost");
            self.link.lock().await.stream = None;
            self.state.send_replace(ConnectionState::Reconnecting {
                attempt: 0,
                error: error.to_string(),
            });
            let mut delay = self.options.backoff_initial;
            let mut attempt = 0;
            reader = loop {
                tokio::time::sleep(delay).await;
                attempt += 1;
                match self.establish().await {
                    Ok(reader) => break reader,
                    Err(error) => {
                        tracing::debug!(endpoint = %self.endpoint.name, %error, attempt, "RFB reconnect failed");
                        self.state.send_replace(ConnectionState::Reconnecting {
                            attempt,
                            error: error.to_string(),
                        });
                        delay = (delay * 2).min(self.options.backoff_max);
                    }
                }
            };
        }
    }

    /// Handles server messages until the connection fails.
    async fn read_messages(
        &self,
        rd: &mut Reader,
        dec: &mut Decoder,
    ) -> Result<Infallible, RfbError> {
        // The first update after connecting answers the full request the
        // handshake sent, so it is published as a keyframe.
        let mut full = true;
        loop {
            match rd.read_u8().await? {
                0 => {
                    self.read_update(rd, dec, &mut full).await?;
                    self.request_update().await?;
                }
                1 => {
                    // SetColourMapEntries: unused with a true color format.
                    let mut head = [0u8; 5];
                    rd.read_exact(&mut head).await?;
                    let count = u16::from_be_bytes([head[3], head[4]]);
                    skip(rd, u64::from(count) * 6).await?;
                }
                2 => {}
                3 => {
                    // ServerCutText: the clipboard is not part of the surface.
                    let mut head = [0u8; 7];
                    rd.read_exact(&mut head).await?;
                    let len = u32::from_be_bytes([head[3], head[4], head[5], head[6]]);
                    skip(rd, u64::from(len)).await?;
                }
                other => {
                    return Err(RfbError::Protocol(format!(
                        "unknown server message type {other}"
                    )));
                }
            }
        }
    }

    /// Applies one FramebufferUpdate to the framebuffer and publishes it.
    async fn read_update(
        &self,
        rd: &mut Reader,
        dec: &mut Decoder,
        full: &mut bool,
    ) -> Result<(), RfbError> {
        let mut head = [0u8; 3];
        rd.read_exact(&mut head).await?;
        let count = u16::from_be_bytes([head[1], head[2]]);
        let input_seq = self.requested_input_seq.load(Ordering::Acquire);
        let (mut width, mut height) = self.size();
        let mut dirty = None;
        for _ in 0..count {
            let mut rh = [0u8; 12];
            rd.read_exact(&mut rh).await?;
            let field = |i: usize| u32::from(u16::from_be_bytes([rh[2 * i], rh[2 * i + 1]]));
            let r = Rect {
                x: field(0),
                y: field(1),
                width: field(2),
                height: field(3),
            };
            let encoding = i32::from_be_bytes([rh[8], rh[9], rh[10], rh[11]]);
            match encoding {
                ENC_DESKTOP_SIZE => {
                    (width, height) = (r.width, r.height);
                    self.picture.locked().fb = Framebuffer::new(width, height);
                    *full = true;
                    dirty = None;
                    continue;
                }
                ENC_CURSOR => {
                    let cursor = dec.cursor(rd, r).await?;
                    self.cursor.send_replace(Some(Arc::new(cursor)));
                    continue;
                }
                _ => {}
            }
            let inside = |r: Rect| decode::fits(r, width, height);
            if !inside(r) {
                return Err(RfbError::Protocol(format!(
                    "rectangle {r:?} outside {width}x{height}"
                )));
            }
            match encoding {
                ENC_COPY_RECT => {
                    let src_x = u32::from(rd.read_u16().await?);
                    let src_y = u32::from(rd.read_u16().await?);
                    if !inside(Rect {
                        x: src_x,
                        y: src_y,
                        ..r
                    }) {
                        return Err(RfbError::Protocol(format!(
                            "CopyRect source {src_x},{src_y} outside"
                        )));
                    }
                    self.picture.locked().fb.copy_rect(src_x, src_y, r);
                }
                ENC_RAW | ENC_RRE | ENC_HEXTILE | ENC_ZRLE => {
                    match encoding {
                        ENC_RAW => dec.raw(rd, r).await?,
                        ENC_RRE => dec.rre(rd, r).await?,
                        ENC_HEXTILE => dec.hextile(rd, r).await?,
                        _ => dec.zrle(rd, r).await?,
                    }
                    self.picture.locked().fb.blit(r, &dec.scratch);
                }
                other => return Err(RfbError::Protocol(format!("unrequested encoding {other}"))),
            }
            if r.width > 0 && r.height > 0 {
                dirty = Some(union(dirty, r));
            }
        }

        let mut pic = self.picture.locked();
        pic.input_seq = input_seq;
        let region = if *full { Some(pic.fb.full()) } else { dirty };
        if let Some(region) = region {
            self.send(&mut pic, region);
        }
        *full = false;
        Ok(())
    }

    /// Publishes `region` of the framebuffer as the next frame.
    fn send(&self, pic: &mut Picture, region: Rect) {
        let full = pic.fb.full();
        if full.width == 0 || full.height == 0 {
            return;
        }
        pic.seq += 1;
        let frame = Frame {
            seq: pic.seq,
            captured_us: now_us(),
            input_seq: pic.input_seq,
            width: full.width,
            height: full.height,
            region,
            encoding: Encoding::Rgba,
            keyframe: region == full,
            data: pic.fb.region(region),
        };
        // An error only means nobody is subscribed right now.
        let _ = self.frames.send(Arc::new(frame));
    }

    /// Asks for the next incremental update of the whole framebuffer.
    async fn request_update(&self) -> Result<(), RfbError> {
        let (width, height) = self.size();
        let mut link = self.link.lock().await;
        let Link {
            stream,
            buf,
            last_input_seq,
            ..
        } = &mut *link;
        let stream = stream.as_mut().ok_or(RfbError::Disconnected)?;
        buf.clear();
        // The framebuffer size came from u16 fields.
        proto::update_request(buf, true, width as u16, height as u16);
        stream.write_all(buf).await?;
        self.requested_input_seq
            .store(*last_input_seq, Ordering::Release);
        Ok(())
    }

    /// Writes `event` followed by an incremental update request, so the
    /// update that shows its effect carries its sequence number.
    async fn input(&self, event: &InputEvent) -> Result<(), RfbError> {
        let (width, height) = self.size();
        let mut link = self.link.lock().await;
        let Link {
            stream,
            input,
            last_input_seq,
            buf,
        } = &mut *link;
        let stream = stream.as_mut().ok_or(RfbError::Disconnected)?;
        buf.clear();
        input.encode(&event.input, width, height, self.options.scroll_step, buf)?;
        if buf.is_empty() {
            return Ok(());
        }
        proto::update_request(buf, true, width as u16, height as u16);
        stream.write_all(buf).await?;
        *last_input_seq = event.seq;
        self.requested_input_seq.store(event.seq, Ordering::Release);
        Ok(())
    }
}
