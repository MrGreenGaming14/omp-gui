//! Frame fan-out ([`FrameFeed`]) and the input clock ([`InputLog`]) that
//! stamps `Frame::input_seq`, shared by every pixel surface.

use loa_contracts::surface::Frame;
use loa_util::LockExt;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Inputs remembered for stamping frames captured a little after them.
const INPUT_HISTORY: usize = 256;

/// Numbers frames, keeps the last keyframe for late viewers and fans frames
/// out to every viewer of one surface.
pub struct FrameFeed {
    tx: broadcast::Sender<Arc<Frame>>,
    seq: AtomicU64,
    last_key: Mutex<Option<Arc<Frame>>>,
}

impl FrameFeed {
    /// `backlog`: frames a slow viewer may fall behind before it loses the oldest.
    pub fn new(backlog: usize) -> Self {
        Self { tx: broadcast::channel(backlog).0, seq: AtomicU64::new(0), last_key: Mutex::new(None) }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Frame>> {
        self.tx.subscribe()
    }

    /// Viewers subscribed right now.
    pub fn viewers(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Numbers `frame` (its `seq` is replaced) and sends it to every viewer.
    pub fn publish(&self, frame: Frame) {
        let frame = Arc::new(Frame { seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1, ..frame });
        if frame.keyframe {
            *self.last_key.locked() = Some(frame.clone());
        }
        // No receivers is fine: nobody is watching right now.
        let _ = self.tx.send(frame);
    }

    /// Sends the last keyframe again under a new sequence number. Returns
    /// false when no keyframe was captured yet.
    pub fn republish_keyframe(&self) -> bool {
        let Some(last) = self.last_key.locked().clone() else { return false };
        self.publish(Frame::clone(&last));
        true
    }
}

/// Remembers when inputs were dispatched, to find the newest input sent
/// before a frame was captured.
#[derive(Default)]
pub struct InputLog {
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
        // A second viewer with lower seqs never moves the stamp back.
        for (seq, sent) in [(1, 100), (3, 200), (2, 300)] {
            log.record(seq, sent);
        }
        assert_eq!([99, 100, 250, 10_000].map(|at| log.seq_before(at)), [0, 1, 3, 3]);
    }
}
