use netanvil_types::{MetricsSummary, RateController, RateDecision, TargetMetric};

/// Resolved PID gains (kp, ki, kd) for use with PID controllers.
pub struct PidGainValues {
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
}

/// PID rate controller that adjusts request rate to maintain a target metric.
///
/// Uses the shared `pid_step_fixed()` function for PID computation,
/// ensuring consistent anti-windup logic across all PID variants.
pub struct PidRateController {
    target_metric: TargetMetric,
    target_value: f64,
    current_rps: f64,
    min_rps: f64,
    max_rps: f64,
    kp: f64,
    ki: f64,
    kd: f64,
    state: super::autotune::PidState,
}

impl PidRateController {
    pub fn new(
        target_metric: TargetMetric,
        target_value: f64,
        initial_rps: f64,
        min_rps: f64,
        max_rps: f64,
        gains: PidGainValues,
    ) -> Self {
        Self {
            target_metric,
            target_value,
            current_rps: initial_rps,
            min_rps,
            max_rps,
            kp: gains.kp,
            ki: gains.ki,
            kd: gains.kd,
            state: super::autotune::PidState::new(1.0), // no smoothing for manual PID
        }
    }

    fn extract_current_value(&self, summary: &MetricsSummary) -> f64 {
        crate::controller::autotune::extract_metric(&self.target_metric, summary)
    }
}

impl RateController for PidRateController {
    fn update(&mut self, summary: &MetricsSummary) -> RateDecision {
        // Need enough samples for meaningful feedback
        if summary.total_requests < 5 {
            return RateDecision {
                target_rps: self.current_rps,
            };
        }

        let current_value = self.extract_current_value(summary);

        self.current_rps = super::autotune::pid_step_fixed(
            &super::autotune::PidStepInput {
                current_value,
                target_value: self.target_value,
                current_rps: self.current_rps,
                min_rps: self.min_rps,
                max_rps: self.max_rps,
                kp: self.kp,
                ki: self.ki,
                kd: self.kd,
            },
            &mut self.state,
        );

        RateDecision {
            target_rps: self.current_rps,
        }
    }

    fn current_rate(&self) -> f64 {
        self.current_rps
    }

    fn set_rate(&mut self, rps: f64) {
        self.current_rps = rps.clamp(self.min_rps, self.max_rps);
        // Reset integral to avoid fighting the new setpoint
        self.state.reset();
    }

    fn set_max_rps(&mut self, max_rps: f64) {
        self.max_rps = max_rps.max(self.min_rps);
        if self.current_rps > self.max_rps {
            self.current_rps = self.max_rps;
        }
    }
}
