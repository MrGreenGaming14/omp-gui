//! Viewer side of `omp/surface/1`.

use bytes::{Bytes, BytesMut};
use loa_contracts::surface::{Document, Encoding, Frame, Input, InputEvent, StreamInfo, SurfaceInfo, SurfaceMode, SurfaceUri};
use loa_util::LockExt;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use crate::proto::fragment::Reassembler;
use crate::proto::transport::{Datagrams, MessageReader, MessageWriter, Transport};
use crate::proto::wire::{
    Body, Close, KeyframeRequest, Open, RejectCode, StreamInfoMsg, StreamMessage, VERSION, encoding_to_wire,
};
use crate::proto::{ProtoError, Result};
use loa_util::time::now_us;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerOptions {
    /// Frame encodings the viewer can decode; the server closes the session rather than send any other.
    pub encodings: Vec<Encoding>,
    /// Documents larger than this are refused before any byte is buffered.
    pub max_document: u64,
}

impl ViewerOptions {
    pub fn new(encodings: Vec<Encoding>) -> Self {
        Self { encodings, max_document: 512 * 1024 * 1024 }
    }
}

pub enum Opened {
    Pixel(PixelViewer),
    Document(DocumentViewer),
}

/// The next known message body; `Close` and the end of the stream end with an error.
async fn next_body(reader: &mut MessageReader) -> Result<Body> {
    loop {
        match reader.next().await?.ok_or_else(|| ProtoError::Closed("the server ended the stream".into()))?.body {
            None => continue,
            Some(Body::Close(close)) => return Err(ProtoError::Closed(close.reason)),
            Some(body) => return Ok(body),
        }
    }
}

pub async fn open(transport: Transport, uri: &SurfaceUri, options: ViewerOptions) -> Result<Opened> {
    let (mut reader, mut writer, datagrams) = transport.into_messages();
    let encodings = options.encodings.iter().map(|&e| encoding_to_wire(e)).collect();
    writer.send(Body::Open(Open { version: VERSION, uri: uri.as_str().to_owned(), encodings })).await?;
    let accept = match next_body(&mut reader).await? {
        Body::Accept(accept) => accept,
        Body::Reject(reject) => {
            let code = RejectCode::try_from(reject.code).unwrap_or(RejectCode::Unspecified);
            return Err(ProtoError::Rejected { code, reason: reject.reason });
        }
        _ => return Err(ProtoError::Protocol("expected Accept or Reject".into())),
    };
    if accept.version == 0 {
        return Err(ProtoError::Protocol("the server accepted with protocol version 0".into()));
    }
    let info: SurfaceInfo =
        accept.info.ok_or_else(|| ProtoError::Protocol("Accept without surface info".into()))?.try_into()?;
    if &info.uri != uri {
        return Err(ProtoError::Protocol(format!("asked for {uri}, the server opened {}", info.uri)));
    }
    Ok(match info.mode {
        SurfaceMode::Pixel => {
            let (out, rx) = mpsc::unbounded_channel();
            tokio::spawn(write_loop(writer, rx));
            Opened::Pixel(PixelViewer {
                info,
                stream: accept.stream.and_then(StreamInfoMsg::into_info),
                reader,
                datagrams: accept.datagrams.then_some(datagrams),
                reassembler: Reassembler::new(),
                input: InputSender(Arc::new(Mutex::new((1, out)))),
            })
        }
        SurfaceMode::Document => Opened::Document(DocumentViewer { reader, writer, max: options.max_document }),
    })
}

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
    let _ = writer.finish().await;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerEvent {
    /// The next frame to show. Older frames never follow it.
    Frame(Frame),
    /// The surface changed size; frames from now on have this size.
    Resized { width: u32, height: u32 },
}

pub struct PixelViewer {
    info: SurfaceInfo,
    stream: Option<StreamInfo>,
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

    pub fn stream(&self) -> Option<&StreamInfo> {
        self.stream.as_ref()
    }

    pub fn input(&self) -> InputSender {
        self.input.clone()
    }

    /// Cancel safe; ends with [`ProtoError::Closed`] when the server closes.
    pub async fn next_event(&mut self) -> Result<ViewerEvent> {
        loop {
            let pushed = tokio::select! {
                biased;
                body = next_body(&mut self.reader) => match body? {
                    Body::Frame(frame) => {
                        let header = frame.header.ok_or_else(|| ProtoError::Protocol("Frame without header".into()))?;
                        // Unknown encoding: skip it like a lost frame.
                        let Some(frame) = header.into_frame(frame.data) else { continue };
                        self.reassembler.push_frame(frame)
                    }
                    Body::Resize(resize) => return Ok(ViewerEvent::Resized { width: resize.width, height: resize.height }),
                    _ => return Err(ProtoError::Protocol("unexpected message in pixel mode".into())),
                },
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

    /// Ends the session, even if input senders are still held elsewhere.
    pub fn close(self) {
        let _ = self.input.send_message(Body::Close(Close { reason: "the viewer closed".into() }));
    }
}

async fn recv(datagrams: Option<&dyn Datagrams>) -> Result<Bytes> {
    match datagrams {
        Some(d) => Ok(d.recv().await?),
        None => std::future::pending().await,
    }
}

/// Sends input events of one viewer, numbered from 1 in send order.
#[derive(Clone)]
pub struct InputSender(Arc<Mutex<(u64, mpsc::UnboundedSender<StreamMessage>)>>);

impl InputSender {
    /// Returns the seq frames echo in [`Frame::input_seq`]. Never blocks.
    pub fn send(&self, input: Input) -> Result<u64> {
        let mut state = self.0.locked();
        let (seq, out) = &mut *state;
        let event = InputEvent { seq: *seq, sent_us: now_us(), input };
        out.send(Body::Input(event.into()).into()).map_err(|_| ProtoError::Closed("the session ended".into()))?;
        *seq += 1;
        Ok(*seq - 1)
    }

    fn send_message(&self, body: Body) -> Result<()> {
        self.0.locked().1.send(body.into()).map_err(|_| ProtoError::Closed("the session ended".into()))
    }
}

pub struct DocumentViewer {
    reader: MessageReader,
    writer: MessageWriter,
    max: u64,
}

impl DocumentViewer {
    /// Receives the file, checks its length and checksum, and closes the session.
    pub async fn receive(mut self) -> Result<Document> {
        let Body::DocumentHeader(header) = next_body(&mut self.reader).await? else {
            return Err(ProtoError::Protocol("expected DocumentHeader".into()));
        };
        if header.length > self.max {
            let reason = format!("document of {} bytes exceeds the {} byte limit", header.length, self.max);
            self.writer.send(Body::Close(Close { reason })).await?;
            return Err(ProtoError::TooLarge { len: header.length, max: self.max });
        }
        let mut data = BytesMut::with_capacity(header.length as usize);
        while (data.len() as u64) < header.length {
            let Body::DocumentChunk(chunk) = next_body(&mut self.reader).await? else {
                return Err(ProtoError::Protocol("expected DocumentChunk".into()));
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
}
