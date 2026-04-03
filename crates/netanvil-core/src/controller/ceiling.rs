//! Progressive ceiling: linearly ramps from `start_value` to `end_value`
//! over `ramp_duration`.
//!
//! Supports deferred start for controllers with a warmup phase — before
//! `start_now()` is called, `ceiling()` returns `start_value`.
//!
//! Used by the Arbiter (deferred start after warmup completes).

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::clock::{Clock, SystemClock};

/// Linear ceiling ramp from `start_value` to `end_value` over `ramp_duration`.
pub struct ProgressiveCeiling {
    start_value: f64,
    end_value: f64,
    ramp_duration: Duration,
    start_time: Option<Instant>,
    last_milestone: u8,
    clock: Arc<dyn Clock>,
}

impl ProgressiveCeiling {
    /// Create a new ceiling ramp without starting the clock.
    /// Call `start_now()` to begin the ramp. Uses system clock.
    pub fn new(start_value: f64, end_value: f64, ramp_duration: Duration) -> Self {
        Self::new_with_clock(start_value, end_value, ramp_duration, Arc::new(SystemClock))
    }

    /// Create a new ceiling ramp with a custom clock.
    pub fn new_with_clock(
        start_value: f64,
        end_value: f64,
        ramp_duration: Duration,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            start_value,
            end_value,
            ramp_duration,
            start_time: None,
            last_milestone: 0,
            clock,
        }
    }

    /// Create and immediately start the ceiling ramp. Uses system clock.
    pub fn started(start_value: f64, end_value: f64, ramp_duration: Duration) -> Self {
        Self::started_with_clock(start_value, end_value, ramp_duration, Arc::new(SystemClock))
    }

    /// Create and immediately start the ceiling ramp with a custom clock.
    pub fn started_with_clock(
        start_value: f64,
        end_value: f64,
        ramp_duration: Duration,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let start_time = Some(clock.now());
        Self {
            start_value,
            end_value,
            ramp_duration,
            start_time,
            last_milestone: 0,
            clock,
        }
    }

    /// Start the clock. Typically called when a warmup phase ends.
    pub fn start_now(&mut self) {
        self.start_time = Some(self.clock.now());
    }

    /// Current ceiling value.
    ///
    /// Returns `start_value` if the clock hasn't been started yet.
    /// Ramps linearly to `end_value` over `ramp_duration`, then stays
    /// at `end_value`.
    pub fn ceiling(&self) -> f64 {
        self.start_value + self.progress() * (self.end_value - self.start_value)
    }

    /// Progress fraction in `[0.0, 1.0]`. Returns 0.0 if not started.
    pub fn progress(&self) -> f64 {
        match self.start_time {
            Some(t) => {
                let secs = self.ramp_duration.as_secs_f64();
                if secs <= 0.0 {
                    1.0
                } else {
                    (self.clock.elapsed_since(t).as_secs_f64() / secs).min(1.0)
                }
            }
            None => 0.0,
        }
    }

    /// Update the ramp endpoint. The current progress fraction is preserved.
    pub fn set_end_value(&mut self, end: f64) {
        self.end_value = end;
    }

    /// Check if a new 25% milestone has been crossed since the last call.
    /// Returns the milestone percentage (25, 50, 75, 100) or `None`.
    pub fn check_milestone(&mut self) -> Option<u8> {
        let pct = (self.progress() * 100.0) as u8;
        let milestone = (pct / 25) * 25;
        if milestone > self.last_milestone {
            self.last_milestone = milestone;
            Some(milestone)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_start_before_started() {
        let c = ProgressiveCeiling::new(100.0, 10000.0, Duration::from_secs(60));
        assert_eq!(c.ceiling(), 100.0);
        assert_eq!(c.progress(), 0.0);
    }

    #[test]
    fn ramps_after_start() {
        let c = ProgressiveCeiling::started(100.0, 10000.0, Duration::from_millis(100));
        // Immediately after start, ceiling is near start_value
        assert!(c.ceiling() < 1000.0);
        std::thread::sleep(Duration::from_millis(120));
        // After ramp_duration, ceiling is at end_value
        assert!((c.ceiling() - 10000.0).abs() < 1.0);
        assert!((c.progress() - 1.0).abs() < 0.01);
    }

    #[test]
    fn deferred_start() {
        let mut c = ProgressiveCeiling::new(100.0, 10000.0, Duration::from_millis(100));
        assert_eq!(c.ceiling(), 100.0);
        c.start_now();
        std::thread::sleep(Duration::from_millis(120));
        assert!((c.ceiling() - 10000.0).abs() < 1.0);
    }

    #[test]
    fn set_end_value() {
        let mut c = ProgressiveCeiling::started(100.0, 10000.0, Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(120));
        assert!((c.ceiling() - 10000.0).abs() < 1.0);

        c.set_end_value(5000.0);
        assert!((c.ceiling() - 5000.0).abs() < 1.0);
    }

    #[test]
    fn zero_duration_gives_end_value() {
        let c = ProgressiveCeiling::started(100.0, 10000.0, Duration::ZERO);
        assert_eq!(c.ceiling(), 10000.0);
        assert_eq!(c.progress(), 1.0);
    }

    #[test]
    fn milestone_detection() {
        let mut c = ProgressiveCeiling::started(0.0, 100.0, Duration::from_millis(100));
        assert_eq!(c.check_milestone(), None); // 0% — not a new milestone

        std::thread::sleep(Duration::from_millis(30));
        // ~30% progress → 25% milestone
        assert_eq!(c.check_milestone(), Some(25));
        // Same milestone shouldn't fire again
        assert_eq!(c.check_milestone(), None);

        std::thread::sleep(Duration::from_millis(30));
        // ~60% → 50% milestone
        assert_eq!(c.check_milestone(), Some(50));

        std::thread::sleep(Duration::from_millis(50));
        // 100%+ → should get 75 or 100
        let m = c.check_milestone();
        assert!(m == Some(75) || m == Some(100));
    }
}
