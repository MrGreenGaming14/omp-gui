//! Frame fan-out and the input clock that stamps `Frame::input_seq`.

use loa_util::LockExt;
use bytes::Bytes;
use loa_surface::{Encoding, Frame, Rect};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

/// Frames a slow viewer may fall behind before it loses the oldest.
const FRAME_BACKLOG: usize = 8;

/// Inputs remembered for stamping frames captured a little after them.
const INPUT_HISTORY: usize = 256;

/// Microseconds since the Unix epoch on the server clock.
pub(crate) fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64)
}

/// The picture data of one frame before it gets a sequence number.
pub(crate) struct Picture {
    pub captured_us: u64,
    pub input_seq: u64,
    pub width: u32,
    pub height: u32,
    pub region: Rect,
    pub encoding: Encoding,
    pub keyframe: bool,
    pub data: Bytes,
}

/// Numbers frames and fans them out to every viewer of one surface.
pub(crate) struct FrameHub {
    tx: broadcast::Sender<Arc<Frame>>,
    seq: AtomicU64,
    last_key: Mutex<Option<Arc<Frame>>>,
}

impl FrameHub {
    pub fn new() -> Self {
        Self { tx: broadcast::channel(FRAME_BACKLOG).0, seq: AtomicU64::new(0), last_key: Mutex::new(None) }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Frame>> {
        self.tx.subscribe()
    }

    pub fn publish(&self, picture: Picture) {
        let frame = Arc::new(Frame {
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            captured_us: picture.captured_us,
            input_seq: picture.input_seq,
            width: picture.width,
            height: picture.height,
            region: picture.region,
            encoding: picture.encoding,
            keyframe: picture.keyframe,
            data: picture.data,
        });
        if frame.keyframe {
            *self.last_key.locked() = Some(frame.clone());
        }
        // No receivers is fine: nobody is watching this tab right now.
        let _ = self.tx.send(frame);
    }

    /// Sends the last keyframe again under a new sequence number. Returns
    /// false when no keyframe was captured yet.
    pub fn republish_keyframe(&self) -> bool {
        let last = self.last_key.locked().clone();
        let Some(last) = last else { return false };
        self.publish(Picture {
            captured_us: last.captured_us,
            input_seq: last.input_seq,
            width: last.width,
            height: last.height,
            region: last.region,
            encoding: last.encoding,
            keyframe: true,
            data: last.data.clone(),
        });
        true
    }
}

/// Remembers when inputs were dispatched, to find the newest input sent
/// before a frame was captured.
#[derive(Default)]
pub(crate) struct InputLog {
    /// `(sent_us, highest seq dispatched so far)`, in dispatch order.
    entries: VecDeque<(u64, u64)>,
    highest: u64,
}

impl InputLog {
    pub fn record(&mut self, seq: u64, sent_us: u64) {
        self.highest = self.highest.max(seq);
        if self.entries.len() == INPUT_HISTORY {
            self.entries.pop_front();
        }
        let sent_us = self.entries.back().map_or(sent_us, |&(last, _)| sent_us.max(last));
        self.entries.push_back((sent_us, self.highest));
    }

    /// Highest seq dispatched at or before `captured_us`, 0 when none is known.
    pub fn seq_before(&self, captured_us: u64) -> u64 {
        match self.entries.partition_point(|&(sent, _)| sent <= captured_us) {
            0 => 0,
            n => self.entries[n - 1].1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_stamped_with_the_last_input_sent_before_capture() {
        let mut log = InputLog::default();
        assert_eq!(log.seq_before(1_000), 0);
        log.record(1, 100);
        log.record(2, 200);
        log.record(3, 300);
        assert_eq!(log.seq_before(99), 0);
        assert_eq!(log.seq_before(100), 1);
        assert_eq!(log.seq_before(250), 2);
        assert_eq!(log.seq_before(10_000), 3);
    }

    #[test]
    fn a_second_viewer_with_lower_seqs_never_moves_the_stamp_back() {
        let mut log = InputLog::default();
        log.record(40, 100);
        log.record(2, 200);
        assert_eq!(log.seq_before(250), 40);
    }

    #[test]
    fn keyframe_is_resent_under_a_new_seq_for_late_viewers() {
        let hub = FrameHub::new();
        assert!(!hub.republish_keyframe());
        hub.publish(Picture {
            captured_us: 5,
            input_seq: 3,
            width: 2,
            height: 1,
            region: Rect { x: 0, y: 0, width: 2, height: 1 },
            encoding: Encoding::Jpeg,
            keyframe: true,
            data: Bytes::from_static(b"jpeg"),
        });
        let mut late = hub.subscribe();
        assert!(hub.republish_keyframe());
        let frame = late.try_recv().unwrap();
        assert_eq!((frame.seq, frame.input_seq, frame.keyframe), (2, 3, true));
        assert_eq!(frame.data, Bytes::from_static(b"jpeg"));
    }
}
