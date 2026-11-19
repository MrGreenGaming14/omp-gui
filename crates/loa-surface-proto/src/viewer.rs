//! Viewer side of `omp/surface/1`: opens a surface and gets either a
//! [`PixelViewer`] (frames in, input out) or a [`DocumentViewer`] (the file,
//! once).

use bytes::BytesMut;
use loa_surface::{Document, Encoding, Frame, Input, InputEvent, SurfaceInfo, SurfaceUri};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use crate::fragment::Reassembler;
use crate::transport::{Datagrams, MessageReader, MessageWriter, Transport};
use crate::wire::{Body, Close, KeyframeRequest, Open, RejectCode, StreamMessage, VERSION, encoding_to_wire};
use crate::{ProtoError, Result, now_us};

/// Largest document accepted when none is given.
pub const DEFAULT_MAX_DOCUMENT: u64 = 512 * 1024 * 1024;

/// What the viewer can handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerOptions {
    /// Frame encodings the viewer can decode. The server closes the session
    /// rather than send any other.
    pub encodings: Vec<Encoding>,
    /// Documents larger than this are refused before any byte is buffered.
    pub max_document: u64,
}

impl ViewerOptions {
    pub fn new(encodings: Vec<Encoding>) -> Self {
        Self { encodings, max_document: DEFAULT_MAX_DOCUMENT }
    }
}

/// An opened surface; its mode decides what the viewer can do with it.
pub enum Opened {
    Pixel(PixelViewer),
    Document(DocumentViewer),
}

impl Opened {
    pub fn info(&self) -> &SurfaceInfo {
        match self {
            Opened::Pixel(v) => &v.info,
            Opened::Document(v) => &v.info,
        }
    }
}

/// Opens `uri` on the server at the other end of `transport`.
pub async fn open(transport: Transport, uri: &SurfaceUri, options: ViewerOptions) -> Result<Opened> {
    let mut reader = MessageReader::new(transport.reader);
    let mut writer = MessageWriter::new(transport.writer);
    let open = Open {
        version: VERSION,
        uri: uri.as_str().to_owned(),
        encodings: options.encodings.iter().map(|&e| encoding_to_wire(e) as i32).collect(),
    };
    writer.send(Body::Open(open)).await?;
    let accept = loop {
        let msg = reader.next().await?.ok_or_else(|| ProtoError::Closed("the server left during open".into()))?;
        match msg.body {
            Some(Body::Accept(accept)) => break accept,
            Some(Body::Reject(reject)) => {
                let code = RejectCode::try_from(reject.code).unwrap_or(RejectCode::Unspecified);
                return Err(ProtoError::Rejected { code, reason: reject.reason });
            }
            Some(Body::Close(close)) => return Err(ProtoError::Closed(close.reason)),
            None => continue,
            Some(_) => return Err(ProtoError::Protocol("expected Accept or Reject".into())),
        }
    };
    if accept.version == 0 {
        return Err(ProtoError::Protocol("the server accepted with protocol version 0".into()));
    }
    let info: SurfaceInfo =
        accept.info.ok_or_else(|| ProtoError::Protocol("Accept without surface info".into()))?.try_into()?;
    if &info.uri != uri {
        return Err(ProtoError::Protocol(format!("asked for {uri}, the server opened {}", info.uri)));
    }
    match info.mode {
        loa_surface::SurfaceMode::Pixel => {
            let (out, rx) = mpsc::unbounded_channel();
            tokio::spawn(write_loop(writer, rx));
            let size = (info.width, info.height);
            Ok(Opened::Pixel(PixelViewer {
                info,
                size,
                reader,
                datagrams: accept.datagrams.then_some(transport.datagrams),
                reassembler: Reassembler::new(),
                input: InputSender { inner: Arc::new(Mutex::new(InputState { next_seq: 1, out })) },
            }))
        }
        loa_surface::SurfaceMode::Document => {
            Ok(Opened::Document(DocumentViewer { info, reader, writer, max: options.max_document }))
        }
    }
}

/// Writes queued messages to the stream until every sender is gone.
async fn write_loop(mut writer: MessageWriter, mut rx: mpsc::UnboundedReceiver<StreamMessage>) {
    while let Some(msg) = rx.recv().await {
        let close = matches!(msg.body, Some(Body::Close(_)));
        if writer.send(msg).await.is_err() {
            return;
        }
        if close {
            break;
        }
    }
    // The session is over; a failed shutdown has nobody left to tell.
    let _ = writer.finish().await;
}

/// What a pixel session produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerEvent {
    /// The next frame to show. Older frames never follow it.
    Frame(Frame),
    /// The surface changed size; frames from now on have this size.
    Resized { width: u32, height: u32 },
}

/// A live pixel surface.
pub struct PixelViewer {
    info: SurfaceInfo,
    size: (u32, u32),
    reader: MessageReader,
    /// `None` when frames come on the stream (TCP fallback).
    datagrams: Option<Arc<dyn Datagrams>>,
    reassembler: Reassembler,
    input: InputSender,
}

impl PixelViewer {
    pub fn info(&self) -> &SurfaceInfo {
        &self.info
    }

    /// Current surface size.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Sends input to the surface. Clone it into the UI; it works while
    /// [`next_event`](Self::next_event) is waiting.
    pub fn input(&self) -> InputSender {
        self.input.clone()
    }

    /// Waits for the next frame or size change. Cancel safe. Ends with
    /// [`ProtoError::Closed`] when the server closes the session.
    pub async fn next_event(&mut self) -> Result<ViewerEvent> {
        loop {
            let pushed = tokio::select! {
                biased;
                msg = self.reader.next() => {
                    let Some(msg) = msg? else {
                        return Err(ProtoError::Closed("the server ended the stream".into()));
                    };
                    match msg.body {
                        Some(Body::Frame(frame)) => {
                            let header = frame.header.ok_or_else(|| ProtoError::Protocol("Frame without header".into()))?;
                            match header.into_frame(frame.data) {
                                Some(frame) => self.reassembler.push_frame(frame),
                                // An encoding from a newer version: skip it like a lost frame.
                                None => continue,
                            }
                        }
                        Some(Body::Resize(resize)) => {
                            self.size = (resize.width, resize.height);
                            return Ok(ViewerEvent::Resized { width: resize.width, height: resize.height });
                        }
                        Some(Body::Close(close)) => return Err(ProtoError::Closed(close.reason)),
                        None => continue,
                        Some(_) => return Err(ProtoError::Protocol("unexpected message in pixel mode".into())),
                    }
                }
                datagram = recv(self.datagrams.as_deref()), if self.datagrams.is_some() => {
                    self.reassembler.push_datagram(datagram?)
                }
            };
            if pushed.request_keyframe {
                self.input.send_message(Body::KeyframeRequest(KeyframeRequest {}))?;
            }
            if let Some(frame) = pushed.frame {
                return Ok(ViewerEvent::Frame(frame));
            }
        }
    }

    /// Ends the session.
    pub fn close(self) {
        // The writer may be gone already, which ends the session too.
        let _ = self.input.send_message(Body::Close(Close { reason: "the viewer closed".into() }));
    }
}

async fn recv(datagrams: Option<&dyn Datagrams>) -> Result<bytes::Bytes> {
    match datagrams {
        Some(d) => Ok(d.recv().await?),
        None => std::future::pending().await,
    }
}

/// Sends input events of one viewer, numbered from 1 in send order.
#[derive(Clone)]
pub struct InputSender {
    inner: Arc<Mutex<InputState>>,
}

struct InputState {
    next_seq: u64,
    out: mpsc::UnboundedSender<StreamMessage>,
}

impl InputSender {
    /// Queues `input` and returns its seq, which frames echo back in
    /// [`Frame::input_seq`]. Never blocks.
    pub fn send(&self, input: Input) -> Result<u64> {
        let mut state = self.lock();
        let seq = state.next_seq;
        let event = InputEvent { seq, sent_us: now_us(), input };
        state
            .out
            .send(Body::Input(event.into()).into())
            .map_err(|_| ProtoError::Closed("the session ended".into()))?;
        state.next_seq += 1;
        Ok(seq)
    }

    fn send_message(&self, body: Body) -> Result<()> {
        self.lock().out.send(body.into()).map_err(|_| ProtoError::Closed("the session ended".into()))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, InputState> {
        // The lock is held only around a counter bump and a channel send,
        // neither of which panics, so a poisoned lock still holds valid state.
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// An opened document surface. It never opens the datagram path.
pub struct DocumentViewer {
    info: SurfaceInfo,
    reader: MessageReader,
    writer: MessageWriter,
    max: u64,
}

impl DocumentViewer {
    pub fn info(&self) -> &SurfaceInfo {
        &self.info
    }

    /// Receives the file, checks its length and checksum, and closes the
    /// session.
    pub async fn receive(mut self) -> Result<Document> {
        let Body::DocumentHeader(header) = self.next_body().await? else {
            return Err(ProtoError::Protocol("expected DocumentHeader".into()));
        };
        if header.length > self.max {
            let reason = format!("document of {} bytes exceeds the {} byte limit", header.length, self.max);
            self.writer.send(Body::Close(Close { reason })).await?;
            return Err(ProtoError::TooLarge { len: header.length, max: self.max });
        }
        // Bounded by `max` above.
        let mut data = BytesMut::with_capacity(header.length as usize);
        while (data.len() as u64) < header.length {
            let chunk = match self.next_body().await? {
                Body::DocumentChunk(chunk) => chunk,
                _ => return Err(ProtoError::Protocol("expected DocumentChunk".into())),
            };
            if chunk.offset != data.len() as u64 || data.len() as u64 + chunk.data.len() as u64 > header.length {
                return Err(ProtoError::Protocol("document chunk out of place".into()));
            }
            data.extend_from_slice(&chunk.data);
        }
        if crc32fast::hash(&data) != header.crc32 {
            return Err(ProtoError::Checksum);
        }
        self.writer.send(Body::Close(Close { reason: "received".into() })).await?;
        self.writer.finish().await?;
        Ok(Document { name: header.name, mime: header.mime, data: data.freeze() })
    }

    /// Next known message body; `Close` ends with an error.
    async fn next_body(&mut self) -> Result<Body> {
        loop {
            let msg = self.reader.next().await?.ok_or_else(|| ProtoError::Closed("the server ended the stream".into()))?;
            match msg.body {
                None => continue,
                Some(Body::Close(close)) => return Err(ProtoError::Closed(close.reason)),
                Some(body) => return Ok(body),
            }
        }
    }
}
