//! Input to pixels latency, measured on the viewer's clock.
//!
//! Each input's send time is paired with the first shown frame whose
//! `input_seq` covers it. The meter keeps the most recent samples and reports
//! their median and 95th percentile.

use std::collections::VecDeque;

/// The input to pixels budget. Past it the meter warns.
pub const BUDGET_US: u64 = 50_000;

/// Samples kept in the rolling window.
pub const WINDOW: usize = 100;

/// Inputs waiting for a frame. Older ones are forgotten past this.
const MAX_PENDING: usize = 4096;

/// What the tab shows.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    pub median_us: u64,
    pub p95_us: u64,
    /// Samples behind the numbers.
    pub samples: usize,
    /// The 95th percentile is past [`BUDGET_US`].
    pub warning: bool,
}

/// The measurement core. Time comes in as arguments, so it is pure.
#[derive(Debug)]
pub struct LatencyMeter {
    /// `(seq, sent_us)` in send order.
    pending: VecDeque<(u64, u64)>,
    samples: VecDeque<u64>,
    window: usize,
    sorted: Vec<u64>,
    reading: Option<Reading>,
}

impl Default for LatencyMeter {
    fn default() -> Self {
        Self::new(WINDOW)
    }
}

impl LatencyMeter {
    /// A meter keeping the last `window` samples (at least one).
    pub fn new(window: usize) -> Self {
        let window = window.max(1);
        Self {
            pending: VecDeque::new(),
            samples: VecDeque::with_capacity(window),
            window,
            sorted: Vec::with_capacity(window),
            reading: None,
        }
    }

    /// Records input `seq` sent at `sent_us`. Sequence numbers go up.
    pub fn input_sent(&mut self, seq: u64, sent_us: u64) {
        if self.pending.len() == MAX_PENDING {
            self.pending.pop_front();
        }
        self.pending.push_back((seq, sent_us));
    }

    /// Records a frame shown at `now_us` that includes every input up to
    /// `input_seq`. Each covered input yields one sample.
    pub fn frame_shown(&mut self, input_seq: u64, now_us: u64) {
        let mut added = false;
        while let Some(&(seq, sent_us)) = self.pending.front() {
            if seq > input_seq {
                break;
            }
            self.pending.pop_front();
            if self.samples.len() == self.window {
                self.samples.pop_front();
            }
            self.samples.push_back(now_us.saturating_sub(sent_us));
            added = true;
        }
        if added {
            self.reading = self.compute();
        }
    }

    /// The current median and p95, `None` before the first sample.
    pub fn reading(&self) -> Option<Reading> {
        self.reading
    }

    fn compute(&mut self) -> Option<Reading> {
        self.sorted.clear();
        self.sorted.extend(self.samples.iter().copied());
        self.sorted.sort_unstable();
        let median_us = nearest_rank(&self.sorted, 50)?;
        let p95_us = nearest_rank(&self.sorted, 95)?;
        Some(Reading {
            median_us,
            p95_us,
            samples: self.sorted.len(),
            warning: p95_us > BUDGET_US,
        })
    }
}

/// Nearest rank percentile of an ascending slice.
fn nearest_rank(sorted: &[u64], percent: usize) -> Option<u64> {
    let rank = (sorted.len() * percent).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_each_input_with_the_first_frame_covering_it() {
        let mut m = LatencyMeter::default();
        m.input_sent(1, 1_000);
        m.input_sent(2, 3_000);
        m.input_sent(3, 4_000);
        // Covers 1 and 2 only.
        m.frame_shown(2, 11_000);
        let r = m.reading().unwrap();
        assert_eq!(r.samples, 2);
        // A later frame covering 2 again adds nothing; 3 is still waiting.
        m.frame_shown(2, 90_000);
        assert_eq!(m.reading().unwrap().samples, 2);
        m.frame_shown(3, 24_000);
        let r = m.reading().unwrap();
        assert_eq!(r.samples, 3);
        // Samples 10 ms, 8 ms, 20 ms.
        assert_eq!(r.median_us, 10_000);
        assert_eq!(r.p95_us, 20_000);
        assert!(!r.warning);
    }

    #[test]
    fn frames_without_new_input_do_not_count() {
        let mut m = LatencyMeter::default();
        m.frame_shown(0, 5_000);
        assert_eq!(m.reading(), None);
    }

    #[test]
    fn warns_when_p95_is_past_the_budget() {
        let mut m = LatencyMeter::default();
        let mut t = 0;
        for seq in 1..=19 {
            m.input_sent(seq, t);
            m.frame_shown(seq, t + 20_000);
            t += 100_000;
        }
        assert!(!m.reading().unwrap().warning);
        m.input_sent(20, t);
        m.frame_shown(20, t + 80_000);
        // 20 samples: p95 is the 19th, still 20 ms.
        assert!(!m.reading().unwrap().warning);
        m.input_sent(21, t);
        m.frame_shown(21, t + 51_000);
        let r = m.reading().unwrap();
        assert_eq!(r.median_us, 20_000);
        assert_eq!(r.p95_us, 51_000);
        assert!(r.warning);
    }

    #[test]
    fn old_samples_leave_the_window() {
        let mut m = LatencyMeter::new(3);
        for (seq, ms) in [(1, 90), (2, 90), (3, 90), (4, 5), (5, 5), (6, 5)] {
            m.input_sent(seq, 0);
            m.frame_shown(seq, ms * 1_000);
        }
        let r = m.reading().unwrap();
        assert_eq!(
            (r.samples, r.median_us, r.p95_us, r.warning),
            (3, 5_000, 5_000, false)
        );
    }
}
