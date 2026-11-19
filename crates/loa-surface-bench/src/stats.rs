//! Order statistics over latency samples.

use serde::Serialize;
use std::time::Duration;

/// Min, median, tail percentiles and max of one measured quantity, in
/// microseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub count: usize,
    pub min_us: u64,
    pub median_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}

impl Summary {
    /// Summarises `samples`. `None` when there are none.
    pub fn of(samples: &[Duration]) -> Option<Self> {
        let mut us: Vec<u64> = samples.iter().map(|d| saturating_us(*d)).collect();
        if us.is_empty() {
            return None;
        }
        us.sort_unstable();
        Some(Self {
            count: us.len(),
            min_us: us[0],
            median_us: percentile(&us, 50.0),
            p95_us: percentile(&us, 95.0),
            p99_us: percentile(&us, 99.0),
            max_us: us[us.len() - 1],
        })
    }

    pub fn p95(&self) -> Duration {
        Duration::from_micros(self.p95_us)
    }
}

/// Nearest rank percentile of an ascending, non empty slice: the smallest
/// sample with at least `p` percent of samples at or below it.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    let rank = (p / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

pub(crate) fn saturating_us(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: &[u64]) -> Vec<Duration> {
        v.iter().map(|m| Duration::from_millis(*m)).collect()
    }

    #[test]
    fn one_slow_sample_in_twenty_sets_p95_but_not_median() {
        let mut samples = vec![10; 19];
        samples.push(60);
        let s = Summary::of(&ms(&samples)).unwrap();
        assert_eq!(s.median_us, 10_000);
        assert_eq!(s.p95_us, 10_000);
        assert_eq!(s.p99_us, 60_000);
        assert_eq!(s.max_us, 60_000);

        // Two slow samples in twenty are more than 5 percent: p95 must see them.
        samples[0] = 60;
        let s = Summary::of(&ms(&samples)).unwrap();
        assert_eq!(s.p95_us, 60_000);
    }

    #[test]
    fn unordered_samples_are_ranked() {
        let s = Summary::of(&ms(&[5, 1, 4, 2, 3])).unwrap();
        assert_eq!((s.min_us, s.median_us, s.max_us), (1_000, 3_000, 5_000));
        assert!(Summary::of(&[]).is_none());
    }
}
