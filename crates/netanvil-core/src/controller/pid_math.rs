//! Shared PID math: gain scheduling, PID state, compute/update split.
//!
//! This module contains the pure computation and state management used by all
//! PID-family controllers (manual PID, autotuning PID, composite PID).
//! Exploration logic and Cohen-Coon system identification remain in `autotune`.

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
}

impl Default for PidState {
    fn default() -> Self {
        Self {
            integral: 0.0,
            last_error: 0.0,
            ema_value: 0.0,
            ema_alpha: 0.0,
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
        }
    }

    /// Apply EMA smoothing to a raw metric value.
    pub fn smooth(&mut self, raw: f64) -> f64 {
        if self.ema_value == 0.0 && raw != 0.0 {
            self.ema_value = raw;
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
    }
}

/// Input parameters for a single PID step.
pub struct PidStepInput {
    pub current_value: f64,
    pub target_value: f64,
    pub current_rps: f64,
    pub min_rps: f64,
    pub max_rps: f64,
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
}

/// Output of a pure PID computation (no state mutation).
#[derive(Debug, Clone)]
pub struct PidOutput {
    /// The computed target RPS after PID adjustment and clamping.
    pub new_rps: f64,
    /// The raw error (target_value - current_value).
    pub error: f64,
    /// Whether the ±20% adjustment clamp fired in the error's direction.
    pub adj_saturated: bool,
    /// Whether the min/max RPS bounds clamped in the error's direction.
    pub rps_saturated: bool,
    /// Gain multipliers from scheduling (None for fixed gains).
    pub multipliers: Option<GainMultipliers>,
}

/// Pure PID computation: reads state but does not mutate it.
///
/// When `use_scheduling` is true, gain scheduling adjusts the effective gains
/// based on normalized error and may zero the integral term for the computation
/// (critical region). The caller must apply the actual state mutation via
/// [`pid_update_state`].
pub fn pid_compute(input: &PidStepInput, state: &PidState, use_scheduling: bool) -> PidOutput {
    let error = input.target_value - input.current_value;

    let (kp, ki, kd, multipliers, effective_integral) = if use_scheduling {
        let normalized_error = if input.target_value.abs() > 1e-9 {
            error / input.target_value
        } else {
            error
        };
        let mult = gain_schedule(normalized_error);
        // In critical region, the integral is zeroed for this computation.
        let integral = if mult.reset_integral {
            0.0
        } else {
            state.integral
        };
        (
            input.kp * mult.kp_scale,
            input.ki * mult.ki_scale,
            input.kd * mult.kd_scale,
            Some(mult),
            integral,
        )
    } else {
        (input.kp, input.ki, input.kd, None, state.integral)
    };

    let derivative = error - state.last_error;

    let output = kp * error + ki * effective_integral + kd * derivative;
    let adj_raw = output * 0.05;
    let adjustment = adj_raw.clamp(-0.20, 0.20);
    let unclamped_rps = input.current_rps * (1.0 + adjustment);
    let new_rps = unclamped_rps.clamp(input.min_rps, input.max_rps);

    let adj_saturated = (adj_raw > 0.20 && error > 0.0) || (adj_raw < -0.20 && error < 0.0);
    let rps_saturated = (unclamped_rps > input.max_rps && error > 0.0)
        || (unclamped_rps < input.min_rps && error < 0.0);

    PidOutput {
        new_rps,
        error,
        adj_saturated,
        rps_saturated,
        multipliers,
    }
}

/// Apply PID state mutations after a computation.
///
/// Updates `last_error`, applies integral reset (if gain scheduling dictates),
/// and conditionally accumulates the error into the integral (anti-windup:
/// suppressed when either the ±20% adjustment clamp or the RPS bounds are
/// saturated in the error's direction).
pub fn pid_update_state(state: &mut PidState, output: &PidOutput) {
    state.last_error = output.error;

    // Integral reset from gain scheduling (critical region).
    if let Some(ref mult) = output.multipliers {
        if mult.reset_integral {
            state.integral = 0.0;
        }
    }

    // Conditional integration anti-windup.
    if !output.adj_saturated && !output.rps_saturated {
        state.integral += output.error;
        state.integral = state.integral.clamp(-1000.0, 1000.0);
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

/// Compute a single PID step with gain scheduling.
///
/// Convenience wrapper around [`pid_compute`] + [`pid_update_state`].
/// Returns the new RPS value.
pub fn pid_step_with_scheduling(input: &PidStepInput, state: &mut PidState) -> f64 {
    let output = pid_compute(input, state, true);
    pid_update_state(state, &output);
    output.new_rps
}

/// Compute a single PID step with fixed gains (no scheduling).
///
/// Convenience wrapper around [`pid_compute`] + [`pid_update_state`].
/// Returns the new RPS value.
pub fn pid_step_fixed(input: &PidStepInput, state: &mut PidState) -> f64 {
    let output = pid_compute(input, state, false);
    pid_update_state(state, &output);
    output.new_rps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_input(current_value: f64, target_value: f64, current_rps: f64) -> PidStepInput {
        PidStepInput {
            current_value,
            target_value,
            current_rps,
            min_rps: 10.0,
            max_rps: 10000.0,
            kp: 0.5,
            ki: 0.1,
            kd: 0.05,
        }
    }

    #[test]
    fn pid_compute_basic_proportional_response() {
        let state = PidState::new(1.0);
        // Use small gains to avoid saturation: kp=0.02, error=50 → output=1.0,
        // adj_raw=0.05 (well under 0.20 clamp)
        let input = PidStepInput {
            current_value: 50.0,
            target_value: 100.0,
            current_rps: 1000.0,
            min_rps: 10.0,
            max_rps: 10000.0,
            kp: 0.02,
            ki: 0.0,
            kd: 0.0,
        };
        let output = pid_compute(&input, &state, false);
        assert!(
            output.new_rps > 1000.0,
            "positive error should increase rate"
        );
        assert_eq!(output.error, 50.0);
        assert!(!output.adj_saturated);
        assert!(!output.rps_saturated);
        assert!(output.multipliers.is_none(), "fixed gains → no multipliers");
    }

    #[test]
    fn pid_compute_negative_error_decreases_rate() {
        let state = PidState::new(1.0);
        let output = pid_compute(&make_input(150.0, 100.0, 1000.0), &state, false);
        assert!(
            output.new_rps < 1000.0,
            "negative error should decrease rate"
        );
        assert_eq!(output.error, -50.0);
    }

    #[test]
    fn pid_compute_does_not_mutate_state() {
        let state = PidState::new(1.0);
        let original_integral = state.integral;
        let original_last_error = state.last_error;
        let _ = pid_compute(&make_input(50.0, 100.0, 1000.0), &state, false);
        assert_eq!(state.integral, original_integral);
        assert_eq!(state.last_error, original_last_error);
    }

    #[test]
    fn pid_compute_with_scheduling_applies_gain_multipliers() {
        let state = PidState::new(1.0);
        let output = pid_compute(&make_input(50.0, 100.0, 1000.0), &state, true);
        assert!(output.multipliers.is_some());
        // normalized_error = 50/100 = 0.5 → ramp-up region (>0.4) → kp_scale=2.0
        let mult = output.multipliers.unwrap();
        assert_eq!(mult.kp_scale, 2.0);
    }

    #[test]
    fn pid_compute_critical_region_zeros_effective_integral() {
        let mut state = PidState::new(1.0);
        state.integral = 500.0;

        // Use small ki so integral matters but output doesn't saturate at ±20%.
        // current=130, target=100 → error=-30, normalized=-0.3 → critical region
        let input = PidStepInput {
            current_value: 130.0,
            target_value: 100.0,
            current_rps: 1000.0,
            min_rps: 10.0,
            max_rps: 10000.0,
            kp: 0.001,
            ki: 0.001,
            kd: 0.0,
        };

        let output = pid_compute(&input, &state, true);
        assert!(output.multipliers.as_ref().unwrap().reset_integral);
        // state.integral unchanged — pid_compute is pure
        assert_eq!(state.integral, 500.0);

        // Fixed-gains version uses the full integral=500, scheduled version zeros it.
        // With ki=0.001: fixed integral term = 0.001*500 = 0.5, scheduled = 0.
        // This difference survives the ×0.05 scaling and doesn't hit the ±20% clamp.
        let output_fixed = pid_compute(&input, &state, false);
        assert_ne!(
            output.new_rps, output_fixed.new_rps,
            "zeroed integral (scheduled) should produce different rate than full integral (fixed)"
        );
    }

    #[test]
    fn pid_compute_adj_saturation_flag() {
        let state = PidState::new(1.0);
        let input = PidStepInput {
            current_value: 10.0,
            target_value: 10000.0,
            current_rps: 1000.0,
            min_rps: 10.0,
            max_rps: 100000.0,
            kp: 10.0,
            ki: 0.0,
            kd: 0.0,
        };
        let output = pid_compute(&input, &state, false);
        assert!(output.adj_saturated);
    }

    #[test]
    fn pid_compute_rps_saturation_flag() {
        let state = PidState::new(1.0);
        let input = PidStepInput {
            current_value: 50.0,
            target_value: 100.0,
            current_rps: 9900.0,
            min_rps: 10.0,
            max_rps: 10000.0,
            kp: 0.5,
            ki: 0.0,
            kd: 0.0,
        };
        let output = pid_compute(&input, &state, false);
        assert_eq!(output.new_rps, 10000.0);
        assert!(output.rps_saturated);
    }

    #[test]
    fn pid_update_state_accumulates_integral() {
        let mut state = PidState::new(1.0);
        let output = PidOutput {
            new_rps: 1100.0,
            error: 50.0,
            adj_saturated: false,
            rps_saturated: false,
            multipliers: None,
        };
        pid_update_state(&mut state, &output);
        assert_eq!(state.last_error, 50.0);
        assert_eq!(state.integral, 50.0);
    }

    #[test]
    fn pid_update_state_suppresses_on_adj_saturation() {
        let mut state = PidState::new(1.0);
        let output = PidOutput {
            new_rps: 1200.0,
            error: 50.0,
            adj_saturated: true,
            rps_saturated: false,
            multipliers: None,
        };
        pid_update_state(&mut state, &output);
        assert_eq!(state.last_error, 50.0);
        assert_eq!(state.integral, 0.0);
    }

    #[test]
    fn pid_update_state_suppresses_on_rps_saturation() {
        let mut state = PidState::new(1.0);
        let output = PidOutput {
            new_rps: 10000.0,
            error: 50.0,
            adj_saturated: false,
            rps_saturated: true,
            multipliers: None,
        };
        pid_update_state(&mut state, &output);
        assert_eq!(state.integral, 0.0);
    }

    #[test]
    fn pid_update_state_resets_then_accumulates_in_critical_region() {
        let mut state = PidState::new(1.0);
        state.integral = 500.0;
        let output = PidOutput {
            new_rps: 800.0,
            error: -100.0,
            adj_saturated: false,
            rps_saturated: false,
            multipliers: Some(GainMultipliers {
                kp_scale: 3.0,
                ki_scale: 0.0,
                kd_scale: 2.5,
                reset_integral: true,
            }),
        };
        pid_update_state(&mut state, &output);
        // Reset to 0, then accumulate error: 0 + (-100) = -100
        assert_eq!(state.integral, -100.0);
    }

    #[test]
    fn pid_step_with_scheduling_matches_split_path() {
        let mut state1 = PidState::new(0.3);
        state1.integral = 10.0;
        state1.last_error = 5.0;
        let mut state2 = state1.clone();

        let input = make_input(80.0, 100.0, 500.0);

        let rps1 = pid_step_with_scheduling(&input, &mut state1);

        let output = pid_compute(&input, &state2, true);
        pid_update_state(&mut state2, &output);

        assert_eq!(rps1, output.new_rps);
        assert_eq!(state1.integral, state2.integral);
        assert_eq!(state1.last_error, state2.last_error);
    }

    #[test]
    fn pid_step_fixed_matches_split_path() {
        let mut state1 = PidState::new(1.0);
        state1.integral = 20.0;
        state1.last_error = -3.0;
        let mut state2 = state1.clone();

        let input = make_input(120.0, 100.0, 2000.0);

        let rps1 = pid_step_fixed(&input, &mut state1);

        let output = pid_compute(&input, &state2, false);
        pid_update_state(&mut state2, &output);

        assert_eq!(rps1, output.new_rps);
        assert_eq!(state1.integral, state2.integral);
        assert_eq!(state1.last_error, state2.last_error);
    }

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
