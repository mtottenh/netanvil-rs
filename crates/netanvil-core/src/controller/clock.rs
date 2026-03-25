//! Abstraction over time for deterministic testing.
//!
//! Controllers call `clock.now()` instead of `Instant::now()`, and
//! `clock.elapsed_since(t)` instead of `t.elapsed()`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Abstraction over time. Controllers store `Arc<dyn Clock>` and use it
/// for all time queries so shadow validation can inject deterministic time.
pub trait Clock: Send + Sync {
    /// Returns the current time.
    fn now(&self) -> Instant;

    /// Equivalent to `self.now().duration_since(t)`.
    /// Replaces `t.elapsed()` which internally calls `Instant::now()`.
    fn elapsed_since(&self, t: Instant) -> Duration {
        self.now().duration_since(t)
    }
}

/// Real wall-clock time. Used in production.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Deterministic clock for tests. Time only advances when `advance()` is called.
pub struct TestClock {
    inner: Mutex<Instant>,
}

impl TestClock {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Instant::now()),
        }
    }

    /// Advance the clock by the given duration.
    pub fn advance(&self, d: Duration) {
        let mut t = self.inner.lock().unwrap();
        *t += d;
    }
}

impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self.inner.lock().unwrap()
    }
}

/// Convenience: create an `Arc<dyn Clock>` backed by `SystemClock`.
pub fn system_clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}
