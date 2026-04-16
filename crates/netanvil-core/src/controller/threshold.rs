//! Threshold-based constraint (AIMD control law).
//!
//! Replaces ramp.rs's eval_timeout, eval_inflight, eval_latency,
//! eval_error_rate, and eval_external with a single configurable type.

use netanvil_types::{ExternalConstraintConfig, MissingSignalBehavior, SignalDirection};

use super::constraint::{Constraint, ConstraintIntent, ConstraintOutput, EvalContext};
use super::constraints::{
    apply_persistence, graduated_action, update_streak, ConstraintAction, ConstraintClass,
    ConstraintState, SEVERITY_BYPASS_PERSISTENCE,
};
use super::smoothing::Smoother;

// ---------------------------------------------------------------------------
// Metric extraction
// ---------------------------------------------------------------------------

/// How to extract a scalar metric from MetricsSummary.
pub enum MetricExtractor {
    /// `timeout_count / total_requests` — requires total_requests > 0.
    TimeoutFraction,
    /// `in_flight_drops / window_duration / current_rps` — requires current_rps > 0.
    InFlightDropFraction,
    /// `latency_p99_ns` converted to milliseconds.
    LatencyP99Ms,
    /// `latency_p90_ns` converted to milliseconds.
    LatencyP90Ms,
    /// `latency_p50_ns` converted to milliseconds.
    LatencyP50Ms,
    /// `error_rate * 100` (fraction → percentage).
    ErrorRatePct,
    /// Named external signal lookup from `external_signals`.
    External {
        signal_name: String,
        direction: SignalDirection,
    },
}

impl MetricExtractor {
    /// Extract the raw metric value. Returns None if the metric is unavailable
    /// (e.g., no requests, or external signal not present).
    fn extract(&self, ctx: &EvalContext) -> Option<f64> {
        match self {
            Self::TimeoutFraction => {
                if ctx.summary.total_requests == 0 {
                    return None;
                }
                Some(ctx.summary.timeout_count as f64 / ctx.summary.total_requests as f64)
            }
            Self::InFlightDropFraction => {
                if ctx.current_rate <= 0.0 {
                    return None;
                }
                let drop_rate = ctx.summary.in_flight_drops as f64
                    / ctx.summary.window_duration.as_secs_f64().max(0.001);
                Some(drop_rate / ctx.current_rate)
            }
            Self::LatencyP99Ms => Some(ctx.summary.latency_p99_ns as f64 / 1_000_000.0),
            Self::LatencyP90Ms => Some(ctx.summary.latency_p90_ns as f64 / 1_000_000.0),
            Self::LatencyP50Ms => Some(ctx.summary.latency_p50_ns as f64 / 1_000_000.0),
            Self::ErrorRatePct => Some(ctx.summary.error_rate * 100.0),
            Self::External { signal_name, .. } => ctx
                .summary
                .external_signals
                .iter()
                .find(|(name, _)| name == signal_name)
                .map(|(_, v)| *v),
        }
    }
}

// ---------------------------------------------------------------------------
// Severity mapping
// ---------------------------------------------------------------------------

/// How severity maps to a ConstraintAction.
pub enum SeverityMapping {
    /// Graduated: <0.7 Allow, [0.7,1.0) Hold, [1.0,1.5) Gentle, [1.5,2.0) Moderate, ≥2.0 Hard.
    Graduated,
    /// Timeout-style: two hard-coded thresholds derived from max_error_rate.
    TimeoutDirect { max_error_rate_pct: f64 },
    /// InFlight-style: >1% Gentle, >0.1% Hold, else Allow.
    InFlightDirect,
    /// ErrorRate-style: < max Allow, < 1.5× max Gentle, ≥ 1.5× max Hard.
    ErrorRateDirect { max_error_rate_pct: f64 },
}

impl SeverityMapping {
    fn map_to_action(&self, severity: f64, raw_value: f64) -> ConstraintAction {
        match self {
            Self::Graduated => graduated_action(severity),
            Self::TimeoutDirect { max_error_rate_pct } => {
                let hard = (max_error_rate_pct / 100.0 * 2.5).max(0.05);
                let soft = (max_error_rate_pct / 100.0 * 0.5).max(0.01);
                if raw_value >= hard {
                    ConstraintAction::HardBackoff
                } else if raw_value >= soft {
                    ConstraintAction::GentleBackoff
                } else {
                    ConstraintAction::AllowIncrease
                }
            }
            Self::InFlightDirect => {
                // raw_value is drop_fraction
                if raw_value > 0.01 {
                    ConstraintAction::GentleBackoff
                } else if raw_value > 0.001 {
                    ConstraintAction::Hold
                } else {
                    ConstraintAction::AllowIncrease
                }
            }
            Self::ErrorRateDirect { max_error_rate_pct } => {
                // raw_value is error_pct
                if raw_value < *max_error_rate_pct {
                    ConstraintAction::AllowIncrease
                } else if raw_value < max_error_rate_pct * 1.5 {
                    ConstraintAction::GentleBackoff
                } else {
                    ConstraintAction::HardBackoff
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ExternalSignalState
// ---------------------------------------------------------------------------

/// Staleness and missing-signal tracking for external signal constraints.
struct ExternalSignalState {
    config: ExternalConstraintConfig,
    ticks_since_seen: u32,
    missing_streak: u32,
}

// ---------------------------------------------------------------------------
// BackoffSchedule — single source of truth for backoff factors
// ---------------------------------------------------------------------------

/// Backoff factors for the three graduated severity levels.
/// Defined once, referenced by all ThresholdConstraint instances.
#[derive(Debug, Clone, Copy)]
pub struct BackoffSchedule {
    pub gentle: f64,
    pub moderate: f64,
    pub hard: f64,
}

impl Default for BackoffSchedule {
    fn default() -> Self {
        Self {
            gentle: 0.90,
            moderate: 0.75,
            hard: 0.50,
        }
    }
}

// ---------------------------------------------------------------------------
// ThresholdConfig — all parameters, named defaults
// ---------------------------------------------------------------------------

/// Configuration for a ThresholdConstraint. All values nameable, all overrideable.
///
/// Use named defaults (`ThresholdConfig::latency_defaults()`, etc.) to get a
/// config pre-filled with the appropriate profile, then override specific fields.
pub struct ThresholdConfig {
    pub id: String,
    pub class: ConstraintClass,
    pub metric: MetricExtractor,
    pub smoother: Smoother,
    pub threshold: f64,
    pub severity_mapping: SeverityMapping,
    pub persistence: u32,
    pub recovery_seed: f64,
    pub backoff: BackoffSchedule,
    pub external: Option<ExternalConstraintConfig>,
    /// Self-caused severity cap. When set, constraint returns Hold and freezes
    /// streak when ticks_since_increase <= 2 and severity < this threshold.
    pub self_caused_cap: Option<f64>,
    /// Whether higher metric values are worse (latency, error rate) or lower
    /// values are worse (throughput, available capacity). Determines severity
    /// computation direction.
    pub direction: SignalDirection,
    /// If set, derive threshold from warmup baseline: `baseline × multiplier`.
    /// Used by latency constraints with `ThresholdSource::FromBaseline`.
    pub baseline_multiplier: Option<f64>,
    /// Floor applied to the observed baseline before multiplying. Prevents
    /// sub-millisecond baselines from producing thresholds tighter than the
    /// OS jitter envelope (default: 4.0ms for 100Hz tick kernels).
    pub baseline_floor_ms: Option<f64>,
}

impl ThresholdConfig {
    /// Defaults for a timeout constraint.
    pub fn timeout_defaults(max_error_rate_pct: f64) -> Self {
        let soft_threshold = (max_error_rate_pct / 100.0 * 0.5).max(0.01);
        Self {
            id: "timeout".into(),
            class: ConstraintClass::Catastrophic,
            metric: MetricExtractor::TimeoutFraction,
            smoother: Smoother::ema(0.5),
            threshold: soft_threshold,
            severity_mapping: SeverityMapping::TimeoutDirect { max_error_rate_pct },
            persistence: 1,
            recovery_seed: 20.0,
            backoff: BackoffSchedule::default(),
            external: None,
            self_caused_cap: None,
            direction: SignalDirection::HigherIsWorse,
            baseline_multiplier: None,
            baseline_floor_ms: None,
        }
    }

    /// Defaults for an in-flight drop constraint (matches old ramp eval_inflight).
    pub fn inflight_defaults() -> Self {
        Self {
            id: "inflight".into(),
            class: ConstraintClass::Catastrophic,
            metric: MetricExtractor::InFlightDropFraction,
            smoother: Smoother::ema(0.5),
            threshold: 0.01,
            severity_mapping: SeverityMapping::InFlightDirect,
            persistence: 1,
            recovery_seed: 20.0,
            backoff: BackoffSchedule::default(),
            external: None,
            self_caused_cap: None,
            direction: SignalDirection::HigherIsWorse,
            baseline_multiplier: None,
            baseline_floor_ms: None,
        }
    }

    /// Defaults for a latency constraint (matches old ramp eval_latency).
    /// Threshold is typically 0.0 at construction and set after warmup.
    pub fn latency_defaults(smoothing_window: usize) -> Self {
        Self {
            id: "latency".into(),
            class: ConstraintClass::OperatingPoint,
            metric: MetricExtractor::LatencyP99Ms,
            smoother: Smoother::median(smoothing_window.max(3)),
            threshold: 0.0,
            severity_mapping: SeverityMapping::Graduated,
            persistence: 2,
            recovery_seed: 5.0,
            backoff: BackoffSchedule::default(),
            external: None,
            self_caused_cap: Some(1.5),
            direction: SignalDirection::HigherIsWorse,
            baseline_multiplier: None,
            baseline_floor_ms: None,
        }
    }

    /// Defaults for an error-rate constraint (matches old ramp eval_error_rate).
    pub fn error_rate_defaults(max_error_rate_pct: f64) -> Self {
        Self {
            id: "error_rate".into(),
            class: ConstraintClass::OperatingPoint,
            metric: MetricExtractor::ErrorRatePct,
            smoother: Smoother::ema(0.3),
            threshold: max_error_rate_pct,
            severity_mapping: SeverityMapping::ErrorRateDirect { max_error_rate_pct },
            persistence: 1,
            recovery_seed: 5.0,
            backoff: BackoffSchedule::default(),
            external: None,
            self_caused_cap: None,
            direction: SignalDirection::HigherIsWorse,
            baseline_multiplier: None,
            baseline_floor_ms: None,
        }
    }

    /// Defaults for an external signal constraint (matches old ramp eval_external).
    pub fn external_defaults(config: ExternalConstraintConfig) -> Self {
        let threshold = config.threshold;
        let persistence = config.persistence;
        let direction = config.direction;
        let signal_name = config.signal_name.clone();
        Self {
            id: signal_name.clone(),
            class: ConstraintClass::OperatingPoint,
            metric: MetricExtractor::External {
                signal_name,
                direction,
            },
            smoother: Smoother::ema(0.3),
            threshold,
            severity_mapping: SeverityMapping::Graduated,
            persistence,
            recovery_seed: 10.0,
            backoff: BackoffSchedule::default(),
            external: Some(config),
            self_caused_cap: None,
            direction,
            baseline_multiplier: None,
            baseline_floor_ms: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ThresholdConstraint
// ---------------------------------------------------------------------------

/// A threshold-based constraint implementing the Constraint trait.
///
/// Uses AIMD control law: maps severity to a discrete action via configurable
/// severity mapping, converts action to desired_rate via backoff factors.
pub struct ThresholdConstraint {
    // Identity
    id: String,
    class: ConstraintClass,

    // Stage 1: signal interpretation
    metric: MetricExtractor,
    smoother: Smoother,
    threshold: f64,
    severity_mapping: SeverityMapping,
    direction: SignalDirection,
    state: ConstraintState,

    // Stage 2: control law parameters
    backoff: BackoffSchedule,

    // Optional features
    external_state: Option<ExternalSignalState>,
    self_caused_cap: Option<f64>,

    // Cached from last evaluate() for observability
    last_severity: f64,
    last_smoothed_value: f64,

    // Baseline-derived threshold: if set, on_warmup_complete sets
    // threshold = max(baseline, floor) × multiplier.
    baseline_multiplier: Option<f64>,
    baseline_floor_ms: Option<f64>,
}

impl ThresholdConstraint {
    /// Construct from a `ThresholdConfig`.
    pub fn new(config: ThresholdConfig) -> Self {
        let external_state = config.external.map(|ext_cfg| ExternalSignalState {
            config: ext_cfg,
            ticks_since_seen: 0,
            missing_streak: 0,
        });
        Self {
            id: config.id,
            class: config.class,
            metric: config.metric,
            smoother: config.smoother,
            threshold: config.threshold,
            severity_mapping: config.severity_mapping,
            direction: config.direction,
            state: ConstraintState::new(config.persistence, config.recovery_seed),
            backoff: config.backoff,
            external_state,
            self_caused_cap: config.self_caused_cap,
            last_severity: 0.0,
            last_smoothed_value: 0.0,
            baseline_multiplier: config.baseline_multiplier,
            baseline_floor_ms: config.baseline_floor_ms,
        }
    }

    /// Convenience: build a timeout constraint from defaults.
    pub fn timeout(max_error_rate_pct: f64) -> Self {
        Self::new(ThresholdConfig::timeout_defaults(max_error_rate_pct))
    }

    /// Convenience: build an in-flight drop constraint from defaults.
    pub fn inflight() -> Self {
        Self::new(ThresholdConfig::inflight_defaults())
    }

    /// Convenience: build a latency constraint with an absolute threshold.
    pub fn latency(target_p99_ms: f64, smoothing_window: usize) -> Self {
        let mut config = ThresholdConfig::latency_defaults(smoothing_window);
        config.threshold = target_p99_ms;
        Self::new(config)
    }

    /// Convenience: build a latency constraint that derives threshold from
    /// warmup baseline × multiplier.
    pub fn latency_from_baseline(multiplier: f64, smoothing_window: usize) -> Self {
        let mut config = ThresholdConfig::latency_defaults(smoothing_window);
        config.threshold = 0.0;
        config.baseline_multiplier = Some(multiplier);
        Self::new(config)
    }

    /// Convenience: build an error-rate constraint from defaults.
    pub fn error_rate(max_error_rate_pct: f64) -> Self {
        Self::new(ThresholdConfig::error_rate_defaults(max_error_rate_pct))
    }

    /// Convenience: build an external signal constraint from defaults.
    pub fn external(config: ExternalConstraintConfig) -> Self {
        Self::new(ThresholdConfig::external_defaults(config))
    }

    /// Set the threshold value (e.g., after warmup determines baseline).
    pub fn set_threshold(&mut self, threshold: f64) {
        self.threshold = threshold;
    }

    /// Set the max_error_rate used by TimeoutDirect and ErrorRateDirect.
    pub fn set_max_error_rate(&mut self, rate: f64) {
        match &mut self.severity_mapping {
            SeverityMapping::TimeoutDirect {
                max_error_rate_pct, ..
            } => {
                *max_error_rate_pct = rate;
                // Re-derive timeout soft threshold.
                self.threshold = (rate / 100.0 * 0.5).max(0.01);
            }
            SeverityMapping::ErrorRateDirect {
                max_error_rate_pct, ..
            } => {
                *max_error_rate_pct = rate;
                self.threshold = rate;
            }
            _ => {}
        }
    }

    /// Compute severity from a smoothed value and the threshold.
    ///
    /// Uses the constraint's `direction` field so both built-in and external
    /// metrics are handled correctly. Higher-is-worse: `smoothed / threshold`.
    /// Lower-is-worse: `threshold / smoothed` (e.g., throughput, available capacity).
    fn compute_severity(&self, smoothed: f64) -> f64 {
        match self.direction {
            SignalDirection::HigherIsWorse => {
                if self.threshold > 0.0 {
                    smoothed / self.threshold
                } else {
                    0.0
                }
            }
            SignalDirection::LowerIsWorse => {
                if smoothed > f64::EPSILON {
                    self.threshold / smoothed
                } else {
                    SEVERITY_BYPASS_PERSISTENCE + 1.0
                }
            }
        }
    }

    /// Convert a ConstraintAction to a ConstraintIntent using backoff factors.
    fn action_to_intent(&self, action: ConstraintAction, current_rate: f64) -> ConstraintIntent {
        match action {
            ConstraintAction::AllowIncrease => ConstraintIntent::NoObjection,
            ConstraintAction::Hold => ConstraintIntent::Hold(current_rate),
            backoff => ConstraintIntent::DesireRate(
                current_rate
                    * backoff.backoff_factor(
                        self.backoff.gentle,
                        self.backoff.moderate,
                        self.backoff.hard,
                    ),
            ),
        }
    }
}

impl Constraint for ThresholdConstraint {
    fn evaluate(&mut self, ctx: &EvalContext) -> Option<ConstraintOutput> {
        // --- External signal: handle missing/staleness first ---
        if let Some(ref mut ext) = self.external_state {
            let signal_present = ctx
                .summary
                .external_signals
                .iter()
                .any(|(name, _)| name == &ext.config.signal_name);

            if !signal_present {
                ext.ticks_since_seen += 1;
                if ext.ticks_since_seen > ext.config.stale_after_ticks {
                    ext.missing_streak += 1;
                }

                let missing_confirmed = ext.missing_streak >= self.state.persistence;
                if !missing_confirmed {
                    return None;
                }

                let (severity, action) = match ext.config.on_missing {
                    MissingSignalBehavior::Ignore => return None,
                    MissingSignalBehavior::Hold => (0.85, ConstraintAction::Hold),
                    MissingSignalBehavior::Backoff => (
                        SEVERITY_BYPASS_PERSISTENCE + 1.0,
                        ConstraintAction::HardBackoff,
                    ),
                };

                update_streak(&mut self.state.violation_streak, severity);
                let action = apply_persistence(
                    action,
                    self.state.violation_streak,
                    self.state.persistence,
                    severity,
                );

                self.last_severity = severity;
                return Some(ConstraintOutput {
                    intent: self.action_to_intent(action, ctx.current_rate),
                    severity,
                });
            } else {
                // Signal present — reset staleness.
                ext.ticks_since_seen = 0;
                ext.missing_streak = 0;
            }
        }

        // --- Extract and smooth the metric ---
        let raw_value = self.metric.extract(ctx)?;
        let smoothed = self.smoother.smooth(raw_value);
        self.last_smoothed_value = smoothed;

        // --- Compute severity ---
        let severity = self.compute_severity(smoothed);
        self.last_severity = severity;

        // --- Map severity to action ---
        let raw_action = self.severity_mapping.map_to_action(severity, raw_value);

        // --- Self-caused detection ---
        // Matches the old ramp's behavior: when the signal is NOT self-caused
        // (ticks_since_increase > 2) and severity is mild (below cap), the
        // spike is likely external noise — cap at Hold and freeze the violation
        // streak to prevent persistence accumulation from external noise.
        //
        // When self-caused IS true (ticks_since_increase <= 2), let the action
        // through unchanged — a severity spike after our own rate increase is
        // a real capacity signal, not noise.
        if let Some(cap) = self.self_caused_cap {
            if ctx.ticks_since_increase > 2 && severity < cap {
                let capped_action = raw_action.min(ConstraintAction::Hold);
                // Don't advance streak — this was external noise.
                let intent = self.action_to_intent(capped_action, ctx.current_rate);
                self.last_severity = severity;
                return Some(ConstraintOutput { intent, severity });
            }
        }

        update_streak(&mut self.state.violation_streak, severity);
        let action = apply_persistence(
            raw_action,
            self.state.violation_streak,
            self.state.persistence,
            severity,
        );

        // --- Convert to intent ---
        Some(ConstraintOutput {
            intent: self.action_to_intent(action, ctx.current_rate),
            severity,
        })
    }

    fn post_select(&mut self, _selected_rate: f64, _current_rate: f64, is_binding: bool) {
        if is_binding {
            // Start recovery tracking for binding constraint.
            self.state.start_recovery_tracking();
        }
        // Always advance recovery (whether binding or not).
        self.state.advance_recovery(self.last_severity);
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn class(&self) -> ConstraintClass {
        self.class
    }

    fn recovery_estimate_ticks(&self) -> Option<f64> {
        Some(self.state.cooldown_estimate())
    }

    fn constraint_state(&self) -> Vec<(String, f64)> {
        vec![
            (format!("{}_severity", self.id), self.last_severity),
            (format!("{}_smoothed", self.id), self.last_smoothed_value),
            (
                format!("{}_streak", self.id),
                self.state.violation_streak as f64,
            ),
            (
                format!("{}_recovery_ema", self.id),
                self.state.recovery_ema_ticks,
            ),
        ]
    }

    fn apply_update(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match action {
            "set_threshold" => {
                let threshold = params
                    .get("threshold")
                    .and_then(|v| v.as_f64())
                    .ok_or("set_threshold requires 'threshold': f64")?;
                let old = self.threshold;
                self.set_threshold(threshold);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_threshold": old,
                    "new_threshold": threshold,
                }))
            }
            "set_max_error_rate" => {
                let rate = params
                    .get("max_error_rate")
                    .and_then(|v| v.as_f64())
                    .ok_or("set_max_error_rate requires 'max_error_rate': f64")?;
                self.set_max_error_rate(rate);
                Ok(serde_json::json!({"ok": true, "new_max_error_rate": rate}))
            }
            other => Err(format!("unknown action '{other}' for threshold constraint")),
        }
    }

    fn on_warmup_complete(&mut self, baseline: &super::constraint::WarmupBaseline) {
        // Only constraints with a baseline_multiplier derive their threshold.
        let multiplier = match self.baseline_multiplier {
            Some(m) => m,
            None => return,
        };

        if baseline.sample_count == 0 || baseline.baseline_p99_ms <= 0.0 {
            tracing::warn!(
                constraint = %self.id,
                sample_count = baseline.sample_count,
                baseline_p99_ms = format!("{:.2}", baseline.baseline_p99_ms),
                "warmup produced no usable baseline — keeping current threshold"
            );
            return;
        }

        // Apply the baseline floor: use max(observed, floor) so that
        // sub-millisecond services get a threshold above the OS jitter envelope.
        let floor = self.baseline_floor_ms.unwrap_or(0.0);
        let effective_baseline = baseline.baseline_p99_ms.max(floor);
        let target = effective_baseline * multiplier;

        if effective_baseline > baseline.baseline_p99_ms {
            tracing::info!(
                constraint = %self.id,
                observed_baseline_ms = format!("{:.2}", baseline.baseline_p99_ms),
                floor_ms = format!("{:.2}", floor),
                effective_baseline_ms = format!("{:.2}", effective_baseline),
                multiplier,
                target_ms = format!("{:.2}", target),
                "baseline floored (observed < floor) — threshold set from floor"
            );
        } else {
            tracing::info!(
                constraint = %self.id,
                baseline_p99_ms = format!("{:.2}", baseline.baseline_p99_ms),
                multiplier,
                target_ms = format!("{:.2}", target),
                "threshold set from warmup baseline"
            );
        }

        self.set_threshold(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use netanvil_types::MetricsSummary;

    fn make_ctx(
        p99_ms: f64,
        ticks_since_increase: u32,
    ) -> (Box<MetricsSummary>, EvalContext<'static>) {
        let summary = Box::new(MetricsSummary {
            total_requests: 100,
            latency_p99_ns: (p99_ms * 1_000_000.0) as u64,
            latency_p90_ns: (p99_ms * 0.9 * 1_000_000.0) as u64,
            latency_p50_ns: (p99_ms * 0.5 * 1_000_000.0) as u64,
            ..Default::default()
        });
        let summary_ref: &'static MetricsSummary = Box::leak(summary);
        let ctx = EvalContext {
            summary: summary_ref,
            current_rate: 1000.0,
            ticks_since_increase,
            min_rps: 10.0,
            max_rps: 50000.0,
        };
        // Leak is fine in tests — process exits.
        (Box::new(MetricsSummary::default()), ctx)
    }

    /// Regression: self-caused detection must NOT suppress AllowIncrease
    /// after a rate increase. (Fixed 2026-04-10: logic was inverted,
    /// causing 4× slower clean ramp.)
    #[test]
    fn self_caused_does_not_suppress_allow_increase() {
        let mut c = ThresholdConstraint::latency(15.0, 3);

        // Warm up median smoother (needs 3 samples).
        for _ in 0..3 {
            let (_, ctx) = make_ctx(5.0, 10);
            c.evaluate(&ctx);
        }

        // Right after a rate increase (ticks_since_increase=0),
        // p99=5ms, target=15ms → severity=0.33 → AllowIncrease.
        // Self-caused gate must NOT suppress this to Hold.
        let (_, ctx) = make_ctx(5.0, 0);
        let output = c.evaluate(&ctx).expect("should produce output");
        assert!(
            matches!(output.intent, ConstraintIntent::NoObjection),
            "mild severity after rate increase should be NoObjection, got {:?}",
            output.intent
        );
    }

    /// The complement: when NOT self-caused, mild backoff IS capped at Hold.
    /// (Noise rejection: external noise shouldn't cause backoff, only hold.)
    #[test]
    fn not_self_caused_mild_backoff_capped_at_hold() {
        let mut c = ThresholdConstraint::latency(15.0, 3);

        // Feed values in the GentleBackoff band (severity 1.0-1.5) to the
        // median smoother. Need 3 samples for the median to reflect them.
        // 18ms / 15ms = 1.2 → GentleBackoff.
        for _ in 0..3 {
            let (_, ctx) = make_ctx(18.0, 10);
            c.evaluate(&ctx);
        }

        // NOT self-caused (ticks_since_increase=10), severity=1.2 < cap 1.5.
        // Without the self-caused cap, this would be GentleBackoff.
        // With the cap (!self_caused && mild), it should be capped at Hold.
        let (_, ctx) = make_ctx(18.0, 10);
        let output = c.evaluate(&ctx).expect("should produce output");
        assert!(
            matches!(output.intent, ConstraintIntent::Hold(_)),
            "mild backoff without recent increase should be capped at Hold, got {:?}",
            output.intent
        );
    }

    /// Severe spikes are never suppressed, regardless of self-caused status.
    #[test]
    fn severe_spike_not_suppressed_regardless_of_self_caused() {
        let mut c = ThresholdConstraint::latency(15.0, 3);

        // Feed severe values through the median smoother (need 3 samples).
        // 80ms / 15ms = 5.3 → well above cap 1.5.
        for _ in 0..3 {
            let (_, ctx) = make_ctx(80.0, 10);
            c.evaluate(&ctx);
        }

        // Severe spike right after increase (self-caused).
        // Severity 5.3 > cap 1.5, so self-caused gate doesn't apply.
        let (_, ctx) = make_ctx(80.0, 0);
        let output = c.evaluate(&ctx).expect("should produce output");
        assert!(
            matches!(output.intent, ConstraintIntent::DesireRate(_)),
            "severe spike should produce backoff, got {:?}",
            output.intent
        );
    }
}
