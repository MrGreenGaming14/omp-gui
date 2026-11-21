//! Live video (feature `remote-video`): the launcher's "Live video" section.

use crate::decode::encoding_name;
use crate::panel::{Message, launcher::section};
use crate::theme::{self, SurfaceKindStyle};
use iced::widget::{column, row, space};
use iced::{Center, Element};
use loa_ui_kit::theme::{SpaceRole as Sp, sp};
use loa_view::surface::VideoStatus;
use loa_view::theme::Icon;

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
