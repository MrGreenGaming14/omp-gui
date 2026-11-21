//! Live video for viewers across the network (feature `remote-video`):
//! browser tabs and VMs are coded as H.264/HEVC for peers that decode it.

use crate::hub::service::{Opened, Origin};
use crate::hub::Result;
use loa_contracts::surface::{Surface, SurfaceMode, SurfaceUri};
use crate::proto::video::ffmpeg::FfmpegConfig;
use crate::proto::video::{LiveVideo, VideoSurface};
use loa_util::LockExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;

/// What remote video starts with.
#[derive(Clone, Debug)]
pub struct VideoConfig {
    /// Live surfaces stream video to network viewers that decode it.
    pub enabled: bool,
    /// The ffmpeg hardware encoders run through.
    pub ffmpeg: Option<FfmpegConfig>,
    /// This machine's encoders; `None` tries them on first need.
    pub encoders: Option<Arc<LiveVideo>>,
}

impl VideoConfig {
    /// On, with ffmpeg from `$PATH`.
    pub fn discover() -> Self {
        Self { enabled: true, ffmpeg: FfmpegConfig::discover(), encoders: None }
    }
}

/// The hub's video encoders and the surfaces it codes.
pub struct RemoteVideo {
    ffmpeg: Option<FfmpegConfig>,
    encoders: OnceCell<Arc<LiveVideo>>,
    enabled: AtomicBool,
    videos: Mutex<Vec<(SurfaceUri, Arc<VideoSurface>)>>,
}

impl RemoteVideo {
    pub(crate) fn new(config: &VideoConfig) -> Self {
        Self {
            ffmpeg: config.ffmpeg.clone(),
            encoders: OnceCell::new_with(config.encoders.clone()),
            enabled: AtomicBool::new(config.enabled),
            videos: Mutex::new(Vec::new()),
        }
    }

    /// This machine's encoders, tried on the first call (it starts ffmpeg a
    /// few times, so it takes a moment).
    pub async fn encoders(&self) -> Arc<LiveVideo> {
        let ffmpeg = self.ffmpeg.clone();
        let probe = || async move {
            let probed = tokio::task::spawn_blocking(move || LiveVideo::probe(ffmpeg)).await;
            Arc::new(probed.unwrap_or_else(|_| LiveVideo::probe(None)))
        };
        self.encoders.get_or_init(probe).await.clone()
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// Turns video on or off, for surfaces already coded too.
    pub async fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
        let videos: Vec<_> = self.videos.locked().iter().map(|(_, video)| video.clone()).collect();
        for video in videos {
            if let Err(error) = video.set_video(enabled).await {
                tracing::warn!(%error, "switching a surface's video");
            }
        }
    }

    /// `opened` for a viewer across the network: live pixel surfaces (browser
    /// tabs, VMs) are coded as video, one encoder per surface; the rest go as
    /// they are.
    pub(crate) async fn surface(&self, opened: Opened) -> Result<Arc<dyn Surface>> {
        let live = matches!(opened.origin, Origin::Browser | Origin::Vm) && opened.info.mode == SurfaceMode::Pixel;
        let uri = &opened.info.uri;
        let coded = |videos: &[(SurfaceUri, Arc<VideoSurface>)]| {
            videos.iter().find(|(open, _)| open == uri).map(|(_, video)| video.clone() as Arc<dyn Surface>)
        };
        if !live {
            return Ok(opened.surface);
        }
        if let Some(video) = coded(&self.videos.locked()) {
            return Ok(video);
        }
        let encoders = self.encoders().await;
        let mut videos = self.videos.locked();
        if let Some(video) = coded(&videos) {
            return Ok(video);
        }
        let video = VideoSurface::new(opened.surface, encoders, self.enabled())?;
        videos.push((uri.clone(), video.clone()));
        Ok(video)
    }

    /// Stops coding `uri`, or every surface for `None`.
    pub(crate) fn forget(&self, uri: Option<&SurfaceUri>) {
        self.videos.locked().retain(|(open, _)| uri.is_some_and(|uri| open != uri));
    }
}
