//! The shimmer clock of the editor's magic keywords and the working indicator: [`PHASES`] steps
//! per cycle, read from wall-clock time so everything that shimmers moves in step whatever
//! rate it ticks at.

use std::time::Duration;

use iced::Subscription;

/// Steps of the shimmer per cycle.
pub(crate) const PHASES: u8 = 56;
/// One shimmer cycle, as in omp.
const CYCLE_MS: u64 = 1800;
/// Ticks that reach every step, for the keyword gradient.
pub(crate) const EVERY_STEP: Duration = Duration::from_millis(CYCLE_MS / PHASES as u64);

/// The step at `now_ms`, Unix milliseconds.
pub(crate) fn phase_at(now_ms: i64) -> u8 {
    let ms = u64::try_from(now_ms).unwrap_or_default() % CYCLE_MS;
    u8::try_from(ms * u64::from(PHASES) / CYCLE_MS).unwrap_or_default()
}

/// How far `phase` is through its cycle, `0.0..1.0`.
pub(crate) fn progress(phase: u8) -> f32 { f32::from(phase) / f32::from(PHASES) }

/// The wall-clock time, Unix milliseconds, `every` so often.
pub(crate) fn ticks(every: Duration) -> Subscription<i64> { iced::time::every(every).map(|_| loa_util::time::now_ms()) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phase_steps_through_one_cycle_and_loops() {
        let cycle = i64::try_from(CYCLE_MS).unwrap();
        assert_eq!((phase_at(0), phase_at(cycle / 2), phase_at(cycle - 1), phase_at(cycle)), (0, PHASES / 2, PHASES - 1, 0));
        assert_eq!(progress(PHASES / 2), 0.5);
    }
}
