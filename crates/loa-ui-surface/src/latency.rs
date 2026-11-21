//! Input to pixels latency on the viewer's clock: each input's send time is paired with the
//! first shown frame whose `input_seq` covers it, over a rolling window of samples.

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
    /// Scratch buffer for the percentiles, reused across frames.
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
        Self { pending: VecDeque::new(), samples: VecDeque::with_capacity(window), window, sorted: Vec::with_capacity(window), reading: None }
    }

    /// Records input `seq` sent at `sent_us`. Sequence numbers go up.
    pub fn input_sent(&mut self, seq: u64, sent_us: u64) {
        if self.pending.len() == MAX_PENDING {
            self.pending.pop_front();
        }
        self.pending.push_back((seq, sent_us));
    }

    /// Records a frame shown at `now_us` that includes every input up to `input_seq`. Each
    /// covered input yields one sample.
    pub fn frame_shown(&mut self, input_seq: u64, now_us: u64) {
        let before = self.pending.len();
        while let Some(&(_, sent_us)) = self.pending.front().filter(|(seq, _)| *seq <= input_seq) {
            self.pending.pop_front();
            if self.samples.len() == self.window {
                self.samples.pop_front();
            }
            self.samples.push_back(now_us.saturating_sub(sent_us));
        }
        if self.pending.len() != before {
            self.sorted.clear();
            self.sorted.extend(&self.samples);
            self.sorted.sort_unstable();
            let (median_us, p95_us) = (percentile(&self.sorted, 50), percentile(&self.sorted, 95));
            self.reading = Some(Reading { median_us, p95_us, samples: self.sorted.len(), warning: p95_us > BUDGET_US });
        }
    }

    /// The current median and p95, `None` before the first sample.
    pub fn reading(&self) -> Option<Reading> {
        self.reading
    }
}

/// Nearest rank percentile of a non-empty ascending slice.
fn percentile(sorted: &[u64], percent: usize) -> u64 {
    sorted[(sorted.len() * percent).div_ceil(100).max(1) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `(input seq, sent ms, frame input_seq, shown ms)` rounds, checking the reading.
    fn run(meter: &mut LatencyMeter, rounds: &[(u64, u64, u64, u64)]) -> Option<(usize, u64, u64, bool)> {
        for &(seq, sent, covers, shown) in rounds {
            if seq > 0 {
                meter.input_sent(seq, sent * 1000);
            }
            meter.frame_shown(covers, shown * 1000);
        }
        meter.reading().map(|r| (r.samples, r.median_us / 1000, r.p95_us / 1000, r.warning))
    }

    #[test]
    fn samples_pair_inputs_with_covering_frames() {
        // Frames without new input count nothing.
        assert_eq!(run(&mut LatencyMeter::default(), &[(0, 0, 0, 5)]), None);
        // 1 and 2 are covered by the first frame, 3 waits; a repeated cover adds nothing.
        let mut m = LatencyMeter::default();
        m.input_sent(1, 1_000);
        assert_eq!(run(&mut m, &[(2, 3, 2, 11), (0, 0, 2, 90), (3, 4, 3, 24)]), Some((3, 10, 20, false)));
        // 21 samples: p95 is the 20th; one past the budget warns.
        let quick: Vec<_> = (1..=20).map(|s| (s, 0, s, 20 + 60 * u64::from(s == 20))).collect();
        let mut m = LatencyMeter::default();
        assert_eq!(run(&mut m, &quick), Some((20, 20, 20, false)));
        assert_eq!(run(&mut m, &[(21, 0, 21, 51)]), Some((21, 20, 51, true)));
        // Old samples leave the window.
        let window: Vec<_> = [90, 90, 90, 5, 5, 5].iter().zip(1..).map(|(&ms, s)| (s, 0, s, ms)).collect();
        assert_eq!(run(&mut LatencyMeter::new(3), &window), Some((3, 5, 5, false)));
    }
}
