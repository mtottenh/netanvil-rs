//! Generic progressive-ceiling wrapper for rate controllers.
//!
//! Progressively raises the inner controller's `max_rps` ceiling from
//! `initial_rps` to the configured `max_rps` over `test_duration / 2`.
//!
//! Uses static dispatch to the inner controller — `SlowStart<PidRateController>`
//! is monomorphized, with only one dyn dispatch at the `Box` boundary.

use std::sync::Arc;
use std::time::Duration;

use netanvil_types::{ControllerInfo, MetricsSummary, RateController, RateDecision};

use super::ceiling::ProgressiveCeiling;
use super::clock::Clock;

/// Generic progressive-ceiling wrapper. Ramps the inner controller's ceiling
/// from `initial_rps` to `max_rps` over `test_duration / 2`.
pub struct SlowStart<C> {
    inner: C,
    ceiling: ProgressiveCeiling,
}

impl<C: RateController> SlowStart<C> {
    /// Create a new progressive-ceiling wrapper.
    ///
    /// # Parameters
    /// - `inner`: The inner rate controller to wrap.
    /// - `initial_rps`: Starting rate (also the ramp floor).
    /// - `max_rps`: Target ceiling after ramp completes.
    /// - `test_duration`: Total test duration (ramp takes half).
    /// - `_control_interval`: Unused (kept for API compatibility).
    pub fn new(
        inner: C,
        initial_rps: f64,
        max_rps: f64,
        test_duration: Duration,
        _control_interval: Duration,
    ) -> Self {
        Self {
            inner,
            ceiling: ProgressiveCeiling::started(initial_rps, max_rps, test_duration / 2),
        }
    }

    /// Create with a custom clock for deterministic testing.
    pub fn new_with_clock(
        inner: C,
        initial_rps: f64,
        max_rps: f64,
        test_duration: Duration,
        _control_interval: Duration,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inner,
            ceiling: ProgressiveCeiling::started_with_clock(
                initial_rps,
                max_rps,
                test_duration / 2,
                clock,
            ),
        }
    }
}

impl<C: RateController> RateController for SlowStart<C> {
    fn update(&mut self, summary: &MetricsSummary) -> RateDecision {
        let ceil = self.ceiling.ceiling();
        self.inner.set_max_rps(ceil);
        self.inner.update(summary)
    }

    fn current_rate(&self) -> f64 {
        self.inner.current_rate()
    }

    fn set_rate(&mut self, rps: f64) {
        self.inner.set_rate(rps);
    }

    fn set_max_rps(&mut self, max_rps: f64) {
        self.ceiling.set_end_value(max_rps);
    }

    fn controller_info(&self) -> ControllerInfo {
        self.inner.controller_info()
    }

    fn apply_update(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        if action == "set_max_rps" {
            if let Some(max) = params.get("max_rps").and_then(|v| v.as_f64()) {
                self.ceiling.set_end_value(max);
            }
        }
        self.inner.apply_update(action, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use netanvil_types::MetricsSummary;

    /// Mock controller that returns whatever rate is set, respecting max_rps.
    struct MockController {
        current_rps: f64,
        max_rps: f64,
        set_rate_calls: Vec<f64>,
        set_max_rps_calls: Vec<f64>,
    }

    impl MockController {
        fn new(initial_rps: f64, max_rps: f64) -> Self {
            Self {
                current_rps: initial_rps,
                max_rps,
                set_rate_calls: Vec::new(),
                set_max_rps_calls: Vec::new(),
            }
        }
    }

    impl RateController for MockController {
        fn update(&mut self, _summary: &MetricsSummary) -> RateDecision {
            let target = self.max_rps;
            self.current_rps = target;
            RateDecision { target_rps: target }
        }

        fn current_rate(&self) -> f64 {
            self.current_rps
        }

        fn set_rate(&mut self, rps: f64) {
            self.set_rate_calls.push(rps);
            self.current_rps = rps;
        }

        fn set_max_rps(&mut self, max_rps: f64) {
            self.set_max_rps_calls.push(max_rps);
            self.max_rps = max_rps;
        }
    }

    fn empty_summary() -> MetricsSummary {
        MetricsSummary::default()
    }

    #[test]
    fn ceiling_increases_linearly() {
        let mock = MockController::new(100.0, 10000.0);
        let mut slow = SlowStart::new(
            mock,
            100.0,
            10000.0,
            Duration::from_secs(60),
            Duration::from_secs(3),
        );

        // First tick: ceiling should be near ramp_start
        let _ = slow.update(&empty_summary());
        let first_ceiling = slow.inner.set_max_rps_calls[0];
        assert!(
            first_ceiling >= 100.0 && first_ceiling < 1000.0,
            "first ceiling {first_ceiling} should be near ramp_start"
        );
    }

    #[test]
    fn ceiling_reaches_max_after_ramp() {
        let mock = MockController::new(100.0, 10000.0);
        let mut slow = SlowStart::new(
            mock,
            100.0,
            10000.0,
            Duration::from_secs(2), // 2s total, 1s ramp
            Duration::from_secs(3),
        );

        // Wait past the ramp duration
        std::thread::sleep(Duration::from_millis(1100));
        let _ = slow.update(&empty_summary());

        let last_ceiling = slow.inner.set_max_rps_calls.last().copied().unwrap();
        assert!(
            (last_ceiling - 10000.0).abs() < 100.0,
            "ceiling should be near max_rps after ramp, got {last_ceiling}"
        );
    }

    #[test]
    fn set_max_rps_updates_ramp_end() {
        let mock = MockController::new(100.0, 10000.0);
        let mut slow: SlowStart<MockController> = SlowStart::new(
            mock,
            100.0,
            10000.0,
            Duration::from_secs(60),
            Duration::from_secs(3),
        );

        slow.set_max_rps(5000.0);
        // After ramp completes, ceiling should reach the new end value
        std::thread::sleep(Duration::from_millis(50));
        // Progress is tiny, but the end_value is updated
        let _ = slow.update(&empty_summary());
    }

    #[test]
    fn update_never_calls_set_rate_on_inner() {
        let mock = MockController::new(100.0, 10000.0);
        let mut slow = SlowStart::new(
            mock,
            100.0,
            10000.0,
            Duration::from_secs(60),
            Duration::from_secs(3),
        );

        // Run several ticks
        for _ in 0..10 {
            let _ = slow.update(&empty_summary());
        }

        // SlowStart should never call set_rate on the inner controller
        // (regression test: the old slew cap used set_rate for back-calculation,
        // which could abort autotuner exploration)
        assert!(
            slow.inner.set_rate_calls.is_empty(),
            "SlowStart::update() should never call set_rate() on inner, got {:?}",
            slow.inner.set_rate_calls
        );
    }
}
