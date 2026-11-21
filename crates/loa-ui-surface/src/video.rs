//! Live video (feature `remote-video`): the in-process H.264 decoder and the
//! launcher's "Live video" section.

use crate::decode::{DecodeError, VideoDecoder, encoding_name};
use crate::panel::Message;
use crate::style;
use iced::widget::{button, column, row, space, text};
use iced::{Center, Element};
use loa_contracts::surface::{Encoding, Frame, StreamInfo};
use loa_ui_shell::theme::style as shell;

/// [`VideoDecoder`] for H.264 streams (Constrained Baseline, what the live
/// video encoders send).
#[derive(Default)]
pub struct H264Decoder(loa_surfaces::proto::video::decode::VideoDecoder);

impl H264Decoder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl VideoDecoder for H264Decoder {
    fn supports(&self, encoding: Encoding) -> bool {
        self.0.supports(encoding)
    }

    fn decode(&mut self, frame: &Frame, out: &mut [u8]) -> Result<bool, DecodeError> {
        self.0.decode(frame, out).map_err(|e| DecodeError::Video(e.to_string()))
    }

    fn reset(&mut self) {
        self.0.reset();
    }
}

/// Live video encoding for viewers across the network, as the launcher
/// shows it. Tabs in this app always get the source frames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoStatus {
    pub enabled: bool,
    /// What network viewers get; `None` while the machine's encoders are tried.
    pub encoder: Option<StreamInfo>,
    /// Hardware encoders that work here, e.g. `h264_vaapi`.
    pub hardware: Vec<String>,
}

/// The launcher section: the on/off toggle and the encoder in use.
pub(crate) fn view(video: &VideoStatus) -> Element<'_, Message> {
    let muted = |s: String| text(s).size(style::SMALL).style(shell::muted);
    let toggle = |label: &'static str, on: bool| {
        button(text(label).size(style::SMALL))
            .on_press(Message::Video(on))
            .padding([3, 8])
            .style(shell::row_for(video.enabled == on))
    };
    let choices = row![
        muted("Encode browser and VM frames as video for peers".into()),
        space::horizontal(),
        toggle("On", true),
        toggle("Off", false),
    ]
    .spacing(6)
    .align_y(Center);
    let encoder = match &video.encoder {
        _ if !video.enabled => "Off: peers get frames as their source makes them (raw pixels, JPEG screencast)".to_owned(),
        None => "Trying this machine's encoders…".to_owned(),
        Some(s) if s.hardware => format!("{} with {}, hardware encoded", encoding_name(s.encoding), s.encoder),
        Some(s) => format!("No hardware encoder works here: {} with {}, software encoded", encoding_name(s.encoding), s.encoder),
    };
    let mut status = column![
        choices,
        muted(encoder),
        muted("Tabs here show the source frames: no encode or decode in the way".into()),
    ]
    .spacing(4);
    if video.hardware.len() > 1 {
        status = status.push(muted(format!("Hardware encoders: {}", video.hardware.join(", "))));
    }
    status.into()
}
