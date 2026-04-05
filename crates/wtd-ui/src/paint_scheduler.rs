use std::time::{Duration, Instant};

/// Coalesces visible pane repaints so bursty alternate-screen output can settle
/// before the window presents a frame.
#[derive(Debug)]
pub struct PaintScheduler {
    pending: bool,
    defer_until: Option<Instant>,
    alt_screen_coalesce: Duration,
}

impl Default for PaintScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl PaintScheduler {
    /// Default quiet-window used for visible alternate-screen redraw bursts.
    pub const DEFAULT_ALT_SCREEN_COALESCE: Duration = Duration::from_millis(10);

    pub fn new() -> Self {
        Self::with_alt_screen_coalesce(Self::DEFAULT_ALT_SCREEN_COALESCE)
    }

    pub fn with_alt_screen_coalesce(alt_screen_coalesce: Duration) -> Self {
        Self {
            pending: false,
            defer_until: None,
            alt_screen_coalesce,
        }
    }

    /// Queue a repaint that should present on the next loop iteration.
    pub fn request_immediate(&mut self) {
        self.pending = true;
        self.defer_until = None;
    }

    /// Queue a repaint for a burst of alternate-screen output. Each call
    /// refreshes the quiet-window so we paint after output goes idle.
    pub fn request_alt_screen_burst(&mut self, now: Instant) {
        self.pending = true;
        self.defer_until = if self.alt_screen_coalesce.is_zero() {
            None
        } else {
            Some(now + self.alt_screen_coalesce)
        };
    }

    pub fn should_paint_now(&self, now: Instant) -> bool {
        if !self.pending {
            return false;
        }

        match self.defer_until {
            Some(deadline) => now >= deadline,
            None => true,
        }
    }

    pub fn complete_paint(&mut self) {
        self.pending = false;
        self.defer_until = None;
    }

    /// Clamp the idle sleep to the next deferred paint deadline so the loop
    /// wakes promptly when an alternate-screen burst settles.
    pub fn sleep_interval(&self, default_sleep: Duration, now: Instant) -> Duration {
        match self.defer_until {
            Some(deadline) if deadline > now => default_sleep.min(deadline - now),
            _ => default_sleep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_requests_paint_without_delay() {
        let mut scheduler = PaintScheduler::with_alt_screen_coalesce(Duration::from_millis(10));
        let now = Instant::now();

        scheduler.request_immediate();

        assert!(scheduler.should_paint_now(now));
    }

    #[test]
    fn alt_screen_burst_waits_for_quiet_window() {
        let mut scheduler = PaintScheduler::with_alt_screen_coalesce(Duration::from_millis(10));
        let start = Instant::now();

        scheduler.request_alt_screen_burst(start);

        assert!(!scheduler.should_paint_now(start));
        assert!(!scheduler.should_paint_now(start + Duration::from_millis(9)));
        assert!(scheduler.should_paint_now(start + Duration::from_millis(10)));
    }

    #[test]
    fn immediate_request_overrides_deferred_alt_screen_paint() {
        let mut scheduler = PaintScheduler::with_alt_screen_coalesce(Duration::from_millis(10));
        let start = Instant::now();

        scheduler.request_alt_screen_burst(start);
        scheduler.request_immediate();

        assert!(scheduler.should_paint_now(start));
    }

    #[test]
    fn sleep_interval_clamps_to_next_deferred_paint_deadline() {
        let scheduler = PaintScheduler::with_alt_screen_coalesce(Duration::from_millis(10));
        let start = Instant::now();
        let mut scheduler = scheduler;
        scheduler.request_alt_screen_burst(start);

        let sleep = scheduler.sleep_interval(Duration::from_millis(16), start);
        assert_eq!(sleep, Duration::from_millis(10));
    }
}
