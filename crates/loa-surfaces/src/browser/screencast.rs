//! Frames from Chrome's `Page.startScreencast`: the headless [`Capture`] strategy.

use crate::browser::Result;
use crate::browser::tab::{Capture, set_viewport};
use crate::feed::{FrameFeed, InputLog};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::page::{
    EventScreencastFrame, ScreencastFrameAckParams, StartScreencastFormat, StartScreencastParams, StopScreencastParams,
};
use chromiumoxide::listeners::EventStream;
use futures::StreamExt;
use loa_contracts::surface::{Encoding, Frame, Rect};
use loa_util::LockExt;
use loa_util::time::now_us;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

/// The screencast of one tab, pumped into its feed by `pump`.
pub(crate) struct Screencast {
    pub pump: JoinHandle<()>,
    pub quality: u8,
}

#[async_trait]
impl Capture for Screencast {
    async fn resize(&self, page: &Page, width: u32, height: u32) -> Result<()> {
        stop(page).await?;
        set_viewport(page, width, height).await?;
        start(page, width, height, self.quality).await
    }

    /// Every frame is a whole JPEG and a static page sends no new ones, so the last is sent
    /// again. A new viewer gets it too: navigation can finish after the only repaint.
    fn keyframe(&self, hub: &FrameFeed, _subscribe: bool) {
        hub.republish_keyframe();
    }

    fn stop(&self) {
        self.pump.abort();
    }
}

/// Starts the screencast at `width` x `height`. Chrome sends a frame on each
/// repaint and waits for the acknowledgement before sending the next.
pub(crate) async fn start(page: &Page, width: u32, height: u32, quality: u8) -> Result<()> {
    let params = StartScreencastParams {
        format: Some(StartScreencastFormat::Jpeg),
        quality: Some(i64::from(quality.min(100))),
        max_width: Some(i64::from(width)),
        max_height: Some(i64::from(height)),
        every_nth_frame: Some(1),
    };
    page.execute(params).await?;
    Ok(())
}

pub(crate) async fn stop(page: &Page) -> Result<()> {
    page.execute(StopScreencastParams::default()).await?;
    Ok(())
}

/// Publishes screencast frames until the page goes away. `events` must be
/// subscribed before [`start`] so no frame is missed.
pub(crate) async fn pump(
    page: Page,
    mut events: EventStream<EventScreencastFrame>,
    hub: Arc<FrameFeed>,
    log: Arc<Mutex<InputLog>>,
) {
    while let Some(event) = events.next().await {
        let captured_us = event.metadata.timestamp.as_ref().map_or_else(now_us, |t| (*t.inner() * 1e6) as u64);
        match STANDARD.decode(event.data.as_ref() as &str) {
            Ok(jpeg) => {
                let input_seq = log.locked().seq_before(captured_us);
                // Metadata may describe the new viewport for a frame still
                // encoded at the old size during resize. The JPEG header is
                // authoritative for the pixels sent to viewers.
                let mut header = jpeg_decoder::Decoder::new(jpeg.as_slice());
                let dimensions = header.read_info().map(|()| header.info()).and_then(|info| {
                    info.map(|info| (u32::from(info.width), u32::from(info.height)))
                        .ok_or_else(|| jpeg_decoder::Error::Format("missing frame header".into()))
                });
                match dimensions {
                    Ok((width, height)) => hub.publish(Frame {
                        seq: 0,
                        captured_us,
                        input_seq,
                        width,
                        height,
                        region: Rect { x: 0, y: 0, width, height },
                        encoding: Encoding::Jpeg,
                        keyframe: true,
                        data: Bytes::from(jpeg),
                    }),
                    Err(error) => tracing::warn!(%error, "screencast frame is not a JPEG"),
                }
            }
            Err(error) => tracing::warn!(%error, "screencast frame is not base64"),
        }
        if page.execute(ScreencastFrameAckParams::new(event.session_id)).await.is_err() {
            break;
        }
    }
}
