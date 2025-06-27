use std::time::{Duration, Instant};

use netanvil_types::{MetricsSummary, RateController, RateDecision};

/// Rate controller that changes rates at predefined time offsets.
///
/// Steps are `(offset_from_start, rps)` pairs. The controller looks up
/// the current step based on elapsed time since construction.
pub struct StepRateController {
    steps: Vec<(Duration, f64)>,
    current_rps: f64,
    start_time: Instant,
}

impl StepRateController {
    pub fn new(steps: Vec<(Duration, f64)>) -> Self {
        assert!(
            !steps.is_empty(),
            "step controller requires at least one step"
        );
        let initial_rps = steps[0].1;
        Self {
            steps,
            current_rps: initial_rps,
            start_time: Instant::now(),
        }
    }

    /// Create with an explicit start time (for testing).
    pub fn with_start_time(steps: Vec<(Duration, f64)>, start_time: Instant) -> Self {
        assert!(
            !steps.is_empty(),
            "step controller requires at least one step"
        );
        let initial_rps = steps[0].1;
        Self {
            steps,
            current_rps: initial_rps,
            start_time,
        }
    }

    fn update_from_time(&mut self) {
        let elapsed = self.start_time.elapsed();
        let mut rps = self.steps[0].1;
        for &(offset, step_rps) in &self.steps {
            if elapsed >= offset {
                rps = step_rps;
            } else {
                break;
            }
        }
        self.current_rps = rps;
    }
}

impl RateController for StepRateController {
    fn update(&mut self, _summary: &MetricsSummary) -> RateDecision {
        self.update_from_time();
        RateDecision {
            target_rps: self.current_rps,
            next_update_interval: Duration::from_millis(100),
        }
    }

    fn current_rate(&self) -> f64 {
        self.current_rps
    }

    fn set_rate(&mut self, rps: f64) {
        // Override the current step; next time-based update will reclaim
        self.current_rps = rps;
    }
}
