//! PID-based constraint for override control.
//!
//! Operates in log-rate space for scale-invariance: `u = ln(rate)`,
//! `desired_rate = exp(u_new)`. Back-calculation tracking for non-binding
//! state ensures bumpless transfer on constraint switches.

use netanvil_types::TargetMetric;

use super::autotune::{ExplorationManager, ExplorationPhase, CONSERVATIVE_GAINS};
use super::constraint::{Constraint, ConstraintIntent, ConstraintOutput, EvalContext};
use super::constraints::ConstraintClass;
use super::pid_math::{
    extract_metric, gain_schedule, pid_back_calculate_log, GainMultipliers, PidState,
};
use super::smoothing::Smoother;

/// exp(709) overflows f64; cap at 700 to stay comfortably in range.
const PID_LOG_RATE_MAX: f64 = 700.0;

/// Default back-calculation tracking gain.
const DEFAULT_TRACKING_GAIN: f64 = 0.5;

/// Saturation tolerance for anti-windup comparison. Log-rate arithmetic
/// accumulates rounding error through exp(), so 0.1% is too tight. 1%
/// errs toward detecting saturation (suppresses integral when unsure),
/// which is safer than missing it (windup).
const SATURATION_TOLERANCE: f64 = 1.01;

/// Maximum log-rate contribution from the integral term. ki * integral_max
/// should stay within this bound (~150× rate multiplier). Prevents the
/// integrator from producing absurd rates in log-space.
const MAX_INTEGRAL_LOG_CONTRIBUTION: f64 = 5.0;

/// Hard ceiling on integral magnitude for very small ki values.
const INTEGRAL_HARD_LIMIT: f64 = 1000.0;

// ---------------------------------------------------------------------------
// Metric direction
// ---------------------------------------------------------------------------

/// Whether higher values of a metric are worse (latency, error rate) or
/// better (throughput). Used for one-sided severity: severity > 0 only when
/// the metric is in the "bad" direction relative to target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricDirection {
    /// Higher values are worse (latency, error rate, queue depth).
    /// Severity = max(0, (smoothed - target) / target).
    HigherIsWorse,
    /// Lower values are worse (throughput, available capacity).
    /// Severity = max(0, (target - smoothed) / target).
    LowerIsWorse,
}

impl MetricDirection {
    /// Infer direction from a TargetMetric. Built-in metrics have canonical
    /// directions; external metrics default to HigherIsWorse.
    pub fn from_metric(metric: &TargetMetric) -> Self {
        match metric {
            TargetMetric::ThroughputSend | TargetMetric::ThroughputRecv => {
                MetricDirection::LowerIsWorse
            }
            _ => MetricDirection::HigherIsWorse,
        }
    }
}

// ---------------------------------------------------------------------------
// Eval cache
// ---------------------------------------------------------------------------

/// Cached state from evaluate(), consumed by post_select() within the same tick.
/// Gains are cached here so effective_gains() is computed once per tick.
#[derive(Debug, Clone)]
struct EvalCache {
    error: f64,
    desired_rate: f64,
    kp: f64,
    ki: f64,
    kd: f64,
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Builder-style configuration for PidConstraint.
pub struct PidConstraintConfig {
    pub id: String,
    pub class: ConstraintClass,
    pub metric: TargetMetric,
    pub direction: MetricDirection,
    pub target: f64,
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
    pub smoother: Smoother,
    pub use_scheduling: bool,
    pub tracking_gain: f64,
}

impl PidConstraintConfig {
    /// Sensible defaults for a setpoint-tracking PID constraint.
    /// Direction is inferred from the metric type.
    pub fn new(id: String, metric: TargetMetric, target: f64) -> Self {
        let direction = MetricDirection::from_metric(&metric);
        Self {
            id,
            class: ConstraintClass::OperatingPoint,
            metric,
            direction,
            target,
            kp: 0.0,
            ki: 0.0,
            kd: 0.0,
            smoother: Smoother::none(),
            use_scheduling: false,
            tracking_gain: DEFAULT_TRACKING_GAIN,
        }
    }
}

// ---------------------------------------------------------------------------
// PidConstraint
// ---------------------------------------------------------------------------

/// A PID-based constraint implementing the Constraint trait.
///
/// Uses log-rate-space PID: `u_new = u_current + kp·error + ki·integral + kd·derivative`,
/// `desired_rate = exp(u_new)`. This preserves the scale-invariance of the old
/// multiplicative formulation while being invertible for back-calculation.
///
/// **Contract:** the arbiter must call `evaluate` exactly once per tick,
/// followed by exactly one `post_select`. Calling `evaluate` twice without
/// an intervening `post_select` is a bug (debug-asserted).
pub struct PidConstraint {
    // Identity
    id: String,
    class: ConstraintClass,

    // Stage 1: signal interpretation
    metric: TargetMetric,
    direction: MetricDirection,
    smoother: Smoother,
    target: f64,

    // Stage 2: PID control law
    kp: f64,
    ki: f64,
    kd: f64,
    state: PidState,
    use_scheduling: bool,

    // Back-calculation tracking
    tracking_gain: f64,

    // Samples fed to smoother (for min_samples gate)
    samples_seen: u32,

    // Whether derivative computation is valid. False at construction and
    // after any integrator reset (NaN/overflow/critical-region recovery).
    // Set true after first successful evaluate. Prevents derivative kick.
    derivative_valid: bool,

    // Eval cache: set in evaluate(), consumed in post_select(), then cleared.
    eval_cache: Option<EvalCache>,

    // Long-lived observability state (survives post_select's cache consumption).
    last_severity: f64,
    last_desired_rate: f64,

    // Diagnostic: rate-limit overflow/NaN warnings (re-armed on successful evaluate)
    overflow_warned: bool,
    nan_warned: bool,

    // Auto-tuning: if true, this constraint is passive (evaluate returns None)
    // until on_exploration_complete delivers computed gains.
    awaiting_exploration: bool,
}

impl PidConstraint {
    /// Create a new PID constraint from a config.
    pub fn from_config(cfg: PidConstraintConfig) -> Self {
        Self {
            id: cfg.id,
            class: cfg.class,
            metric: cfg.metric,
            direction: cfg.direction,
            smoother: cfg.smoother,
            target: cfg.target,
            kp: cfg.kp,
            ki: cfg.ki,
            kd: cfg.kd,
            state: PidState::default(),
            use_scheduling: cfg.use_scheduling,
            tracking_gain: cfg.tracking_gain,
            samples_seen: 0,
            derivative_valid: false,
            eval_cache: None,
            last_severity: 0.0,
            last_desired_rate: 0.0,
            overflow_warned: false,
            nan_warned: false,
            awaiting_exploration: false,
        }
    }

    /// Convenience: create from positional args (for tests and builder).
    pub fn new(
        id: String,
        metric: TargetMetric,
        target: f64,
        kp: f64,
        ki: f64,
        kd: f64,
        smoother: Smoother,
        use_scheduling: bool,
    ) -> Self {
        let direction = MetricDirection::from_metric(&metric);
        Self::from_config(PidConstraintConfig {
            id,
            class: ConstraintClass::OperatingPoint,
            metric,
            direction,
            target,
            kp,
            ki,
            kd,
            smoother,
            use_scheduling,
            tracking_gain: DEFAULT_TRACKING_GAIN,
        })
    }

    /// Create a PID constraint configured for auto-tuning exploration.
    ///
    /// Uses conservative placeholder gains until `on_exploration_complete()`
    /// delivers computed gains from the `ExplorationManager`. Returns `None`
    /// from `evaluate()` while `awaiting_exploration` is true.
    pub fn auto_tuning(
        id: String,
        metric: TargetMetric,
        target: f64,
        smoother: Smoother,
    ) -> Self {
        let direction = MetricDirection::from_metric(&metric);
        let mut s = Self::from_config(PidConstraintConfig {
            id,
            class: ConstraintClass::OperatingPoint,
            metric,
            direction,
            target,
            kp: CONSERVATIVE_GAINS.kp,
            ki: CONSERVATIVE_GAINS.ki,
            kd: CONSERVATIVE_GAINS.kd,
            smoother,
            use_scheduling: true,
            tracking_gain: DEFAULT_TRACKING_GAIN,
        });
        s.awaiting_exploration = true;
        s
    }

    /// Set the target value. Resets the integrator because the accumulated
    /// error was correct for the old target and is meaningless for the new one.
    pub fn set_target(&mut self, target: f64) {
        self.target = target;
        self.reset_integrator();
    }

    /// Set the PID gains. Scales the integrator to preserve continuity:
    /// `ki_old * I_old == ki_new * I_new`, so the integral's contribution
    /// to the output doesn't jump when gains change.
    pub fn set_gains(&mut self, kp: f64, ki: f64, kd: f64) {
        if self.ki.abs() > 1e-12 && ki.abs() > 1e-12 {
            self.state.integral *= self.ki / ki;
        } else if ki.abs() <= 1e-12 {
            self.state.integral = 0.0;
        }
        self.kp = kp;
        self.ki = ki;
        self.kd = kd;
    }

    /// Set the back-calculation tracking gain.
    pub fn set_tracking_gain(&mut self, tracking_gain: f64) {
        self.tracking_gain = tracking_gain;
    }

    /// Reset the PID integrator. Invalidates derivative for next tick.
    pub fn reset_integrator(&mut self) {
        self.state.integral = 0.0;
        self.derivative_valid = false;
    }

    /// Get effective gains after gain scheduling. Called once per tick in
    /// evaluate(); the result is cached in EvalCache for post_select().
    fn effective_gains(&self, error: f64) -> (f64, f64, f64, Option<GainMultipliers>) {
        if self.use_scheduling {
            let normalized_error = if self.target.abs() > 1e-9 {
                error / self.target
            } else {
                error
            };
            let mult = gain_schedule(normalized_error);
            (
                self.kp * mult.kp_scale,
                self.ki * mult.ki_scale,
                self.kd * mult.kd_scale,
                Some(mult),
            )
        } else {
            (self.kp, self.ki, self.kd, None)
        }
    }

    /// Clamp the integral so that ki * integral stays within a sane log-rate
    /// range. The limit is scaled by the current ki magnitude.
    fn clamp_integral(&mut self, ki: f64) {
        let integral_max = if ki.abs() > 1e-12 {
            (MAX_INTEGRAL_LOG_CONTRIBUTION / ki.abs()).min(INTEGRAL_HARD_LIMIT)
        } else {
            INTEGRAL_HARD_LIMIT
        };
        self.state.integral = self.state.integral.clamp(-integral_max, integral_max);
    }

    /// Compute one-sided severity: positive only when the metric is in the
    /// "bad" direction relative to target. Comparable to threshold severity.
    fn compute_severity(&self, smoothed: f64) -> f64 {
        if self.target.abs() < 1e-9 {
            return 0.0;
        }
        let raw = match self.direction {
            MetricDirection::HigherIsWorse => (smoothed - self.target) / self.target,
            MetricDirection::LowerIsWorse => (self.target - smoothed) / self.target,
        };
        raw.max(0.0)
    }
}

impl Constraint for PidConstraint {
    fn evaluate(&mut self, ctx: &EvalContext) -> Option<ConstraintOutput> {
        // Passive during exploration — gains not yet determined.
        if self.awaiting_exploration {
            return None;
        }

        // Contract: evaluate should not be called twice without post_select.
        debug_assert!(
            self.eval_cache.is_none(),
            "PidConstraint::evaluate called twice without post_select"
        );

        // Feed the smoother BEFORE the min_samples check so it warms up.
        let raw = extract_metric(&self.metric, ctx.summary);
        let smoothed = self.smoother.smooth(raw);
        self.samples_seen = self.samples_seen.saturating_add(1);

        // Gate on smoother readiness.
        if (self.samples_seen as usize) < self.smoother.min_samples() {
            return None;
        }

        // Re-arm anomaly warnings on any successful evaluate.
        self.overflow_warned = false;
        self.nan_warned = false;

        // Compute error and directional severity
        let error = self.target - smoothed;
        let severity = self.compute_severity(smoothed);

        // Stage 2: PID in log-rate space
        let (kp, ki, kd, mult) = self.effective_gains(error);

        // Handle integral reset in critical region
        if let Some(ref m) = mult {
            if m.reset_integral {
                self.state.integral = 0.0;
                self.derivative_valid = false;
            }
        }

        // Derivative: skip when not valid (first sample, post-reset).
        let derivative = if self.derivative_valid {
            error - self.state.last_error
        } else {
            0.0
        };

        let u_current = ctx.current_rate.max(1e-9).ln();
        let u_new = u_current + kp * error + ki * self.state.integral + kd * derivative;

        // Numerical safety — only intercept genuine pathology.
        let desired_rate = if u_new.is_nan() {
            if !self.nan_warned {
                tracing::warn!(
                    constraint = %self.id,
                    kp, ki, kd, error,
                    integral = self.state.integral,
                    "PID forward path produced NaN, resetting integrator"
                );
                self.nan_warned = true;
            }
            self.state.integral = 0.0;
            self.derivative_valid = false;
            ctx.current_rate
        } else if u_new > PID_LOG_RATE_MAX {
            if !self.overflow_warned {
                tracing::warn!(
                    constraint = %self.id,
                    u_new = format!("{:.1}", u_new),
                    integral = self.state.integral,
                    "PID log-rate overflow, resetting integrator"
                );
                self.overflow_warned = true;
            }
            self.state.integral = 0.0;
            self.derivative_valid = false;
            PID_LOG_RATE_MAX.exp()
        } else {
            u_new.exp()
        };

        // Mark derivative valid for next tick (only if this tick was clean).
        if !u_new.is_nan() && u_new <= PID_LOG_RATE_MAX {
            self.derivative_valid = true;
        }

        // Update long-lived observability state.
        self.last_severity = severity;
        self.last_desired_rate = desired_rate;

        self.eval_cache = Some(EvalCache {
            error,
            desired_rate,
            kp,
            ki,
            kd,
        });

        Some(ConstraintOutput {
            intent: ConstraintIntent::DesireRate(desired_rate),
            severity,
        })
    }

    fn post_select(&mut self, selected_rate: f64, current_rate: f64, is_binding: bool) {
        let cache = match self.eval_cache.take() {
            Some(c) => c,
            None => return,
        };

        let error = cache.error;
        let ki = cache.ki;

        if is_binding {
            // Saturation check: did the arbiter clamp our desired rate?
            let clamped_down =
                cache.desired_rate > selected_rate * SATURATION_TOLERANCE && error > 0.0;
            let clamped_up =
                cache.desired_rate * SATURATION_TOLERANCE < selected_rate && error < 0.0;
            let saturated = clamped_down || clamped_up;

            if !saturated {
                self.state.integral += error;
            }
        } else {
            // Non-binding: back-calculation tracking for bumpless transfer.
            pid_back_calculate_log(
                &mut self.state,
                selected_rate,
                current_rate,
                cache.kp,
                ki,
                cache.kd,
                error,
                self.tracking_gain,
            );
        }

        // Integral clamp scaled by ki — applies to both paths.
        self.clamp_integral(ki);

        // Update last_error for derivative computation on next tick.
        self.state.last_error = error;
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn class(&self) -> ConstraintClass {
        self.class
    }

    fn on_rate_override(&mut self, _new_rate: f64) {
        // External rate jump invalidates the derivative — next tick would
        // compute error against the old last_error and the new current_rate,
        // producing a kick. Same mechanism as NaN/overflow recovery.
        self.derivative_valid = false;
        self.eval_cache = None;
    }

    fn requires_exploration(&self) -> bool {
        self.awaiting_exploration
    }

    fn on_exploration_complete(&mut self, manager: &ExplorationManager) {
        if !self.awaiting_exploration {
            return;
        }

        // Find our metric in the manager by TargetMetric value.
        let gains = manager
            .metrics
            .iter()
            .enumerate()
            .find(|(_, m)| m.metric == self.metric)
            .map(|(idx, _)| match &manager.phase {
                ExplorationPhase::Complete => manager.compute_gains_for(idx),
                _ => CONSERVATIVE_GAINS,
            })
            .unwrap_or_else(|| {
                tracing::warn!(
                    constraint = %self.id,
                    metric = ?self.metric,
                    "no matching metric in exploration manager, using conservative gains"
                );
                CONSERVATIVE_GAINS
            });

        tracing::info!(
            constraint = %self.id,
            kp = format!("{:.4}", gains.kp),
            ki = format!("{:.4}", gains.ki),
            kd = format!("{:.4}", gains.kd),
            dead_time_ticks = gains.dead_time_ticks,
            "exploration complete, applying computed gains"
        );

        self.set_gains(gains.kp, gains.ki, gains.kd);
        self.awaiting_exploration = false;
    }

    fn constraint_state(&self) -> Vec<(String, f64)> {
        vec![
            (format!("{}_severity", self.id), self.last_severity),
            (format!("{}_integral", self.id), self.state.integral),
            (format!("{}_smoothed", self.id), self.smoother.current()),
            (format!("{}_target", self.id), self.target),
            (format!("{}_desired_rate", self.id), self.last_desired_rate),
        ]
    }

    fn apply_update(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match action {
            "set_target" => {
                let target = params
                    .get("target")
                    .and_then(|v| v.as_f64())
                    .ok_or("set_target requires 'target': f64")?;
                let old = self.target;
                self.set_target(target);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_target": old,
                    "new_target": target,
                }))
            }
            "set_gains" => {
                let kp = params
                    .get("kp")
                    .and_then(|v| v.as_f64())
                    .ok_or("set_gains requires 'kp': f64")?;
                let ki = params
                    .get("ki")
                    .and_then(|v| v.as_f64())
                    .ok_or("set_gains requires 'ki': f64")?;
                let kd = params
                    .get("kd")
                    .and_then(|v| v.as_f64())
                    .ok_or("set_gains requires 'kd': f64")?;
                self.set_gains(kp, ki, kd);
                Ok(serde_json::json!({
                    "ok": true,
                    "new_gains": { "kp": kp, "ki": ki, "kd": kd },
                }))
            }
            "set_tracking_gain" => {
                let tg = params
                    .get("tracking_gain")
                    .and_then(|v| v.as_f64())
                    .ok_or("set_tracking_gain requires 'tracking_gain': f64")?;
                let old = self.tracking_gain;
                self.set_tracking_gain(tg);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_tracking_gain": old,
                    "new_tracking_gain": tg,
                }))
            }
            "reset_integrator" => {
                self.reset_integrator();
                Ok(serde_json::json!({ "ok": true, "action": "reset_integrator" }))
            }
            other => Err(format!("unknown action '{other}'")),
        }
    }
}
