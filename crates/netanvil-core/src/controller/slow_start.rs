//! Generic slow-start wrapper for rate controllers.
//!
//! Progressively raises the inner controller's `max_rps` ceiling from
//! `initial_rps` to the configured `max_rps` over `test_duration / 2`.
//! Also applies an asymmetric slew rate cap: increases are limited to
//! `max_increase_per_tick`, while decreases are uncapped (immediate backoff).
//!
//! Uses static dispatch to the inner controller — `SlowStart<PidRateController>`
//! is monomorphized, with only one dyn dispatch at the `Box` boundary.

use std::time::{Duration, Instant};

use netanvil_types::{MetricsSummary, RateController, RateDecision};

/// Generic slow-start wrapper. Ramps the inner controller's ceiling
/// from `ramp_start` to `ramp_end` over `ramp_duration`, and caps
/// per-tick rate increases to `max_increase_per_tick`.
pub struct SlowStart<C> {
    inner: C,
    ramp_start: f64,
    ramp_end: f64,
    ramp_duration: Duration,
    max_increase_per_tick: f64,
    start_time: Instant,
    last_rate: f64,
}

impl<C: RateController> SlowStart<C> {
    /// Create a new slow-start wrapper.
    ///
    /// # Parameters
    /// - `inner`: The inner rate controller to wrap.
    /// - `initial_rps`: Starting rate (also the ramp floor).
    /// - `max_rps`: Target ceiling after ramp completes.
    /// - `test_duration`: Total test duration (ramp takes half).
    /// - `control_interval`: Coordinator tick interval (for slew calc).
    pub fn new(
        inner: C,
        initial_rps: f64,
        max_rps: f64,
        test_duration: Duration,
        control_interval: Duration,
    ) -> Self {
        let ramp_duration = test_duration / 2;
        let ramp_start = initial_rps;
        let ramp_end = max_rps;

        // Slew cap: how much rate can increase per tick during the ramp.
        // After the ramp completes this still limits per-tick increases,
        // preventing wild oscillations during steady-state PID operation.
        let max_increase_per_tick = if ramp_duration.as_secs_f64() > 0.0 {
            (ramp_end - ramp_start) * control_interval.as_secs_f64()
                / ramp_duration.as_secs_f64()
        } else {
            f64::INFINITY
        };

        Self {
            inner,
            ramp_start,
            ramp_end,
            ramp_duration,
            max_increase_per_tick,
            start_time: Instant::now(),
            last_rate: initial_rps,
        }
    }
}

impl<C: RateController> RateController for SlowStart<C> {
    fn update(&mut self, summary: &MetricsSummary) -> RateDecision {
        // 1. Progressively raise the inner controller's max_rps
        let elapsed = self.start_time.elapsed();
        let progress =
            (elapsed.as_secs_f64() / self.ramp_duration.as_secs_f64()).min(1.0);
        let ceiling = self.ramp_start + progress * (self.ramp_end - self.ramp_start);
        self.inner.set_max_rps(ceiling);

        // 2. Delegate to inner controller
        let mut decision = self.inner.update(summary);

        // 3. Slew rate cap (increases only; decreases are uncapped for fast backoff)
        let max_rate = self.last_rate + self.max_increase_per_tick;
        if decision.target_rps > max_rate {
            decision.target_rps = max_rate;
            self.inner.set_rate(max_rate); // back-calculation
        }

        self.last_rate = decision.target_rps;
        decision
    }

    fn current_rate(&self) -> f64 {
        self.inner.current_rate()
    }

    fn set_rate(&mut self, rps: f64) {
        self.inner.set_rate(rps);
        self.last_rate = rps;
    }

    fn set_max_rps(&mut self, max_rps: f64) {
        self.ramp_end = max_rps;
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
            // Simulate a PID that always wants to jump to max_rps
            let target = self.max_rps;
            self.current_rps = target;
            RateDecision {
                target_rps: target,
            }
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
    fn slew_rate_caps_increases() {
        /// Controller that always wants to jump to a fixed high rate,
        /// ignoring its ceiling (simulates aggressive PID output).
        struct AggressiveController {
            current_rps: f64,
            desired: f64,
            set_rate_calls: Vec<f64>,
        }

        impl RateController for AggressiveController {
            fn update(&mut self, _summary: &MetricsSummary) -> RateDecision {
                self.current_rps = self.desired;
                RateDecision {
                    target_rps: self.desired,
                }
            }
            fn current_rate(&self) -> f64 {
                self.current_rps
            }
            fn set_rate(&mut self, rps: f64) {
                self.set_rate_calls.push(rps);
                self.current_rps = rps;
            }
            fn set_max_rps(&mut self, _max_rps: f64) {
                // Deliberately ignore ceiling to test slew cap
            }
        }

        let inner = AggressiveController {
            current_rps: 100.0,
            desired: 50000.0,
            set_rate_calls: Vec::new(),
        };
        let mut slow = SlowStart::new(
            inner,
            100.0,
            10000.0,
            Duration::from_secs(60),
            Duration::from_secs(3),
        );

        // max_increase_per_tick = (10000 - 100) * 3.0 / 30.0 = 990.0
        let decision = slow.update(&empty_summary());

        // The inner controller tries to jump to 50000, slew cap should limit
        assert!(
            decision.target_rps <= 100.0 + 990.0 + 1.0, // +1 for float tolerance
            "rate {} should be capped by slew limit",
            decision.target_rps
        );

        // Back-calculation: set_rate should have been called
        assert!(
            !slow.inner.set_rate_calls.is_empty(),
            "set_rate should be called for back-calculation when slew fires"
        );
    }

    #[test]
    fn decreases_are_uncapped() {
        /// Controller that always wants to decrease
        struct DecreasingController {
            current_rps: f64,
            max_rps: f64,
        }

        impl RateController for DecreasingController {
            fn update(&mut self, _summary: &MetricsSummary) -> RateDecision {
                // Simulate a PID that wants to halve the rate (error response)
                self.current_rps *= 0.5;
                RateDecision {
                    target_rps: self.current_rps,
                }
            }
            fn current_rate(&self) -> f64 {
                self.current_rps
            }
            fn set_rate(&mut self, rps: f64) {
                self.current_rps = rps;
            }
            fn set_max_rps(&mut self, max_rps: f64) {
                self.max_rps = max_rps;
            }
        }

        let inner = DecreasingController {
            current_rps: 5000.0,
            max_rps: 10000.0,
        };
        let mut slow = SlowStart::new(
            inner,
            100.0,
            10000.0,
            Duration::from_secs(60),
            Duration::from_secs(3),
        );
        slow.last_rate = 5000.0;

        let decision = slow.update(&empty_summary());
        // Rate should drop freely to 2500 — not capped
        assert!(
            decision.target_rps < 3000.0,
            "decrease should not be capped, got {}",
            decision.target_rps
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
        let mut slow = SlowStart::new(
            mock,
            100.0,
            10000.0,
            Duration::from_secs(60),
            Duration::from_secs(3),
        );

        slow.set_max_rps(5000.0);
        assert_eq!(slow.ramp_end, 5000.0);
    }
}
