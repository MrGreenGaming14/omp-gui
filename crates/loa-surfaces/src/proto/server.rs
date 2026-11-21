//! Server side of `omp/surface/1`: answers one viewer's `Open` and streams
//! the surface until either side closes.

use loa_contracts::surface::{Encoding, Frame, Surface, SurfaceMode};
use std::sync::Arc;
use tokio::sync::broadcast::{Receiver, error::RecvError};

use crate::proto::fragment::fragment;
use crate::proto::transport::{DatagramError, Datagrams, MessageReader, MessageWriter, Transport};
use crate::proto::wire::{
    Accept, Body, Close, DocumentChunk, DocumentHeader, FrameHeader, FrameMsg, Open, Reject, RejectCode, Resize,
    VERSION, encoding_from_wire,
};
use crate::proto::{ProtoError, Result};

/// Bytes of the file per document chunk.
const DOCUMENT_CHUNK: usize = 64 * 1024;

/// Serves `surface` to the viewer on `transport`. The viewer must open
/// exactly this surface's uri. Returns when the viewer closes or the
/// surface ends.
pub async fn serve(surface: Arc<dyn Surface>, transport: Transport) -> Result<()> {
    let (reader, writer) = (MessageReader::new(transport.reader), MessageWriter::new(transport.writer));
    let mut session = Session { reader, writer, datagrams: transport.datagrams };
    let Some(open) = session.read_open().await? else { return Ok(()) };
    let uri = surface.info().uri;
    if open.uri != uri.as_str() {
        return session.reject(RejectCode::NotFound, format!("this connection serves {uri}")).await;
    }
    session.run(surface, open).await
}

struct Session {
    reader: MessageReader,
    writer: MessageWriter,
    datagrams: Arc<dyn Datagrams>,
}

impl Session {
    /// The viewer's `Open`, skipping unknown messages. `None` if the viewer
    /// left before sending one or its version was rejected.
    async fn read_open(&mut self) -> Result<Option<Open>> {
        loop {
            let Some(msg) = self.reader.next().await? else { return Ok(None) };
            match msg.body {
                None => continue,
                Some(Body::Open(open)) if open.version == 0 => {
                    let reason = format!("protocol version 0 is not supported, this server speaks {VERSION}");
                    self.reject(RejectCode::Version, reason).await?;
                    return Ok(None);
                }
                Some(Body::Open(open)) => return Ok(Some(open)),
                Some(_) => return Err(self.violation("expected Open").await),
            }
        }
    }

    async fn reject(&mut self, code: RejectCode, reason: String) -> Result<()> {
        self.writer.send(Body::Reject(Reject { code: code as i32, reason })).await?;
        self.writer.finish().await
    }

    async fn close(&mut self, reason: impl Into<String>) -> Result<()> {
        self.writer.send(Body::Close(Close { reason: reason.into() })).await
    }

    /// Tells the viewer why the session ends and returns the error.
    async fn violation(&mut self, what: &str) -> ProtoError {
        // The session is over either way; the viewer may already be gone.
        let _ = self.close(format!("protocol violation: {what}")).await;
        ProtoError::Protocol(what.to_owned())
    }

    async fn run(mut self, surface: Arc<dyn Surface>, open: Open) -> Result<()> {
        let info = surface.info();
        let datagrams = info.mode == SurfaceMode::Pixel && self.datagrams.max_size().is_some();
        let encodings: Vec<Encoding> = open.encodings.iter().filter_map(|&e| encoding_from_wire(e)).collect();
        // Subscribe before accepting so no frame after the keyframe is
        // missed, and pick the encoding from what this viewer decodes.
        let (frames, stream) = match info.mode {
            SurfaceMode::Pixel => (Some(surface.frames()?), surface.negotiate(&encodings).await?),
            SurfaceMode::Document => (None, None),
        };
        let stream = stream.as_ref().map(Into::into);
        let accept = Accept { version: open.version.min(VERSION), info: Some((&info).into()), datagrams, stream };
        self.writer.send(Body::Accept(accept)).await?;
        match frames {
            Some(frames) => self.pixel(surface, frames, encodings, datagrams, (info.width, info.height)).await,
            None => self.document(surface).await,
        }
    }

    async fn pixel(
        mut self,
        surface: Arc<dyn Surface>,
        mut frames: Receiver<Arc<Frame>>,
        encodings: Vec<Encoding>,
        use_datagrams: bool,
        mut size: (u32, u32),
    ) -> Result<()> {
        // The viewer starts with nothing: give it a decodable picture.
        surface.request_keyframe().await?;
        loop {
            tokio::select! {
                biased;
                msg = self.reader.next() => {
                    let Some(msg) = msg? else { return Ok(()) };
                    match msg.body {
                        Some(Body::Input(input)) => {
                            // Kinds from a newer version are skipped.
                            if let Some(event) = input.into_event() {
                                surface.input(event).await?;
                            }
                        }
                        Some(Body::KeyframeRequest(_)) => surface.request_keyframe().await?,
                        Some(Body::Close(_)) => return Ok(()),
                        None => {}
                        Some(_) => return Err(self.violation("unexpected message in pixel mode").await),
                    }
                }
                frame = frames.recv() => match frame {
                    Ok(frame) => {
                        if !encodings.contains(&frame.encoding) {
                            let reason = format!("the viewer can't decode {:?} frames", frame.encoding);
                            self.close(reason.clone()).await?;
                            return Err(ProtoError::Closed(reason));
                        }
                        if size != (frame.width, frame.height) {
                            size = (frame.width, frame.height);
                            self.writer.send(Body::Resize(Resize { width: size.0, height: size.1 })).await?;
                        }
                        if !use_datagrams {
                            let msg = FrameMsg { header: Some(FrameHeader::from(&*frame)), data: frame.data.clone() };
                            self.writer.send(Body::Frame(msg)).await?;
                        } else if !self.send_datagrams(&frame)? {
                            return Ok(());
                        }
                    }
                    // This session fell behind the surface: frames were
                    // skipped, so deltas can't be decoded until a keyframe.
                    Err(RecvError::Lagged(_)) => surface.request_keyframe().await?,
                    Err(RecvError::Closed) => {
                        self.close("the surface closed").await?;
                        return self.writer.finish().await;
                    }
                },
            }
        }
    }

    /// Sends one frame as datagrams. `false` when the connection is gone.
    fn send_datagrams(&self, frame: &Frame) -> Result<bool> {
        let max = self.datagrams.max_size().ok_or(DatagramError::Unsupported)?;
        match fragment(frame, max, |datagram| Ok(self.datagrams.send(datagram)?)) {
            Ok(()) => Ok(true),
            Err(ProtoError::Datagram(DatagramError::Closed)) => Ok(false),
            // The path shrank under this frame. It is late anyway: drop it,
            // the next frame is cut for the new limit.
            Err(ProtoError::Datagram(DatagramError::TooLarge { .. })) => Ok(true),
            Err(e) => Err(e),
        }
    }

    /// Sends the file once on the stream, then waits for the viewer to close.
    async fn document(mut self, surface: Arc<dyn Surface>) -> Result<()> {
        let doc = surface.document().await?;
        let length = doc.data.len() as u64;
        let header = DocumentHeader { name: doc.name, mime: doc.mime, length, crc32: crc32fast::hash(&doc.data) };
        self.writer.send(Body::DocumentHeader(header)).await?;
        for offset in (0..doc.data.len()).step_by(DOCUMENT_CHUNK) {
            let data = doc.data.slice(offset..(offset + DOCUMENT_CHUNK).min(doc.data.len()));
            self.writer.send(Body::DocumentChunk(DocumentChunk { offset: offset as u64, data })).await?;
        }
        loop {
            match self.reader.next().await?.map(|msg| msg.body) {
                None => return Ok(()),
                Some(Some(Body::Close(_))) => return self.writer.finish().await,
                Some(None) => {}
                Some(Some(_)) => return Err(self.violation("unexpected message in document mode").await),
            }
        }
    }
}
