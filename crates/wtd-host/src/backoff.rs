//! Exponential backoff for session restart scheduling (spec &sect;17.8).
//!
//! Delay schedule: 500ms, 1s, 2s, 4s, 8s, 16s, 30s cap.
//! Resets after 60 seconds of stable running.

use std::time::{Duration, Instant};

const BASE_DELAY_MS: u64 = 500;
const MAX_DELAY_MS: u64 = 30_000;
const STABLE_RUN_SECS: u64 = 60;

/// Tracks restart attempts and computes exponential backoff delays.
pub struct BackoffState {
    restart_count: u32,
    last_start: Option<Instant>,
}

impl BackoffState {
    pub fn new() -> Self {
        Self {
            restart_count: 0,
            last_start: None,
        }
    }

    /// Record that the session has (re)started.
    pub fn record_start(&mut self) {
        self.last_start = Some(Instant::now());
    }

    /// Compute the next restart delay and increment the restart counter.
    ///
    /// If the session ran for longer than 60 seconds since `record_start`,
    /// the counter resets first (preventing restart storms for transient failures).
    pub fn next_delay(&mut self) -> Duration {
        self.maybe_reset();
        self.restart_count += 1;
        let exponent = (self.restart_count - 1).min(31);
        let delay_ms = BASE_DELAY_MS
            .saturating_mul(1u64 << exponent)
            .min(MAX_DELAY_MS);
        Duration::from_millis(delay_ms)
    }

    /// Current restart count (0 = no restarts yet).
    pub fn restart_count(&self) -> u32 {
        self.restart_count
    }

    /// Reset the counter if the session has been stable for >= 60 seconds.
    fn maybe_reset(&mut self) {
        if let Some(start) = self.last_start {
            if start.elapsed() >= Duration::from_secs(STABLE_RUN_SECS) {
                self.restart_count = 0;
            }
        }
    }

    /// Override `last_start` for testing (simulate a session that started in the past).
    #[cfg(test)]
    pub fn set_last_start(&mut self, instant: Instant) {
        self.last_start = Some(instant);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_delay_progression() {
        let mut b = BackoffState::new();
        b.record_start();
        assert_eq!(b.next_delay(), Duration::from_millis(500));
        assert_eq!(b.next_delay(), Duration::from_millis(1_000));
        assert_eq!(b.next_delay(), Duration::from_millis(2_000));
        assert_eq!(b.next_delay(), Duration::from_millis(4_000));
        assert_eq!(b.next_delay(), Duration::from_millis(8_000));
        assert_eq!(b.next_delay(), Duration::from_millis(16_000));
        assert_eq!(b.next_delay(), Duration::from_millis(30_000));
        // Further calls stay at cap
        assert_eq!(b.next_delay(), Duration::from_millis(30_000));
    }

    #[test]
    fn backoff_resets_after_stable_run() {
        let mut b = BackoffState::new();
        // Simulate a session that started 61 seconds ago
        b.set_last_start(Instant::now() - Duration::from_secs(61));
        // Accumulate some restarts first
        b.restart_count = 5;
        // next_delay should reset the counter then compute from restart_count=1
        let delay = b.next_delay();
        assert_eq!(delay, Duration::from_millis(500));
        assert_eq!(b.restart_count(), 1);
    }

    #[test]
    fn backoff_does_not_reset_before_stable_threshold() {
        let mut b = BackoffState::new();
        b.record_start(); // just started
        b.restart_count = 3;
        let delay = b.next_delay();
        // restart_count was 3, now 4 → 500 * 2^3 = 4000
        assert_eq!(delay, Duration::from_millis(4_000));
        assert_eq!(b.restart_count(), 4);
    }

    #[test]
    fn backoff_no_start_recorded() {
        let mut b = BackoffState::new();
        // No record_start called — should still work (no reset possible)
        assert_eq!(b.next_delay(), Duration::from_millis(500));
        assert_eq!(b.next_delay(), Duration::from_millis(1_000));
    }
}
