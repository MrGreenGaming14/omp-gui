//! Protobuf messages of `omp/surface/1` and their conversions to the
//! contract types.
//!
//! The reliable stream carries length delimited [`StreamMessage`]s. Each
//! datagram carries one [`Datagram`]. A message whose body has a tag this
//! version does not know decodes with `body: None` and is skipped, so newer
//! peers can add message types without breaking older ones.

use bytes::Bytes;
use loa_contracts::surface::{
    Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, Rect, StreamInfo, SurfaceInfo, SurfaceMode,
    SurfaceUri,
};

use crate::{ProtoError, Result};

/// Protocol revision spoken by this crate. The ALPN carries the major
/// version; this field lets `/1` peers add messages compatibly.
pub const VERSION: u32 = 1;

/// One message on the reliable, ordered stream.
#[derive(Clone, PartialEq, prost::Message)]
pub struct StreamMessage {
    #[prost(oneof = "Body", tags = "1, 2, 3, 4, 5, 6, 7, 8, 9, 10")]
    pub body: Option<Body>,
}

#[derive(Clone, PartialEq, prost::Oneof)]
pub enum Body {
    /// Viewer to server, first message.
    #[prost(message, tag = "1")]
    Open(Open),
    /// Server to viewer, answer to `Open`.
    #[prost(message, tag = "2")]
    Accept(Accept),
    /// Server to viewer, answer to `Open`. The server then ends the stream.
    #[prost(message, tag = "3")]
    Reject(Reject),
    /// Viewer to server, pixel mode.
    #[prost(message, tag = "4")]
    Input(InputMsg),
    /// Viewer to server, pixel mode: the picture can't be decoded until the
    /// next keyframe.
    #[prost(message, tag = "5")]
    KeyframeRequest(KeyframeRequest),
    /// Server to viewer, pixel mode: the surface changed size.
    #[prost(message, tag = "6")]
    Resize(Resize),
    /// Server to viewer, pixel mode, only when the connection has no
    /// datagrams (TCP fallback).
    #[prost(message, tag = "7")]
    Frame(FrameMsg),
    /// Server to viewer, document mode: what follows.
    #[prost(message, tag = "8")]
    DocumentHeader(DocumentHeader),
    /// Server to viewer, document mode: consecutive slices of the file.
    #[prost(message, tag = "9")]
    DocumentChunk(DocumentChunk),
    /// Either side: the session ends.
    #[prost(message, tag = "10")]
    Close(Close),
}

impl From<Body> for StreamMessage {
    fn from(body: Body) -> Self {
        Self { body: Some(body) }
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Open {
    #[prost(uint32, tag = "1")]
    pub version: u32,
    #[prost(string, tag = "2")]
    pub uri: String,
    /// Frame encodings the viewer can decode.
    #[prost(enumeration = "WireEncoding", repeated, tag = "3")]
    pub encodings: Vec<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Accept {
    /// Lower of the two peers' versions.
    #[prost(uint32, tag = "1")]
    pub version: u32,
    #[prost(message, optional, tag = "2")]
    pub info: Option<SurfaceInfoMsg>,
    /// Pixel mode: frames come as datagrams. Otherwise on the stream.
    #[prost(bool, tag = "3")]
    pub datagrams: bool,
    /// Pixel mode: what the server streams for this viewer, chosen from the
    /// viewer's `Open.encodings`. Absent when the surface doesn't say.
    #[prost(message, optional, tag = "4")]
    pub stream: Option<StreamInfoMsg>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct StreamInfoMsg {
    #[prost(enumeration = "WireEncoding", tag = "1")]
    pub encoding: i32,
    /// e.g. `h264_vaapi` or `rusty_h264`.
    #[prost(string, tag = "2")]
    pub encoder: String,
    #[prost(bool, tag = "3")]
    pub hardware: bool,
}

impl From<&StreamInfo> for StreamInfoMsg {
    fn from(stream: &StreamInfo) -> Self {
        Self { encoding: encoding_to_wire(stream.encoding) as i32, encoder: stream.encoder.clone(), hardware: stream.hardware }
    }
}

impl StreamInfoMsg {
    /// `None` when the encoding is unknown to this version.
    pub fn into_info(self) -> Option<StreamInfo> {
        Some(StreamInfo { encoding: encoding_from_wire(self.encoding)?, encoder: self.encoder, hardware: self.hardware })
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Reject {
    #[prost(enumeration = "RejectCode", tag = "1")]
    pub code: i32,
    #[prost(string, tag = "2")]
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, prost::Enumeration)]
#[repr(i32)]
pub enum RejectCode {
    Unspecified = 0,
    /// The viewer's protocol version is not supported.
    Version = 1,
    /// No such surface.
    NotFound = 2,
    /// The server failed to open the surface.
    Unavailable = 3,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SurfaceInfoMsg {
    #[prost(string, tag = "1")]
    pub uri: String,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(uint32, tag = "3")]
    pub width: u32,
    #[prost(uint32, tag = "4")]
    pub height: u32,
    #[prost(enumeration = "WireMode", tag = "5")]
    pub mode: i32,
    #[prost(string, optional, tag = "6")]
    pub mime: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, prost::Enumeration)]
#[repr(i32)]
pub enum WireMode {
    Unspecified = 0,
    Pixel = 1,
    Document = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, prost::Enumeration)]
#[repr(i32)]
pub enum WireEncoding {
    Unspecified = 0,
    Rgba = 1,
    Jpeg = 2,
    Png = 3,
    H264 = 4,
    Hevc = 5,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct KeyframeRequest {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Resize {
    #[prost(uint32, tag = "1")]
    pub width: u32,
    #[prost(uint32, tag = "2")]
    pub height: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Close {
    #[prost(string, tag = "1")]
    pub reason: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputMsg {
    #[prost(uint64, tag = "1")]
    pub seq: u64,
    #[prost(uint64, tag = "2")]
    pub sent_us: u64,
    #[prost(oneof = "InputKind", tags = "3, 4, 5, 6, 7, 8")]
    pub kind: Option<InputKind>,
}

#[derive(Clone, PartialEq, prost::Oneof)]
pub enum InputKind {
    #[prost(message, tag = "3")]
    MouseMove(MouseMoveMsg),
    #[prost(message, tag = "4")]
    MouseButton(MouseButtonMsg),
    #[prost(message, tag = "5")]
    Scroll(ScrollMsg),
    #[prost(message, tag = "6")]
    Key(KeyMsg),
    #[prost(message, tag = "7")]
    Text(TextMsg),
    #[prost(message, tag = "8")]
    Resize(Resize),
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct MouseMoveMsg {
    #[prost(float, tag = "1")]
    pub x: f32,
    #[prost(float, tag = "2")]
    pub y: f32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct MouseButtonMsg {
    #[prost(float, tag = "1")]
    pub x: f32,
    #[prost(float, tag = "2")]
    pub y: f32,
    #[prost(enumeration = "WireButton", tag = "3")]
    pub button: i32,
    #[prost(bool, tag = "4")]
    pub pressed: bool,
    /// [`MOD_SHIFT`] | [`MOD_CTRL`] | [`MOD_ALT`] | [`MOD_META`].
    #[prost(uint32, tag = "5")]
    pub modifiers: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ScrollMsg {
    #[prost(float, tag = "1")]
    pub x: f32,
    #[prost(float, tag = "2")]
    pub y: f32,
    #[prost(float, tag = "3")]
    pub dx: f32,
    #[prost(float, tag = "4")]
    pub dy: f32,
    #[prost(uint32, tag = "5")]
    pub modifiers: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct KeyMsg {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub code: String,
    #[prost(bool, tag = "3")]
    pub pressed: bool,
    #[prost(uint32, tag = "4")]
    pub modifiers: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct TextMsg {
    #[prost(string, tag = "1")]
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, prost::Enumeration)]
#[repr(i32)]
pub enum WireButton {
    Unspecified = 0,
    Left = 1,
    Middle = 2,
    Right = 3,
    Back = 4,
    Forward = 5,
}

pub const MOD_SHIFT: u32 = 1;
pub const MOD_CTRL: u32 = 1 << 1;
pub const MOD_ALT: u32 = 1 << 2;
pub const MOD_META: u32 = 1 << 3;

/// Everything about a frame except its pixels.
#[derive(Clone, PartialEq, prost::Message)]
pub struct FrameHeader {
    #[prost(uint64, tag = "1")]
    pub seq: u64,
    #[prost(uint64, tag = "2")]
    pub captured_us: u64,
    #[prost(uint64, tag = "3")]
    pub input_seq: u64,
    #[prost(uint32, tag = "4")]
    pub width: u32,
    #[prost(uint32, tag = "5")]
    pub height: u32,
    #[prost(uint32, tag = "6")]
    pub region_x: u32,
    #[prost(uint32, tag = "7")]
    pub region_y: u32,
    #[prost(uint32, tag = "8")]
    pub region_width: u32,
    #[prost(uint32, tag = "9")]
    pub region_height: u32,
    #[prost(enumeration = "WireEncoding", tag = "10")]
    pub encoding: i32,
    #[prost(bool, tag = "11")]
    pub keyframe: bool,
}

/// A whole frame on the stream.
#[derive(Clone, PartialEq, prost::Message)]
pub struct FrameMsg {
    #[prost(message, optional, tag = "1")]
    pub header: Option<FrameHeader>,
    #[prost(bytes = "bytes", tag = "2")]
    pub data: Bytes,
}

/// One unreliable datagram.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Datagram {
    #[prost(oneof = "DatagramKind", tags = "1")]
    pub kind: Option<DatagramKind>,
}

#[derive(Clone, PartialEq, prost::Oneof)]
pub enum DatagramKind {
    #[prost(message, tag = "1")]
    Fragment(Fragment),
}

/// Slice `index` of `count` of frame `seq`. Fragment 0 carries the header.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Fragment {
    #[prost(uint64, tag = "1")]
    pub seq: u64,
    #[prost(uint32, tag = "2")]
    pub index: u32,
    #[prost(uint32, tag = "3")]
    pub count: u32,
    #[prost(message, optional, tag = "4")]
    pub header: Option<FrameHeader>,
    #[prost(bytes = "bytes", tag = "5")]
    pub data: Bytes,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DocumentHeader {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub mime: String,
    /// Total bytes that follow in chunks.
    #[prost(uint64, tag = "3")]
    pub length: u64,
    /// CRC-32 (IEEE) of the whole file.
    #[prost(uint32, tag = "4")]
    pub crc32: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DocumentChunk {
    /// Position of `data` in the file. Chunks arrive in order.
    #[prost(uint64, tag = "1")]
    pub offset: u64,
    #[prost(bytes = "bytes", tag = "2")]
    pub data: Bytes,
}

pub fn encoding_to_wire(encoding: Encoding) -> WireEncoding {
    match encoding {
        Encoding::Rgba => WireEncoding::Rgba,
        Encoding::Jpeg => WireEncoding::Jpeg,
        Encoding::Png => WireEncoding::Png,
        Encoding::H264 => WireEncoding::H264,
        Encoding::Hevc => WireEncoding::Hevc,
    }
}

/// `None` for encodings this version does not know.
pub fn encoding_from_wire(value: i32) -> Option<Encoding> {
    match WireEncoding::try_from(value).ok()? {
        WireEncoding::Unspecified => None,
        WireEncoding::Rgba => Some(Encoding::Rgba),
        WireEncoding::Jpeg => Some(Encoding::Jpeg),
        WireEncoding::Png => Some(Encoding::Png),
        WireEncoding::H264 => Some(Encoding::H264),
        WireEncoding::Hevc => Some(Encoding::Hevc),
    }
}

/// Video codecs need every frame since the last keyframe to decode.
pub fn is_video(encoding: Encoding) -> bool {
    matches!(encoding, Encoding::H264 | Encoding::Hevc)
}

impl From<&SurfaceInfo> for SurfaceInfoMsg {
    fn from(info: &SurfaceInfo) -> Self {
        Self {
            uri: info.uri.as_str().to_owned(),
            name: info.name.clone(),
            width: info.width,
            height: info.height,
            mode: match info.mode {
                SurfaceMode::Pixel => WireMode::Pixel,
                SurfaceMode::Document => WireMode::Document,
            } as i32,
            mime: info.mime.clone(),
        }
    }
}

impl TryFrom<SurfaceInfoMsg> for SurfaceInfo {
    type Error = ProtoError;

    fn try_from(msg: SurfaceInfoMsg) -> Result<Self> {
        let mode = match WireMode::try_from(msg.mode) {
            Ok(WireMode::Pixel) => SurfaceMode::Pixel,
            Ok(WireMode::Document) => SurfaceMode::Document,
            _ => return Err(ProtoError::Protocol(format!("unknown surface mode {}", msg.mode))),
        };
        Ok(Self {
            uri: msg.uri.parse::<SurfaceUri>()?,
            name: msg.name,
            width: msg.width,
            height: msg.height,
            mode,
            mime: msg.mime,
        })
    }
}

impl From<&Frame> for FrameHeader {
    fn from(frame: &Frame) -> Self {
        Self {
            seq: frame.seq,
            captured_us: frame.captured_us,
            input_seq: frame.input_seq,
            width: frame.width,
            height: frame.height,
            region_x: frame.region.x,
            region_y: frame.region.y,
            region_width: frame.region.width,
            region_height: frame.region.height,
            encoding: encoding_to_wire(frame.encoding) as i32,
            keyframe: frame.keyframe,
        }
    }
}

impl FrameHeader {
    /// `None` when the encoding is unknown to this version.
    pub fn into_frame(self, data: Bytes) -> Option<Frame> {
        Some(Frame {
            seq: self.seq,
            captured_us: self.captured_us,
            input_seq: self.input_seq,
            width: self.width,
            height: self.height,
            region: Rect { x: self.region_x, y: self.region_y, width: self.region_width, height: self.region_height },
            encoding: encoding_from_wire(self.encoding)?,
            keyframe: self.keyframe,
            data,
        })
    }
}

fn modifiers_to_wire(m: Modifiers) -> u32 {
    let mut bits = 0;
    for (on, bit) in [(m.shift, MOD_SHIFT), (m.ctrl, MOD_CTRL), (m.alt, MOD_ALT), (m.meta, MOD_META)] {
        if on {
            bits |= bit;
        }
    }
    bits
}

fn modifiers_from_wire(bits: u32) -> Modifiers {
    Modifiers {
        shift: bits & MOD_SHIFT != 0,
        ctrl: bits & MOD_CTRL != 0,
        alt: bits & MOD_ALT != 0,
        meta: bits & MOD_META != 0,
    }
}

fn button_to_wire(button: MouseButton) -> WireButton {
    match button {
        MouseButton::Left => WireButton::Left,
        MouseButton::Middle => WireButton::Middle,
        MouseButton::Right => WireButton::Right,
        MouseButton::Back => WireButton::Back,
        MouseButton::Forward => WireButton::Forward,
    }
}

fn button_from_wire(value: i32) -> Option<MouseButton> {
    match WireButton::try_from(value).ok()? {
        WireButton::Unspecified => None,
        WireButton::Left => Some(MouseButton::Left),
        WireButton::Middle => Some(MouseButton::Middle),
        WireButton::Right => Some(MouseButton::Right),
        WireButton::Back => Some(MouseButton::Back),
        WireButton::Forward => Some(MouseButton::Forward),
    }
}

impl From<InputEvent> for InputMsg {
    fn from(event: InputEvent) -> Self {
        let kind = match event.input {
            Input::MouseMove { x, y } => InputKind::MouseMove(MouseMoveMsg { x, y }),
            Input::MouseButton { x, y, button, pressed, modifiers } => InputKind::MouseButton(MouseButtonMsg {
                x,
                y,
                button: button_to_wire(button) as i32,
                pressed,
                modifiers: modifiers_to_wire(modifiers),
            }),
            Input::Scroll { x, y, dx, dy, modifiers } => {
                InputKind::Scroll(ScrollMsg { x, y, dx, dy, modifiers: modifiers_to_wire(modifiers) })
            }
            Input::Key { key, code, pressed, modifiers } => {
                InputKind::Key(KeyMsg { key, code, pressed, modifiers: modifiers_to_wire(modifiers) })
            }
            Input::Text { text } => InputKind::Text(TextMsg { text }),
            Input::Resize { width, height } => InputKind::Resize(Resize { width, height }),
        };
        Self { seq: event.seq, sent_us: event.sent_us, kind: Some(kind) }
    }
}

impl InputMsg {
    /// `None` for input kinds or buttons this version does not know.
    pub fn into_event(self) -> Option<InputEvent> {
        let input = match self.kind? {
            InputKind::MouseMove(m) => Input::MouseMove { x: m.x, y: m.y },
            InputKind::MouseButton(m) => Input::MouseButton {
                x: m.x,
                y: m.y,
                button: button_from_wire(m.button)?,
                pressed: m.pressed,
                modifiers: modifiers_from_wire(m.modifiers),
            },
            InputKind::Scroll(m) => {
                Input::Scroll { x: m.x, y: m.y, dx: m.dx, dy: m.dy, modifiers: modifiers_from_wire(m.modifiers) }
            }
            InputKind::Key(m) => {
                Input::Key { key: m.key, code: m.code, pressed: m.pressed, modifiers: modifiers_from_wire(m.modifiers) }
            }
            InputKind::Text(m) => Input::Text { text: m.text },
            InputKind::Resize(m) => Input::Resize { width: m.width, height: m.height },
        };
        Some(InputEvent { seq: self.seq, sent_us: self.sent_us, input })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn every_input_kind_survives_the_wire() {
        let mods = Modifiers { shift: true, ctrl: false, alt: true, meta: true };
        let inputs = [
            Input::MouseMove { x: 1.5, y: 2.25 },
            Input::MouseButton { x: 3.0, y: 4.0, button: MouseButton::Forward, pressed: true, modifiers: mods },
            Input::Scroll { x: 5.0, y: 6.0, dx: -1.0, dy: 120.0, modifiers: mods },
            Input::Key { key: "Enter".into(), code: "Enter".into(), pressed: false, modifiers: mods },
            Input::Text { text: "héllo".into() },
            Input::Resize { width: 800, height: 600 },
        ];
        for (i, input) in inputs.into_iter().enumerate() {
            let event = InputEvent { seq: i as u64 + 1, sent_us: 42, input };
            let bytes = InputMsg::from(event.clone()).encode_to_vec();
            let back = InputMsg::decode(bytes.as_slice()).unwrap().into_event().unwrap();
            assert_eq!(back, event);
        }
    }

    #[test]
    fn a_message_type_from_a_newer_version_decodes_as_unknown() {
        // Field 15, length delimited, 2 bytes: a body this version has no tag for.
        let bytes = [15 << 3 | 2, 2, 0xAA, 0xBB];
        let msg = StreamMessage::decode(&bytes[..]).unwrap();
        assert!(msg.body.is_none());
    }
}
