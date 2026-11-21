//! Protobuf messages of `omp/surface/1` and their contract conversions. The
//! stream carries length delimited [`StreamMessage`]s, each datagram one
//! [`Datagram`]. Unknown body tags decode as `body: None` and are skipped.

use bytes::Bytes;
use loa_contracts::surface::{
    Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, Rect, StreamInfo, SurfaceInfo, SurfaceMode,
};

use crate::proto::{ProtoError, Result};

/// Minor revision within the ALPN's major version.
pub const VERSION: u32 = 1;

macro_rules! messages {
    ($($(#[$meta:meta])* $name:ident { $($(#[$f:meta])* $field:ident: $ty:ty),* $(,)? })*) => {$(
        $(#[$meta])* #[derive(Clone, PartialEq, prost::Message)]
        pub struct $name { $($(#[$f])* pub $field: $ty),* }
    )*};
}

macro_rules! enums {
    ($($name:ident { $($(#[$meta:meta])* $variant:ident = $value:literal),* $(,)? })*) => {$(
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, prost::Enumeration)]
        #[repr(i32)]
        pub enum $name { Unspecified = 0, $($(#[$meta])* $variant = $value),* }
    )*};
}

macro_rules! oneofs {
    ($($name:ident { $($(#[$meta:meta])* $variant:ident($ty:ty) = $tag:literal),* $(,)? })*) => {$(
        #[derive(Clone, PartialEq, prost::Oneof)]
        pub enum $name { $($(#[$meta])* #[prost(message, tag = $tag)] $variant($ty)),* }
    )*};
}

messages! {
    StreamMessage { #[prost(oneof = "Body", tags = "1, 2, 3, 4, 5, 6, 7, 8, 9, 10")] body: Option<Body> }
    Open {
        #[prost(uint32, tag = "1")] version: u32,
        #[prost(string, tag = "2")] uri: String,
        #[prost(enumeration = "WireEncoding", repeated, tag = "3")] encodings: Vec<i32>,
    }
    Accept {
        /// Lower of the two peers' versions.
        #[prost(uint32, tag = "1")] version: u32,
        #[prost(message, optional, tag = "2")] info: Option<SurfaceInfoMsg>,
        /// Pixel mode: frames come as datagrams, else on the stream.
        #[prost(bool, tag = "3")] datagrams: bool,
        #[prost(message, optional, tag = "4")] stream: Option<StreamInfoMsg>,
    }
    StreamInfoMsg {
        #[prost(enumeration = "WireEncoding", tag = "1")] encoding: i32,
        #[prost(string, tag = "2")] encoder: String,
        #[prost(bool, tag = "3")] hardware: bool,
    }
    Reject { #[prost(enumeration = "RejectCode", tag = "1")] code: i32, #[prost(string, tag = "2")] reason: String }
    SurfaceInfoMsg {
        #[prost(string, tag = "1")] uri: String,
        #[prost(string, tag = "2")] name: String,
        #[prost(uint32, tag = "3")] width: u32,
        #[prost(uint32, tag = "4")] height: u32,
        #[prost(enumeration = "WireMode", tag = "5")] mode: i32,
        #[prost(string, optional, tag = "6")] mime: Option<String>,
    }
    KeyframeRequest {}
    Resize { #[prost(uint32, tag = "1")] width: u32, #[prost(uint32, tag = "2")] height: u32 }
    Close { #[prost(string, tag = "1")] reason: String }
    InputMsg {
        #[prost(uint64, tag = "1")] seq: u64,
        #[prost(uint64, tag = "2")] sent_us: u64,
        #[prost(oneof = "InputKind", tags = "3, 4, 5, 6, 7, 8")] kind: Option<InputKind>,
    }
    MouseMoveMsg { #[prost(float, tag = "1")] x: f32, #[prost(float, tag = "2")] y: f32 }
    MouseButtonMsg {
        #[prost(float, tag = "1")] x: f32,
        #[prost(float, tag = "2")] y: f32,
        #[prost(enumeration = "WireButton", tag = "3")] button: i32,
        #[prost(bool, tag = "4")] pressed: bool,
        /// Bits 0-3: shift, ctrl, alt, meta.
        #[prost(uint32, tag = "5")] modifiers: u32,
    }
    ScrollMsg {
        #[prost(float, tag = "1")] x: f32,
        #[prost(float, tag = "2")] y: f32,
        #[prost(float, tag = "3")] dx: f32,
        #[prost(float, tag = "4")] dy: f32,
        #[prost(uint32, tag = "5")] modifiers: u32,
    }
    KeyMsg {
        #[prost(string, tag = "1")] key: String,
        #[prost(string, tag = "2")] code: String,
        #[prost(bool, tag = "3")] pressed: bool,
        #[prost(uint32, tag = "4")] modifiers: u32,
    }
    TextMsg { #[prost(string, tag = "1")] text: String }
    FrameHeader {
        #[prost(uint64, tag = "1")] seq: u64,
        #[prost(uint64, tag = "2")] captured_us: u64,
        #[prost(uint64, tag = "3")] input_seq: u64,
        #[prost(uint32, tag = "4")] width: u32,
        #[prost(uint32, tag = "5")] height: u32,
        #[prost(uint32, tag = "6")] region_x: u32,
        #[prost(uint32, tag = "7")] region_y: u32,
        #[prost(uint32, tag = "8")] region_width: u32,
        #[prost(uint32, tag = "9")] region_height: u32,
        #[prost(enumeration = "WireEncoding", tag = "10")] encoding: i32,
        #[prost(bool, tag = "11")] keyframe: bool,
    }
    FrameMsg {
        #[prost(message, optional, tag = "1")] header: Option<FrameHeader>,
        #[prost(bytes = "bytes", tag = "2")] data: Bytes,
    }
    Datagram { #[prost(oneof = "DatagramKind", tags = "1")] kind: Option<DatagramKind> }
    /// Slice `index` of `count` of frame `seq`. Fragment 0 carries the header.
    Fragment {
        #[prost(uint64, tag = "1")] seq: u64,
        #[prost(uint32, tag = "2")] index: u32,
        #[prost(uint32, tag = "3")] count: u32,
        #[prost(message, optional, tag = "4")] header: Option<FrameHeader>,
        #[prost(bytes = "bytes", tag = "5")] data: Bytes,
    }
    DocumentHeader {
        #[prost(string, tag = "1")] name: String,
        #[prost(string, tag = "2")] mime: String,
        #[prost(uint64, tag = "3")] length: u64,
        /// CRC-32 (IEEE) of the whole file.
        #[prost(uint32, tag = "4")] crc32: u32,
    }
    DocumentChunk { #[prost(uint64, tag = "1")] offset: u64, #[prost(bytes = "bytes", tag = "2")] data: Bytes }
}

oneofs! {
    // Viewer: `Open`, then `Input`/`KeyframeRequest` (pixel). Server: `Accept` or
    // `Reject`, then `Resize`/`Frame` (pixel, `Frame` only without datagrams) or
    // `DocumentHeader` and in-order `DocumentChunk`s. Either side may `Close`.
    Body {
        Open(Open) = "1",
        Accept(Accept) = "2",
        Reject(Reject) = "3",
        Input(InputMsg) = "4",
        KeyframeRequest(KeyframeRequest) = "5",
        Resize(Resize) = "6",
        Frame(FrameMsg) = "7",
        DocumentHeader(DocumentHeader) = "8",
        DocumentChunk(DocumentChunk) = "9",
        Close(Close) = "10",
    }
    InputKind {
        MouseMove(MouseMoveMsg) = "3",
        MouseButton(MouseButtonMsg) = "4",
        Scroll(ScrollMsg) = "5",
        Key(KeyMsg) = "6",
        Text(TextMsg) = "7",
        Resize(Resize) = "8",
    }
    DatagramKind { Fragment(Fragment) = "1" }
}

impl From<Body> for StreamMessage {
    fn from(body: Body) -> Self {
        Self { body: Some(body) }
    }
}

enums! {
    RejectCode { Version = 1, NotFound = 2, Unavailable = 3 }
    WireMode { Pixel = 1, Document = 2 }
    WireEncoding { Rgba = 1, Jpeg = 2, Png = 3, H264 = 4, Hevc = 5 }
    WireButton { Left = 1, Middle = 2, Right = 3, Back = 4, Forward = 5 }
}

/// Contract enum to wire enum and back (`None` for unknown or `Unspecified`).
macro_rules! wire_enum {
    ($to:ident, $from:ident, $ty:ty, $wire:ident: $($v:ident),*) => {
        pub fn $to(value: $ty) -> i32 {
            (match value { $(<$ty>::$v => $wire::$v,)* }) as i32
        }

        pub fn $from(value: i32) -> Option<$ty> {
            match $wire::try_from(value).ok()? {
                $wire::Unspecified => None,
                $($wire::$v => Some(<$ty>::$v),)*
            }
        }
    };
}

wire_enum!(encoding_to_wire, encoding_from_wire, Encoding, WireEncoding: Rgba, Jpeg, Png, H264, Hevc);
wire_enum!(button_to_wire, button_from_wire, MouseButton, WireButton: Left, Middle, Right, Back, Forward);
wire_enum!(mode_to_wire, mode_from_wire, SurfaceMode, WireMode: Pixel, Document);

pub fn is_video(encoding: Encoding) -> bool {
    matches!(encoding, Encoding::H264 | Encoding::Hevc)
}

impl From<&StreamInfo> for StreamInfoMsg {
    fn from(stream: &StreamInfo) -> Self {
        Self { encoding: encoding_to_wire(stream.encoding), encoder: stream.encoder.clone(), hardware: stream.hardware }
    }
}

impl StreamInfoMsg {
    pub fn into_info(self) -> Option<StreamInfo> {
        Some(StreamInfo { encoding: encoding_from_wire(self.encoding)?, encoder: self.encoder, hardware: self.hardware })
    }
}

impl From<&SurfaceInfo> for SurfaceInfoMsg {
    fn from(info: &SurfaceInfo) -> Self {
        Self {
            uri: info.uri.as_str().to_owned(),
            name: info.name.clone(),
            width: info.width,
            height: info.height,
            mode: mode_to_wire(info.mode),
            mime: info.mime.clone(),
        }
    }
}

impl TryFrom<SurfaceInfoMsg> for SurfaceInfo {
    type Error = ProtoError;

    fn try_from(msg: SurfaceInfoMsg) -> Result<Self> {
        let mode = mode_from_wire(msg.mode).ok_or_else(|| ProtoError::Protocol(format!("unknown surface mode {}", msg.mode)))?;
        Ok(Self { uri: msg.uri.parse()?, name: msg.name, width: msg.width, height: msg.height, mode, mime: msg.mime })
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
            encoding: encoding_to_wire(frame.encoding),
            keyframe: frame.keyframe,
        }
    }
}

impl FrameHeader {
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
    u32::from(m.shift) | u32::from(m.ctrl) << 1 | u32::from(m.alt) << 2 | u32::from(m.meta) << 3
}

fn modifiers_from_wire(bits: u32) -> Modifiers {
    let on = |bit: u32| bits & 1 << bit != 0;
    Modifiers { shift: on(0), ctrl: on(1), alt: on(2), meta: on(3) }
}

impl From<InputEvent> for InputMsg {
    fn from(event: InputEvent) -> Self {
        let kind = match event.input {
            Input::MouseMove { x, y } => InputKind::MouseMove(MouseMoveMsg { x, y }),
            Input::MouseButton { x, y, button, pressed, modifiers } => {
                let (button, modifiers) = (button_to_wire(button), modifiers_to_wire(modifiers));
                InputKind::MouseButton(MouseButtonMsg { x, y, button, pressed, modifiers })
            }
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
    pub fn into_event(self) -> Option<InputEvent> {
        let input = match self.kind? {
            InputKind::MouseMove(m) => Input::MouseMove { x: m.x, y: m.y },
            InputKind::MouseButton(m) => {
                let (button, modifiers) = (button_from_wire(m.button)?, modifiers_from_wire(m.modifiers));
                Input::MouseButton { x: m.x, y: m.y, button, pressed: m.pressed, modifiers }
            }
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
    fn inputs_round_trip_and_unknown_bodies_decode_as_none() {
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
            assert_eq!(InputMsg::decode(bytes.as_slice()).unwrap().into_event().unwrap(), event);
        }
        // Field 15, length delimited, 2 bytes: a body this version has no tag for.
        assert!(StreamMessage::decode(&[15 << 3 | 2, 2, 0xAA, 0xBB][..]).unwrap().body.is_none());
    }
}
