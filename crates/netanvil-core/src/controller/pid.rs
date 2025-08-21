use std::time::Duration;

use netanvil_types::{MetricsSummary, RateController, RateDecision, TargetMetric};

/// Resolved PID gains (kp, ki, kd) for use with PID controllers.
pub struct PidGainValues {
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
}

/// PID rate controller that adjusts request rate to maintain a target metric.
///
/// The controller uses a standard PID algorithm:
///   output = kp * error + ki * integral + kd * derivative
///
/// The output is then applied as a proportional adjustment to the current rate,
/// clamped to prevent runaway changes.
pub struct PidRateController {
    target_metric: TargetMetric,
    target_value: f64,
    current_rps: f64,
    min_rps: f64,
    max_rps: f64,
    kp: f64,
    ki: f64,
    kd: f64,
    integral: f64,
    last_error: f64,
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
            integral: 0.0,
            last_error: 0.0,
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
                next_update_interval: Duration::from_millis(100),
            };
        }

        let current_value = self.extract_current_value(summary);

        // For latency: error > 0 means we're below target (good, can increase rate)
        // For error rate: error > 0 means we're below target (good)
        let error = self.target_value - current_value;

        // Integral with anti-windup
        self.integral += error;
        self.integral = self.integral.clamp(-1000.0, 1000.0);

        // Derivative
        let derivative = error - self.last_error;
        self.last_error = error;

        // PID output
        let output = self.kp * error + self.ki * self.integral + self.kd * derivative;

        // Apply as proportional adjustment, clamped to prevent wild swings
        let scale_factor = 0.05;
        let adjustment = (output * scale_factor).clamp(-0.20, 0.20);
        let new_rps = self.current_rps * (1.0 + adjustment);
        self.current_rps = new_rps.clamp(self.min_rps, self.max_rps);

        RateDecision {
            target_rps: self.current_rps,
            next_update_interval: Duration::from_millis(100),
        }
    }

    fn current_rate(&self) -> f64 {
        self.current_rps
    }

    fn set_rate(&mut self, rps: f64) {
        self.current_rps = rps.clamp(self.min_rps, self.max_rps);
        // Reset integral to avoid fighting the new setpoint
        self.integral = 0.0;
        self.last_error = 0.0;
    }
}
