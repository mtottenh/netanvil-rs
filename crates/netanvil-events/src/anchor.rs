//! Time anchor for converting monotonic `Instant` to wall-clock epoch.
//!
//! `Instant` is monotonic but has no relation to wall-clock time. We capture
//! a `(SystemTime, Instant)` pair once at engine startup and use it to convert
//! all subsequent `Instant` values to epoch microseconds.

use std::time::{Instant, SystemTime};

/// Anchor point for converting `Instant` → epoch microseconds.
///
/// Captured once at engine start, then `Copy`-moved to each I/O worker.
#[derive(Debug, Clone, Copy)]
pub struct TimeAnchor {
    epoch_us: u64,
    instant: Instant,
}

impl TimeAnchor {
    /// Capture the current wall-clock and monotonic time as an anchor.
    pub fn now() -> Self {
        let instant = Instant::now();
        let epoch_us = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
        Self { epoch_us, instant }
    }

    /// Convert a monotonic `Instant` to epoch microseconds.
    pub fn to_epoch_us(&self, t: Instant) -> u64 {
        if t >= self.instant {
            self.epoch_us + t.duration_since(self.instant).as_micros() as u64
        } else {
            self.epoch_us - self.instant.duration_since(t).as_micros() as u64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn anchor_converts_monotonically() {
        let anchor = TimeAnchor::now();
        let t1 = Instant::now();
        std::thread::sleep(Duration::from_millis(1));
        let t2 = Instant::now();

        let e1 = anchor.to_epoch_us(t1);
        let e2 = anchor.to_epoch_us(t2);
        assert!(e2 > e1, "epoch must increase: {e2} > {e1}");
        assert!(e2 - e1 >= 1000, "at least 1ms gap: {} us", e2 - e1);
    }

    #[test]
    fn anchor_handles_past_instant() {
        let before = Instant::now();
        std::thread::sleep(Duration::from_millis(1));
        let anchor = TimeAnchor::now();

        let epoch = anchor.to_epoch_us(before);
        assert!(
            epoch < anchor.epoch_us,
            "past instant should be before anchor epoch"
        );
    }
}
