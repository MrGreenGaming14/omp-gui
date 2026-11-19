//! Human-readable latency budget table and machine-readable sample summaries.

use crate::{bench::BenchResult, stats::Summary};
use serde::Serialize;
use std::{fmt::Write, time::Duration};

/// Measured latency distributions in the units used by the design budget.
#[derive(Clone, Debug, Serialize)]
pub struct LatencyReport {
    pub iterations: usize,
    pub input_to_pixels: Summary,
    pub server_render: Option<Summary>,
    pub capture_encode: Option<Summary>,
    pub decode: Summary,
    pub budget_ms: f64,
    pub passed: bool,
}

impl LatencyReport {
    pub fn from_result(result: &BenchResult, budget: Duration) -> crate::Result<Self> {
        let total: Vec<_> = result.samples.iter().map(|s| s.latency).collect();
        let decode: Vec<_> = result.samples.iter().map(|s| s.decode).collect();
        let render: Vec<_> = result.samples.iter().filter_map(|s| s.stages.as_ref().map(|t| t.render)).collect();
        let encode: Vec<_> = result.samples.iter().filter_map(|s| s.stages.as_ref().map(|t| t.encode)).collect();
        let input_to_pixels = Summary::of(&total).ok_or_else(|| crate::BenchError::Invalid("benchmark contains no samples".into()))?;
        let decode = Summary::of(&decode).ok_or_else(|| crate::BenchError::Invalid("benchmark contains no decode samples".into()))?;
        Ok(Self {
            iterations: result.samples.len(),
            passed: input_to_pixels.p95() <= budget,
            budget_ms: budget.as_secs_f64() * 1000.0,
            input_to_pixels,
            server_render: Summary::of(&render),
            capture_encode: Summary::of(&encode),
            decode,
        })
    }

    /// The steps and targets from `surfaces.md`; uninstrumented stages are explicitly unknown.
    pub fn table(&self) -> String {
        let mut out = String::from("Step                               Target              Min     Median      p95      p99      Max\n");
        out.push_str("--------------------------------------------------------------------------------------------------\n");
        row(&mut out, "Input reaches the server", "<1 ms local", None);
        row(&mut out, "Server reacts and renders a frame", "<=16 ms", self.server_render.as_ref());
        row(&mut out, "Capture and hardware encode", "<=5 ms", self.capture_encode.as_ref());
        row(&mut out, "Network", "<1 ms local", None);
        row(&mut out, "Decode", "<=3 ms", Some(&self.decode));
        row(&mut out, "Shown on next screen refresh", "<=16 ms", None);
        row(&mut out, "Total input to pixels", &format!("<={:.1} ms", self.budget_ms), Some(&self.input_to_pixels));
        out
    }

    /// JSON with min, median, p95, p99 and max in microseconds for measured stages.
    pub fn json(&self) -> crate::Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| crate::BenchError::Invalid(e.to_string()))
    }
}

fn row(out: &mut String, label: &str, target: &str, value: Option<&Summary>) {
    if let Some(s) = value {
        let _ = writeln!(out, "{label:<34} {target:<18} {:>6.2} {:>9.2} {:>8.2} {:>8.2} {:>8.2}",
            s.min_us as f64 / 1000.0, s.median_us as f64 / 1000.0,
            s.p95_us as f64 / 1000.0, s.p99_us as f64 / 1000.0, s.max_us as f64 / 1000.0);
    } else {
        let _ = writeln!(out, "{label:<34} {target:<18}       n/a       n/a      n/a      n/a      n/a");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::Sample;

    #[test]
    fn sixty_millisecond_sample_fails_fifty_millisecond_gate() {
        let result = BenchResult {
            baseline_seq: 0,
            samples: (1..=10).map(|seq| Sample { input_seq: seq, frame_seq: seq, latency: Duration::from_millis(60),
                decode: Duration::from_millis(1), pixel_difference: 100.0, stages: None }).collect(),
        };
        let report = LatencyReport::from_result(&result, Duration::from_millis(50)).unwrap();
        assert!(!report.passed);
        assert_eq!(report.input_to_pixels.p95_us, 60_000);
        assert!(report.table().contains("Total input to pixels"));
        assert!(report.json().unwrap().contains("60000"));
    }
}
