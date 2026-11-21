//! Live video for peers (feature `remote-video`): the launcher's toggle,
//! saved in `[surfaces] video`, and the encoder status it shows.

use std::sync::Arc;

use iced::Task;
use loa_surfaces::{HubConfig, LiveVideo, SurfaceHub};
use loa_ui_surface::{H264Decoder, NewDecoder, Options, Panel, VideoStatus};

use super::{Message, save};
use crate::area::Ctx;

pub(super) fn configure(config: &mut HubConfig, settings: &loa_config::SurfaceSettings) {
    config.video.enabled = settings.video;
}

/// Tabs here decode H.264 too.
pub(super) fn options(options: &mut Options) {
    let h264: NewDecoder = Arc::new(|| Box::new(H264Decoder::default()));
    options.decoder = Some(h264);
}

/// Shows the toggle and tries this machine's encoders.
pub(super) fn start(hub: &Arc<SurfaceHub>, panel: &mut Panel) -> Task<Message> {
    panel.set_video(VideoStatus { enabled: hub.video().enabled(), encoder: None, hardware: Vec::new() });
    let hub = Arc::clone(hub);
    Task::perform(async move { hub.video().encoders().await }, Message::Encoders)
}

pub(super) fn set(hub: &Arc<SurfaceHub>, enabled: bool, ctx: &mut Ctx) -> Task<Message> {
    save(ctx, |settings| settings.video = enabled);
    let hub = Arc::clone(hub);
    Task::future(async move { hub.video().set_enabled(enabled).await }).discard()
}

/// The encoder the renderer here gets (it decodes H.264) and the hardware
/// encoders that work.
pub(super) fn status(enabled: bool, encoders: &LiveVideo) -> VideoStatus {
    let encoder = encoders.choose(&[loa_contracts::surface::Encoding::H264]).map(|info| loa_surfaces::proto::video::stream_info(&info));
    let hardware = encoders
        .encoders()
        .iter()
        .filter(|probe| probe.works() && probe.info.accel.is_hardware())
        .map(|probe| probe.info.name.trim_start_matches("ffmpeg ").to_owned())
        .collect();
    VideoStatus { enabled, encoder, hardware }
}
