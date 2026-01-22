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
}

impl ThresholdConfig {
    /// Defaults for a timeout constraint (matches old ramp eval_timeout).
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
    state: ConstraintState,

    // Stage 2: control law parameters
    backoff: BackoffSchedule,

    // Optional features
    external_state: Option<ExternalSignalState>,
    self_caused_cap: Option<f64>,

    // Cached from last evaluate() for observability
    last_severity: f64,
    last_smoothed_value: f64,
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
            state: ConstraintState::new(config.persistence, config.recovery_seed),
            backoff: config.backoff,
            external_state,
            self_caused_cap: config.self_caused_cap,
            last_severity: 0.0,
            last_smoothed_value: 0.0,
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

    /// Convenience: build a latency constraint from defaults.
    pub fn latency(target_p99_ms: f64, smoothing_window: usize) -> Self {
        let mut config = ThresholdConfig::latency_defaults(smoothing_window);
        config.threshold = target_p99_ms;
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
    fn compute_severity(&self, smoothed: f64) -> f64 {
        match &self.metric {
            MetricExtractor::External { direction, .. } => match direction {
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
            },
            _ => {
                if self.threshold > 0.0 {
                    smoothed / self.threshold
                } else {
                    0.0
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

        // --- Self-caused detection: freeze streak and cap at Hold ---
        // When we recently increased rate (within 2 ticks) and the severity
        // spike is mild (below cap), it's likely external noise during our
        // rate change — cap at Hold and do NOT advance the violation streak
        // (prevents persistence accumulation during suppression window).
        if let Some(cap) = self.self_caused_cap {
            if ctx.ticks_since_increase <= 2 && severity < cap {
                return Some(ConstraintOutput {
                    intent: ConstraintIntent::Hold(ctx.current_rate),
                    severity,
                });
            }
        }

        // --- Map severity to action ---
        let raw_action = self.severity_mapping.map_to_action(severity, raw_value);

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

    fn apply_update(&mut self, params: &serde_json::Value) -> Result<serde_json::Value, String> {
        if let Some(threshold) = params.get("threshold").and_then(|v| v.as_f64()) {
            let old = self.threshold;
            self.set_threshold(threshold);
            Ok(serde_json::json!({
                "ok": true,
                "previous_threshold": old,
                "new_threshold": threshold,
            }))
        } else if let Some(rate) = params.get("max_error_rate").and_then(|v| v.as_f64()) {
            self.set_max_error_rate(rate);
            Ok(serde_json::json!({"ok": true, "new_max_error_rate": rate}))
        } else {
            Err("expected 'threshold' or 'max_error_rate' field".into())
        }
    }

    fn on_warmup_complete(
        &mut self,
        baseline: &super::constraint::WarmupBaseline,
        latency_multiplier: f64,
    ) {
        // Only latency constraints derive their threshold from warmup baseline.
        if matches!(
            self.metric,
            MetricExtractor::LatencyP99Ms
                | MetricExtractor::LatencyP90Ms
                | MetricExtractor::LatencyP50Ms
        ) {
            let target = baseline.baseline_p99_ms * latency_multiplier;
            self.set_threshold(target);
            tracing::info!(
                constraint = %self.id,
                baseline_p99_ms = format!("{:.2}", baseline.baseline_p99_ms),
                multiplier = latency_multiplier,
                target_ms = format!("{:.2}", target),
                "threshold set from warmup baseline"
            );
        }
    }
}
