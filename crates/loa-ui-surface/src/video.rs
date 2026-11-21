//! Live video (feature `remote-video`): the in-process H.264 [`FrameDecoder`] and the
//! launcher's "Live video" section.

use crate::decode::{DecodeError, FrameDecoder, encoding_name};
use crate::framebuffer::Framebuffer;
use crate::panel::{Message, launcher::section};
use crate::theme::{self, SurfaceKindStyle};
use iced::widget::{column, row, space};
use iced::{Center, Element};
use loa_contracts::surface::{Encoding, Frame, StreamInfo};
use loa_ui_kit::Icon;
use loa_ui_kit::theme::{SpaceRole as Sp, sp};

/// Decodes H.264 streams (Constrained Baseline, what the live video encoders send).
#[derive(Default)]
pub struct H264Decoder(loa_surfaces::proto::video::decode::VideoDecoder);

impl FrameDecoder for H264Decoder {
    fn accepts(&self) -> &[Encoding] {
        &[Encoding::H264]
    }

    fn apply(&mut self, frame: &Frame, fb: &mut Framebuffer) -> Result<bool, DecodeError> {
        self.0.decode(frame, fb.pixels_mut()).map_err(|e| DecodeError::Video(e.to_string()))
    }

    fn reset(&mut self) {
        self.0.reset();
    }
}

/// Live video encoding for viewers across the network, as the launcher shows it. Tabs in this
/// app always get the source frames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoStatus {
    pub enabled: bool,
    /// What network viewers get; `None` while the machine's encoders are tried.
    pub encoder: Option<StreamInfo>,
    /// Hardware encoders that work here, e.g. `h264_vaapi`.
    pub hardware: Vec<String>,
}

/// The launcher section: the on/off toggle and the encoder in use.
pub(crate) fn live_video_section(video: &VideoStatus) -> Element<'_, Message> {
    let toggle = |label, on| theme::choice(label, video.enabled == on, Some(Message::Video(on)));
    let choices = row![theme::hint("Encode browser and VM frames as video for peers"), space::horizontal(), toggle("On", true), toggle("Off", false)];
    let encoder = match &video.encoder {
        _ if !video.enabled => "Off: peers get frames as their source makes them (raw pixels, JPEG screencast)".to_owned(),
        None => "Trying this machine's encoders…".to_owned(),
        Some(s) if s.hardware => format!("{} with {}, hardware encoded", encoding_name(s.encoding), s.encoder),
        Some(s) => format!("No hardware encoder works here: {} with {}, software encoded", encoding_name(s.encoding), s.encoder),
    };
    let hardware = (video.hardware.len() > 1).then(|| Element::from(theme::hint(format!("Hardware encoders: {}", video.hardware.join(", ")))));
    let status = column![
        choices.spacing(sp(Sp::Xs)).align_y(Center),
        theme::hint(encoder),
        theme::hint("Tabs here show the source frames: no encode or decode in the way"),
    ];
    section(SurfaceKindStyle { icon: Icon::Monitor, title: "Live video" }, status.push(hardware).spacing(sp(Sp::Xs)))
}
