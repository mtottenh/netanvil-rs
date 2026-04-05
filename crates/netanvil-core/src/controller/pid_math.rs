//! Shared PID math: gain scheduling, PID state, back-calculation tracking.
//!
//! Used by `PidConstraint` (log-rate-space PID) and `ExplorationManager`
//! (metric extraction). Exploration and Cohen-Coon remain in `autotune`.

use netanvil_types::{MetricsSummary, TargetMetric};

// ---------------------------------------------------------------------------
// Metric extraction (shared by all PID controllers)
// ---------------------------------------------------------------------------

/// Extract the current value of a target metric from a MetricsSummary.
pub fn extract_metric(metric: &TargetMetric, summary: &MetricsSummary) -> f64 {
    match metric {
        TargetMetric::LatencyP50 => summary.latency_p50_ns as f64 / 1_000_000.0, // ns → ms
        TargetMetric::LatencyP90 => summary.latency_p90_ns as f64 / 1_000_000.0,
        TargetMetric::LatencyP99 => summary.latency_p99_ns as f64 / 1_000_000.0,
        TargetMetric::ErrorRate => summary.error_rate * 100.0, // fraction → percentage
        TargetMetric::ThroughputSend => summary.throughput_send_bps / 125_000.0, // bytes/s → Mbps
        TargetMetric::ThroughputRecv => summary.throughput_recv_bps / 125_000.0,
        TargetMetric::External { name } => summary
            .external_signals
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| *v)
            .unwrap_or(0.0),
    }
}

// ---------------------------------------------------------------------------
// Gain scheduling
// ---------------------------------------------------------------------------

/// Gain multipliers for a specific operating region.
#[derive(Debug, Clone, Copy)]
pub struct GainMultipliers {
    pub kp_scale: f64,
    pub ki_scale: f64,
    pub kd_scale: f64,
    pub reset_integral: bool,
}

/// Determine gain multipliers based on normalized error (error / target).
///
/// Regions:
/// - Ramp-up: far below target (error > 40% of target)
/// - Approach: closing in (10-40%)
/// - Tracking: near target (±10%)
/// - Overshoot: above target (10-30% over)
/// - Critical: way above target (>30% over)
pub fn gain_schedule(normalized_error: f64) -> GainMultipliers {
    match normalized_error {
        e if e > 0.4 => GainMultipliers {
            kp_scale: 2.0,
            ki_scale: 0.5,
            kd_scale: 1.0,
            reset_integral: false,
        },
        e if e > 0.1 => GainMultipliers {
            kp_scale: 1.0,
            ki_scale: 0.8,
            kd_scale: 1.5,
            reset_integral: false,
        },
        e if e > -0.1 => GainMultipliers {
            kp_scale: 0.7,
            ki_scale: 1.0,
            kd_scale: 1.0,
            reset_integral: false,
        },
        e if e > -0.3 => GainMultipliers {
            kp_scale: 1.5,
            ki_scale: 0.3,
            kd_scale: 2.0,
            reset_integral: false,
        },
        _ => GainMultipliers {
            kp_scale: 3.0,
            ki_scale: 0.0,
            kd_scale: 2.5,
            reset_integral: true,
        },
    }
}

// ---------------------------------------------------------------------------
// PID step computation (shared by all controllers)
// ---------------------------------------------------------------------------

/// PID state for one control loop.
#[derive(Debug, Clone)]
pub struct PidState {
    pub integral: f64,
    pub last_error: f64,
    pub ema_value: f64,
    pub ema_alpha: f64,
    pub ema_initialized: bool,
}

impl Default for PidState {
    fn default() -> Self {
        Self {
            integral: 0.0,
            last_error: 0.0,
            ema_value: 0.0,
            ema_alpha: 0.0,
            ema_initialized: false,
        }
    }
}

impl PidState {
    pub fn new(smoothing: f64) -> Self {
        Self {
            integral: 0.0,
            last_error: 0.0,
            ema_value: 0.0,
            ema_alpha: smoothing.clamp(0.0, 1.0),
            ema_initialized: false,
        }
    }

    /// Apply EMA smoothing to a raw metric value.
    pub fn smooth(&mut self, raw: f64) -> f64 {
        if !self.ema_initialized {
            self.ema_value = raw;
            self.ema_initialized = true;
        } else {
            self.ema_value = self.ema_alpha * raw + (1.0 - self.ema_alpha) * self.ema_value;
        }
        self.ema_value
    }

    /// Reset integral, derivative, and EMA state.
    pub fn reset(&mut self) {
        self.integral = 0.0;
        self.last_error = 0.0;
        self.ema_value = 0.0;
        self.ema_initialized = false;
    }
}

/// Back-calculation tracking for non-binding PID constraints.
///
/// When a PID constraint is not binding (another constraint selected a lower
/// rate), the integrator needs to track the actual output so it can resume
/// bumplessly if it becomes binding. This is the standard "external reset
/// feedback" technique from override control.
///
/// Operates in **log-rate space**: `u = ln(rate)`. The PID forward path is
/// `u_new = u_current + kp·error + ki·integral + kd·derivative`, so we can
/// invert to find the integral that would have produced the selected rate.
///
/// When `ki ≈ 0` (P-only, PD, or critical region with ki_scale=0), there is
/// no integral action and nothing to track — the function is a no-op.
pub fn pid_back_calculate_log(
    state: &mut PidState,
    selected_rate: f64,
    current_rate: f64,
    kp: f64,
    ki: f64,
    kd: f64,
    error: f64,
    tracking_gain: f64,
) {
    // Compute derivative BEFORE updating last_error — otherwise
    // derivative = error - error = 0 always (the audit-caught bug).
    let derivative = error - state.last_error;

    // Update last_error so the derivative is correct on the next tick.
    state.last_error = error;

    // Guard: no integral action → nothing to track.
    if ki.abs() < 1e-12 {
        return;
    }

    // Back-calculate in log-rate space.
    let u_selected = selected_rate.max(1e-9).ln();
    let u_current = current_rate.max(1e-9).ln();
    let required_integral = (u_selected - u_current - kp * error - kd * derivative) / ki;
    state.integral += tracking_gain * (required_integral - state.integral);
    state.integral = state.integral.clamp(-1000.0, 1000.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_calculate_tracks_toward_selected_rate() {
        let mut state = PidState::new(1.0);
        state.integral = 10.0;
        state.last_error = 5.0;

        // Selected rate is 1200, current is 1000, so u_selected > u_current.
        // With kp=0.5 and error=20, the required integral should be such that
        // u_current + kp*error + ki*integral = u_selected.
        pid_back_calculate_log(&mut state, 1200.0, 1000.0, 0.5, 0.1, 0.0, 20.0, 0.5);

        // Integral should have moved toward the required value
        assert_ne!(state.integral, 10.0, "integral should have changed");
        assert_eq!(state.last_error, 20.0, "last_error should be updated");
    }

    #[test]
    fn back_calculate_skips_when_ki_zero() {
        let mut state = PidState::new(1.0);
        state.integral = 10.0;
        state.last_error = 5.0;

        pid_back_calculate_log(&mut state, 1200.0, 1000.0, 0.5, 0.0, 0.0, 20.0, 0.5);

        // ki=0 → no integral action → integral unchanged
        assert_eq!(state.integral, 10.0);
        // last_error still updated
        assert_eq!(state.last_error, 20.0);
    }

    #[test]
    fn back_calculate_bumpless_transfer() {
        // Simulate: PID would have produced rate=1000. The arbiter also selected
        // rate=1000 (we are binding). Back-calculation should leave the integral
        // approximately unchanged, since the "required" integral matches the
        // current integral.
        let mut state = PidState::new(1.0);
        let current_rate: f64 = 1000.0;
        let kp: f64 = 0.5;
        let ki: f64 = 0.1;
        let kd: f64 = 0.0;
        let error: f64 = 10.0;
        state.last_error = error; // derivative = 0
                                  // Compute what the PID would produce:
        let u_current = current_rate.ln();
        let u_pid = u_current + kp * error + ki * state.integral + kd * 0.0;
        let pid_rate = u_pid.exp();

        let original_integral = state.integral;
        pid_back_calculate_log(&mut state, pid_rate, current_rate, kp, ki, kd, error, 0.5);

        // Integral should barely change since selected_rate ≈ what PID wanted
        assert!(
            (state.integral - original_integral).abs() < 0.01,
            "integral should be nearly unchanged for bumpless transfer: {} vs {}",
            state.integral,
            original_integral
        );
    }

    #[test]
    fn back_calculate_uses_nonzero_derivative() {
        // Regression test: the derivative term must be nonzero when
        // last_error differs from the current error. A prior bug
        // overwrote last_error before computing derivative, making it
        // always 0.
        let mut state = PidState::new(1.0);
        state.last_error = 5.0; // previous error
        state.integral = 0.0;
        let error: f64 = 20.0; // current error → derivative = 20 - 5 = 15
        let kd: f64 = 0.1;

        // Run back-calculate with kd > 0 and nonzero derivative.
        // If derivative is incorrectly 0, the required_integral will differ.
        let mut state_with_deriv = state.clone();
        pid_back_calculate_log(
            &mut state_with_deriv,
            1200.0,
            1000.0,
            0.5,
            0.1,
            kd,
            error,
            0.5,
        );

        // Run again with kd = 0 (derivative doesn't matter)
        let mut state_without_deriv = state.clone();
        pid_back_calculate_log(
            &mut state_without_deriv,
            1200.0,
            1000.0,
            0.5,
            0.1,
            0.0,
            error,
            0.5,
        );

        // If derivative is correctly nonzero, the integrals should differ
        // because the kd*derivative term changes required_integral.
        assert_ne!(
            state_with_deriv.integral, state_without_deriv.integral,
            "derivative term should affect back-calculation when kd > 0 and derivative != 0"
        );
    }
}
