//! Adaptive ramp rate controller using gentle AIMD.
//!
//! Phase 1 (warmup): Runs at a low fixed RPS to learn the baseline p99 latency.
//! Phase 2 (ramp): Uses AIMD (Additive Increase / Multiplicative Decrease) with
//!   graduated backoff, median-smoothed p99, and violation streak tracking to
//!   discover the maximum sustainable rate.
//!
//! Constraints are evaluated through a unified model: each constraint produces
//! a severity value and action. The binding constraint (minimum desired rate)
//! drives the rate decision. `ConstraintClass` determines whether the known-good
//! floor applies (`OperatingPoint`) or is bypassed (`Catastrophic`).
//!
//! A `ProgressiveCeiling` linearly raises `max_rps` from `warmup_rps` to
//! the configured ceiling over `test_duration / 2`, preventing the controller
//! from jumping to extreme rates early in the test.
//!
//! The AIMD approach is structurally the same problem as TCP congestion control:
//! find the edge, tolerate noisy feedback, and report the sustainable rate as the
//! median of the oscillation around equilibrium.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use netanvil_types::{
    ControllerInfo, ControllerType, ExternalConstraintConfig, MetricsSummary, MissingSignalBehavior,
    RateController, RateDecision, SignalDirection,
};

use super::ceiling::ProgressiveCeiling;

// ---------------------------------------------------------------------------
// AIMD parameters — all interpretable and independently tunable.
// ---------------------------------------------------------------------------

/// Multiplicative increase factor per clean tick (10% per tick).
const INCREASE_FACTOR: f64 = 1.10;

/// Graduated backoff factors based on violation severity.
const BACKOFF_GENTLE: f64 = 0.90; // severity 1.0–1.5
const BACKOFF_MODERATE: f64 = 0.75; // severity 1.5–2.0
const BACKOFF_HARD: f64 = 0.50; // severity > 2.0

/// Default number of p99 samples to keep for median smoothing.
const DEFAULT_SMOOTHING_WINDOW: usize = 3;

/// Don't drop below this fraction of the best recent rate.
const KNOWN_GOOD_FLOOR_FRACTION: f64 = 0.80;

/// How long to remember the best known-good rate.
const KNOWN_GOOD_WINDOW: Duration = Duration::from_secs(60);

/// Minimum cooldown (in ticks) after a backoff before allowing any rate
/// increase.
const COOLDOWN_MIN_TICKS: u32 = 2;

/// Multiplier applied to the observed recovery time (in ticks) to compute
/// cooldown.
const COOLDOWN_RECOVERY_MULTIPLIER: f64 = 1.5;

/// EMA smoothing factor for recovery time estimation.
const RECOVERY_EMA_ALPHA: f64 = 0.3;

/// When approaching the last known failure rate, switch from multiplicative
/// increase to additive increase (congestion avoidance).
const CONGESTION_AVOIDANCE_THRESHOLD: f64 = 0.85;

/// Additive increase per tick in congestion avoidance mode, as a fraction
/// of the failure rate.
const ADDITIVE_INCREASE_FRACTION: f64 = 0.01;

// ---------------------------------------------------------------------------
// Unified constraint system
// ---------------------------------------------------------------------------

/// Severity above which persistence is bypassed (immediate backoff).
const SEVERITY_BYPASS_PERSISTENCE: f64 = 3.0;

/// Minimum ticks after backoff before recovery can be declared.
const RECOVERY_MIN_TICKS: u32 = 3;

/// Minimum recovery samples before EMA is trusted over seed.
const RECOVERY_MIN_SAMPLES: u32 = 3;

/// Severity threshold below which recovery is declared.
const RECOVERY_SEVERITY_THRESHOLD: f64 = 0.9;

/// Actions a constraint can request, ordered from least to most severe.
/// `Ord` is derived with variants in ascending severity order so that
/// `action.min(Hold)` caps severity at Hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ConstraintAction {
    AllowIncrease,
    Hold,
    GentleBackoff,
    ModerateBackoff,
    HardBackoff,
}

impl ConstraintAction {
    fn backoff_factor(self) -> f64 {
        match self {
            Self::GentleBackoff => BACKOFF_GENTLE,
            Self::ModerateBackoff => BACKOFF_MODERATE,
            Self::HardBackoff => BACKOFF_HARD,
            _ => 1.0,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::AllowIncrease => "increase",
            Self::Hold => "hold",
            Self::GentleBackoff => "backoff_gentle",
            Self::ModerateBackoff => "backoff_moderate",
            Self::HardBackoff => "backoff_hard",
        }
    }

    fn is_backoff(self) -> bool {
        matches!(
            self,
            Self::GentleBackoff | Self::ModerateBackoff | Self::HardBackoff
        )
    }
}

/// Whether a constraint bypasses or respects the known-good floor on backoff.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ConstraintClass {
    /// Floor applies on backoff (latency, error rate, external).
    OperatingPoint,
    /// Floor is BYPASSED on backoff (timeout, in-flight drops).
    Catastrophic,
}

/// Identifies a constraint type.
#[derive(Debug, Clone, PartialEq)]
enum ConstraintId {
    Timeout,
    InFlightDrop,
    Latency,
    ErrorRate,
    External(String),
}

impl ConstraintId {
    fn label(&self) -> &str {
        match self {
            Self::Timeout => "timeout",
            Self::InFlightDrop => "inflight_drop",
            Self::Latency => "latency",
            Self::ErrorRate => "error_rate",
            Self::External(name) => name.as_str(),
        }
    }
}

/// Result of evaluating a single constraint.
struct ConstraintEvaluation {
    id: ConstraintId,
    class: ConstraintClass,
    severity: f64,
    action: ConstraintAction,
}

/// Per-constraint state tracking.
struct ConstraintState {
    /// Consecutive ticks where severity >= 1.0.
    violation_streak: u32,
    /// Ticks of violation required before backoff is confirmed.
    persistence: u32,
    /// Ticks since this signal was last seen (external constraints only).
    ticks_since_seen: u32,
    /// Consecutive ticks where the signal is stale/missing (external only).
    missing_streak: u32,
    /// EMA of observed recovery time in ticks.
    recovery_ema_ticks: f64,
    /// Number of recovery observations. EMA not trusted until >= RECOVERY_MIN_SAMPLES.
    recovery_samples: u32,
    /// Fallback recovery estimate used until EMA earns trust.
    initial_seed: f64,
    /// Ticks since this constraint last triggered backoff (for measuring recovery).
    /// `None` when not tracking a recovery.
    ticks_since_backoff: Option<u32>,
}

impl ConstraintState {
    fn new(persistence: u32, initial_seed: f64) -> Self {
        Self {
            violation_streak: 0,
            persistence,
            ticks_since_seen: 0,
            missing_streak: 0,
            recovery_ema_ticks: initial_seed,
            recovery_samples: 0,
            initial_seed,
            ticks_since_backoff: None,
        }
    }

    /// Cooldown estimate: trust EMA only with enough samples, otherwise
    /// use the larger of EMA and initial seed.
    fn cooldown_estimate(&self) -> f64 {
        if self.recovery_samples >= RECOVERY_MIN_SAMPLES {
            self.recovery_ema_ticks
        } else {
            self.recovery_ema_ticks.max(self.initial_seed)
        }
    }
}

/// Maps continuous severity to a graduated action.
/// Used by Latency and External constraints.
///
/// Thresholds: `<0.7` AllowIncrease, `[0.7,1.0)` Hold, `[1.0,1.5)` Gentle,
/// `[1.5,2.0)` Moderate, `≥2.0` Hard.
fn graduated_action(severity: f64) -> ConstraintAction {
    if severity < 0.7 {
        ConstraintAction::AllowIncrease
    } else if severity < 1.0 {
        ConstraintAction::Hold
    } else if severity < 1.5 {
        ConstraintAction::GentleBackoff
    } else if severity < 2.0 {
        ConstraintAction::ModerateBackoff
    } else {
        ConstraintAction::HardBackoff
    }
}

/// Updates violation streak with hysteresis.
///
/// Streak increments on violation (severity >= 1.0), resets only in the
/// AllowIncrease zone (severity < 0.7), and is maintained in the Hold
/// band `[0.7, 1.0)`. This prevents rapid flapping at the boundary.
fn update_streak(streak: &mut u32, severity: f64) {
    if severity >= 1.0 {
        *streak += 1;
    } else if severity < 0.7 {
        *streak = 0;
    }
    // [0.7, 1.0): maintained (no increment, no reset)
}

/// Persistence gate: backoff actions are downgraded to Hold when the
/// streak hasn't met the persistence requirement and severity doesn't
/// bypass it. AllowIncrease and Hold pass through ungated.
fn apply_persistence(
    action: ConstraintAction,
    streak: u32,
    persistence: u32,
    severity: f64,
) -> ConstraintAction {
    if !action.is_backoff() {
        return action;
    }
    if streak >= persistence || severity > SEVERITY_BYPASS_PERSISTENCE {
        action
    } else {
        ConstraintAction::Hold
    }
}

// ---------------------------------------------------------------------------
// Constraint evaluation
// ---------------------------------------------------------------------------

/// Context passed to constraint evaluation.
struct EvalContext<'a> {
    summary: &'a MetricsSummary,
    current_rps: f64,
    self_caused: bool,
    max_error_rate_pct: f64,
    target_p99_ms: f64,
}

/// A constraint that evaluates metrics and produces an action.
enum Constraint {
    /// Timeout-based backoff. Catastrophic, persistence=1, direct action.
    Timeout { state: ConstraintState },
    /// In-flight drop backoff. Catastrophic, persistence=1, direct action.
    InFlightDrop { state: ConstraintState },
    /// Latency-based backoff. OperatingPoint, persistence=2, graduated_action + self_caused.
    Latency {
        p99_window: VecDeque<f64>,
        smoothing_window: usize,
        last_smoothed_p99: f64,
        state: ConstraintState,
    },
    /// Error-rate backoff. OperatingPoint, persistence=1, direct action.
    ErrorRate { state: ConstraintState },
    /// External signal constraint. OperatingPoint, configurable persistence, graduated_action.
    External {
        config: ExternalConstraintConfig,
        state: ConstraintState,
    },
}

impl Constraint {
    fn id(&self) -> ConstraintId {
        match self {
            Self::Timeout { .. } => ConstraintId::Timeout,
            Self::InFlightDrop { .. } => ConstraintId::InFlightDrop,
            Self::Latency { .. } => ConstraintId::Latency,
            Self::ErrorRate { .. } => ConstraintId::ErrorRate,
            Self::External { config, .. } => ConstraintId::External(config.signal_name.clone()),
        }
    }

    fn state(&self) -> &ConstraintState {
        match self {
            Self::Timeout { state, .. }
            | Self::InFlightDrop { state, .. }
            | Self::Latency { state, .. }
            | Self::ErrorRate { state, .. }
            | Self::External { state, .. } => state,
        }
    }

    fn state_mut(&mut self) -> &mut ConstraintState {
        match self {
            Self::Timeout { state, .. }
            | Self::InFlightDrop { state, .. }
            | Self::Latency { state, .. }
            | Self::ErrorRate { state, .. }
            | Self::External { state, .. } => state,
        }
    }

    /// Advance recovery tracking for this constraint.
    /// Call each tick with the constraint's current severity.
    fn advance_recovery(&mut self, severity: f64) {
        let state = self.state_mut();
        if let Some(ref mut ticks) = state.ticks_since_backoff {
            *ticks += 1;
            // Recovery declared when enough ticks have passed AND severity
            // is below the recovery threshold.
            if *ticks >= RECOVERY_MIN_TICKS && severity < RECOVERY_SEVERITY_THRESHOLD {
                let recovery_ticks = *ticks as f64;
                state.recovery_samples += 1;
                state.recovery_ema_ticks = RECOVERY_EMA_ALPHA * recovery_ticks
                    + (1.0 - RECOVERY_EMA_ALPHA) * state.recovery_ema_ticks;
                state.ticks_since_backoff = None;
            }
        }
    }

    /// Start or restart recovery tracking. If already tracking (re-backoff
    /// before recovery completed), discard the in-progress measurement.
    fn start_recovery_tracking(&mut self) {
        self.state_mut().ticks_since_backoff = Some(0);
    }

    fn evaluate(&mut self, ctx: &EvalContext) -> Option<ConstraintEvaluation> {
        match self {
            Self::Timeout { state } => Self::eval_timeout(state, ctx),
            Self::InFlightDrop { state } => Self::eval_inflight(state, ctx),
            Self::Latency {
                p99_window,
                smoothing_window,
                last_smoothed_p99,
                state,
            } => Self::eval_latency(p99_window, *smoothing_window, last_smoothed_p99, state, ctx),
            Self::ErrorRate { state } => Self::eval_error_rate(state, ctx),
            Self::External { config, state } => Self::eval_external(config, state, ctx),
        }
    }

    // -- Timeout: Catastrophic, persistence=1, direct action mapping --
    //
    // Thresholds derived from max_error_rate:
    //   soft = (max_error_rate/100 × 0.5).max(0.01)
    //   hard = (max_error_rate/100 × 2.5).max(0.05)

    fn eval_timeout(
        state: &mut ConstraintState,
        ctx: &EvalContext,
    ) -> Option<ConstraintEvaluation> {
        if ctx.summary.total_requests == 0 {
            return None;
        }

        let timeout_fraction =
            ctx.summary.timeout_count as f64 / ctx.summary.total_requests as f64;
        let hard_threshold = (ctx.max_error_rate_pct / 100.0 * 2.5).max(0.05);
        let soft_threshold = (ctx.max_error_rate_pct / 100.0 * 0.5).max(0.01);

        // Severity normalized to soft_threshold = 1.0 (for logging).
        let severity = if soft_threshold > 0.0 {
            timeout_fraction / soft_threshold
        } else {
            0.0
        };

        // Direct action mapping (not graduated).
        let raw_action = if timeout_fraction >= hard_threshold {
            ConstraintAction::HardBackoff
        } else if timeout_fraction >= soft_threshold {
            ConstraintAction::GentleBackoff
        } else {
            ConstraintAction::AllowIncrease
        };

        update_streak(&mut state.violation_streak, severity);
        let action =
            apply_persistence(raw_action, state.violation_streak, state.persistence, severity);

        Some(ConstraintEvaluation {
            id: ConstraintId::Timeout,
            class: ConstraintClass::Catastrophic,
            severity,
            action,
        })
    }

    // -- InFlightDrop: Catastrophic, persistence=1, direct action mapping --
    //
    // Thresholds: >1% drops → GentleBackoff, >0.1% → Hold, else → Allow.

    fn eval_inflight(
        state: &mut ConstraintState,
        ctx: &EvalContext,
    ) -> Option<ConstraintEvaluation> {
        if ctx.current_rps <= 0.0 {
            return None;
        }

        let drop_rate = ctx.summary.in_flight_drops as f64
            / ctx.summary.window_duration.as_secs_f64().max(0.001);
        let drop_fraction = drop_rate / ctx.current_rps;

        // Severity normalized to 1% threshold (for logging).
        let severity = drop_fraction / 0.01;

        // Direct action mapping.
        let raw_action = if drop_fraction > 0.01 {
            ConstraintAction::GentleBackoff // >1% drops: real saturation
        } else if drop_fraction > 0.001 {
            ConstraintAction::Hold // 0.1–1% drops: at the edge
        } else {
            ConstraintAction::AllowIncrease // <0.1% drops: noise
        };

        update_streak(&mut state.violation_streak, severity);
        let action =
            apply_persistence(raw_action, state.violation_streak, state.persistence, severity);

        Some(ConstraintEvaluation {
            id: ConstraintId::InFlightDrop,
            class: ConstraintClass::Catastrophic,
            severity,
            action,
        })
    }

    // -- Latency: OperatingPoint, persistence=2, graduated_action + self_caused --
    //
    // Severity = smoothed_p99 / target_p99 (the latency ratio).
    // The self_caused override caps at Hold for external mild noise
    // (not self-caused AND severity < 1.5), applied AFTER persistence gate.

    fn eval_latency(
        p99_window: &mut VecDeque<f64>,
        smoothing_window: usize,
        last_smoothed_p99: &mut f64,
        state: &mut ConstraintState,
        ctx: &EvalContext,
    ) -> Option<ConstraintEvaluation> {
        // Update p99 smoothing window.
        let raw_p99 = ctx.summary.latency_p99_ns as f64 / 1_000_000.0;
        let window_size = if smoothing_window > 0 {
            smoothing_window
        } else {
            DEFAULT_SMOOTHING_WINDOW
        };
        p99_window.push_back(raw_p99);
        while p99_window.len() > window_size {
            p99_window.pop_front();
        }

        let smoothed_p99 = if p99_window.is_empty() {
            0.0
        } else {
            let mut sorted: Vec<f64> = p99_window.iter().copied().collect();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            sorted[sorted.len() / 2]
        };
        *last_smoothed_p99 = smoothed_p99;

        let severity = if ctx.target_p99_ms > 0.0 {
            smoothed_p99 / ctx.target_p99_ms
        } else {
            0.0
        };

        // Graduated action (continuous severity → graduated response).
        let raw_action = graduated_action(severity);

        update_streak(&mut state.violation_streak, severity);
        let action =
            apply_persistence(raw_action, state.violation_streak, state.persistence, severity);

        // self_caused override: caps at Hold for external mild noise.
        // Applied AFTER persistence gate; only softens, never strengthens.
        let action = if !ctx.self_caused && severity < 1.5 {
            action.min(ConstraintAction::Hold)
        } else {
            action
        };

        Some(ConstraintEvaluation {
            id: ConstraintId::Latency,
            class: ConstraintClass::OperatingPoint,
            severity,
            action,
        })
    }

    // -- ErrorRate: OperatingPoint, persistence=1, direct action mapping --
    //
    // Thresholds: < max → Allow, < 1.5× max → Gentle, ≥ 1.5× max → Hard.

    fn eval_error_rate(
        state: &mut ConstraintState,
        ctx: &EvalContext,
    ) -> Option<ConstraintEvaluation> {
        let error_pct = ctx.summary.error_rate * 100.0;

        // Severity normalized to max_error_rate = 1.0.
        let severity = if ctx.max_error_rate_pct > 0.0 {
            error_pct / ctx.max_error_rate_pct
        } else {
            0.0
        };

        // Direct action mapping.
        let raw_action = if error_pct < ctx.max_error_rate_pct {
            ConstraintAction::AllowIncrease
        } else if error_pct < ctx.max_error_rate_pct * 1.5 {
            ConstraintAction::GentleBackoff
        } else {
            ConstraintAction::HardBackoff
        };

        update_streak(&mut state.violation_streak, severity);
        let action =
            apply_persistence(raw_action, state.violation_streak, state.persistence, severity);

        Some(ConstraintEvaluation {
            id: ConstraintId::ErrorRate,
            class: ConstraintClass::OperatingPoint,
            severity,
            action,
        })
    }

    // -- External: OperatingPoint, configurable persistence, graduated_action --
    //
    // Direction-aware severity:
    //   HigherIsWorse: severity = value / threshold
    //   LowerIsWorse:  severity = threshold / value.max(ε)
    //
    // Staleness: if signal not seen for stale_after_ticks, missing_streak
    // increments. When missing_streak >= persistence, on_missing applies:
    //   Ignore → AllowIncrease (severity 0)
    //   Hold   → Hold (severity 0.85)
    //   Backoff → HardBackoff (severity 3.0, bypasses persistence)

    fn eval_external(
        config: &ExternalConstraintConfig,
        state: &mut ConstraintState,
        ctx: &EvalContext,
    ) -> Option<ConstraintEvaluation> {
        // Look up the signal value in MetricsSummary::external_signals.
        let signal_value = ctx
            .summary
            .external_signals
            .iter()
            .find(|(name, _)| *name == config.signal_name)
            .map(|(_, v)| *v);

        let (severity, raw_action) = if let Some(value) = signal_value {
            // Signal present — reset staleness tracking.
            state.ticks_since_seen = 0;
            state.missing_streak = 0;

            // Direction-aware severity.
            let severity = match config.direction {
                SignalDirection::HigherIsWorse => {
                    if config.threshold > 0.0 {
                        value / config.threshold
                    } else {
                        0.0
                    }
                }
                SignalDirection::LowerIsWorse => {
                    if value > f64::EPSILON {
                        config.threshold / value
                    } else {
                        // Value at/near zero with LowerIsWorse → worst case.
                        SEVERITY_BYPASS_PERSISTENCE + 1.0
                    }
                }
            };

            (severity, graduated_action(severity))
        } else {
            // Signal missing — advance staleness.
            state.ticks_since_seen += 1;
            if state.ticks_since_seen > config.stale_after_ticks {
                state.missing_streak += 1;
            }

            // Apply on_missing only when missing_streak >= persistence.
            let missing_confirmed = state.missing_streak >= state.persistence;

            if !missing_confirmed {
                // Not yet confirmed missing — no constraint.
                return None;
            }

            match config.on_missing {
                MissingSignalBehavior::Ignore => return None,
                MissingSignalBehavior::Hold => (0.85, ConstraintAction::Hold),
                MissingSignalBehavior::Backoff => {
                    (SEVERITY_BYPASS_PERSISTENCE + 1.0, ConstraintAction::HardBackoff)
                }
            }
        };

        update_streak(&mut state.violation_streak, severity);
        let action =
            apply_persistence(raw_action, state.violation_streak, state.persistence, severity);

        Some(ConstraintEvaluation {
            id: ConstraintId::External(config.signal_name.clone()),
            class: ConstraintClass::OperatingPoint,
            severity,
            action,
        })
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the ramp controller.
#[derive(Debug, Clone)]
pub struct RampConfig {
    pub warmup_rps: f64,
    pub warmup_duration: Duration,
    pub latency_multiplier: f64,
    pub max_error_rate: f64,
    pub min_rps: f64,
    pub max_rps: f64,
    pub control_interval: Duration,
    pub test_duration: Duration,
    /// Number of p99 samples for median smoothing (default: 3).
    pub smoothing_window: usize,
    /// When true, freeze rate decisions if achieved/target > 2.0.
    pub enable_ratio_freeze: bool,
    /// External signal constraints. Empty = no external constraints (default).
    pub external_constraints: Vec<netanvil_types::ExternalConstraintConfig>,
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

pub struct RampRateController {
    config: RampConfig,
    state: RampState,
    current_rps: f64,

    // -- Warmup --
    warmup_start: Instant,
    warmup_p99_samples: Vec<f64>,

    // -- Learned from warmup --
    baseline_p99_ms: f64,
    target_p99_ms: f64,

    // -- Unified constraints --
    constraints: Vec<Constraint>,

    // -- AIMD shared state --
    /// Best rate seen in the recent window (known-good floor).
    max_recent_rate: f64,
    /// When the max_recent_rate was last updated.
    max_recent_rate_time: Instant,
    /// Last tick's smoothed p99 (copied from Latency constraint for logging).
    last_smoothed_p99: f64,
    /// Ticks since the last rate increase (for self-caused detection).
    ticks_since_increase: u32,
    /// Total ticks spent in the Ramping state.
    ramping_ticks: u32,

    // -- Progressive ceiling --
    ceiling: ProgressiveCeiling,
    last_time_ceiling: f64,

    // -- Post-backoff cooldown --
    cooldown_remaining: u32,

    // -- Congestion avoidance --
    last_failure_rate: f64,
    backoff_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RampState {
    Warmup,
    Ramping,
}

impl RampRateController {
    pub fn new(config: RampConfig) -> Self {
        let current_rps = config.warmup_rps;
        let smoothing = if config.smoothing_window > 0 {
            config.smoothing_window
        } else {
            DEFAULT_SMOOTHING_WINDOW
        };
        let ceiling = ProgressiveCeiling::new(
            config.warmup_rps,
            config.max_rps,
            config.test_duration / 2,
        );

        let mut constraints = vec![
            Constraint::Timeout {
                state: ConstraintState::new(1, 20.0), // persistence=1, seed=20 ticks
            },
            Constraint::InFlightDrop {
                state: ConstraintState::new(1, 20.0), // persistence=1, seed=20 ticks
            },
            Constraint::Latency {
                p99_window: VecDeque::with_capacity(smoothing + 1),
                smoothing_window: smoothing,
                last_smoothed_p99: 0.0,
                state: ConstraintState::new(2, 5.0), // persistence=2, seed=5 ticks
            },
            Constraint::ErrorRate {
                state: ConstraintState::new(1, 5.0), // persistence=1, seed=5 ticks
            },
        ];

        // Add external signal constraints from config.
        for ext in &config.external_constraints {
            constraints.push(Constraint::External {
                config: ext.clone(),
                state: ConstraintState::new(ext.persistence, 10.0), // seed=10 ticks
            });
        }

        Self {
            warmup_start: Instant::now(),
            warmup_p99_samples: Vec::with_capacity(64),
            baseline_p99_ms: 0.0,
            target_p99_ms: 0.0,
            current_rps,
            constraints,
            max_recent_rate: current_rps,
            max_recent_rate_time: Instant::now(),
            last_smoothed_p99: 0.0,
            ticks_since_increase: 0,
            ramping_ticks: 0,
            ceiling,
            last_time_ceiling: 0.0,
            cooldown_remaining: 0,
            last_failure_rate: 0.0,
            backoff_count: 0,
            state: RampState::Warmup,
            config,
        }
    }

    pub fn set_latency_multiplier(&mut self, multiplier: f64) {
        let old = self.config.latency_multiplier;
        self.config.latency_multiplier = multiplier;
        self.target_p99_ms = self.baseline_p99_ms * multiplier;
        tracing::info!(
            old_multiplier = old,
            new_multiplier = multiplier,
            new_target_p99_ms = format!("{:.2}", self.target_p99_ms),
            "ramp: latency multiplier updated"
        );
    }

    pub fn set_max_error_rate(&mut self, rate: f64) {
        let old = self.config.max_error_rate;
        self.config.max_error_rate = rate;
        tracing::info!(old_max_error_rate = old, new_max_error_rate = rate, "ramp: max error rate updated");
    }

    pub fn set_min_rps(&mut self, min_rps: f64) {
        let old = self.config.min_rps;
        self.config.min_rps = min_rps;
        tracing::info!(old_min_rps = old, new_min_rps = min_rps, "ramp: min RPS updated");
    }

    fn transition_to_ramping(&mut self) {
        // Compute baseline p99 from warmup samples (median for robustness).
        self.warmup_p99_samples
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = self.warmup_p99_samples.len();
        self.baseline_p99_ms = if n == 0 {
            1.0
        } else {
            self.warmup_p99_samples[n / 2]
        };

        self.target_p99_ms = self.baseline_p99_ms * self.config.latency_multiplier;

        tracing::info!(
            baseline_p99_ms = format!("{:.2}", self.baseline_p99_ms),
            target_p99_ms = format!("{:.2}", self.target_p99_ms),
            multiplier = self.config.latency_multiplier,
            max_error_rate = self.config.max_error_rate,
            warmup_samples = n,
            "ramp warmup complete, transitioning to AIMD control"
        );

        self.ceiling.start_now();
        self.max_recent_rate_time = Instant::now();
        self.state = RampState::Ramping;
    }

    /// Update the known-good rate floor, expiring stale entries.
    fn update_known_good(&mut self) {
        if self.max_recent_rate_time.elapsed() > KNOWN_GOOD_WINDOW {
            // Stale — reset to current rate.
            self.max_recent_rate = self.current_rps;
            self.max_recent_rate_time = Instant::now();
        }
        if self.current_rps > self.max_recent_rate {
            self.max_recent_rate = self.current_rps;
            self.max_recent_rate_time = Instant::now();
        }
    }

    fn ramping_tick(&mut self, summary: &MetricsSummary) -> RateDecision {
        self.ramping_ticks += 1;

        // 0. Ratio-freeze band-aid: if achieved completions vastly exceed
        // the target rate, the feedback signal is corrupted. Freeze.
        if self.config.enable_ratio_freeze
            && self.current_rps > 0.0
            && self.ramping_ticks > 10
        {
            let ratio = summary.request_rate / self.current_rps;
            if ratio > 2.0 {
                tracing::warn!(
                    achieved_rps = format!("{:.0}", summary.request_rate),
                    target_rps = format!("{:.0}", self.current_rps),
                    ratio = format!("{:.2}", ratio),
                    ramping_ticks = self.ramping_ticks,
                    "rate frozen: achieved >> target, measurements unreliable"
                );
                return RateDecision {
                    target_rps: self.current_rps,
                };
            }
        }

        // 1. Progressive ceiling.
        let time_ceiling = self.ceiling.ceiling();
        self.last_time_ceiling = time_ceiling;

        if let Some(milestone) = self.ceiling.check_milestone() {
            tracing::info!(
                progress_pct = milestone,
                time_ceiling = format!("{:.0}", time_ceiling),
                "ramp ceiling milestone"
            );
        }

        // 2. Self-caused detection: a spike within 2 ticks of a rate
        // increase is likely a capacity signal (we pushed too hard).
        let self_caused = self.ticks_since_increase <= 2;

        // 3. Evaluate all constraints.
        let ctx = EvalContext {
            summary,
            current_rps: self.current_rps,
            self_caused,
            max_error_rate_pct: self.config.max_error_rate,
            target_p99_ms: self.target_p99_ms,
        };
        let mut evaluations = Vec::with_capacity(self.constraints.len());
        for constraint in &mut self.constraints {
            if let Some(eval) = constraint.evaluate(&ctx) {
                evaluations.push(eval);
            }
        }

        // Copy smoothed p99 from Latency constraint for observability.
        for c in &self.constraints {
            if let Constraint::Latency {
                last_smoothed_p99, ..
            } = c
            {
                self.last_smoothed_p99 = *last_smoothed_p99;
            }
        }

        // 4. Advance per-constraint recovery tracking.
        // Each constraint with active recovery tracking gets its timer
        // incremented and checked for recovery (severity < threshold after
        // minimum ticks).
        for constraint in &mut self.constraints {
            let cid = constraint.id();
            let severity = evaluations
                .iter()
                .find(|e| e.id == cid)
                .map(|e| e.severity)
                .unwrap_or(0.0);
            constraint.advance_recovery(severity);
        }

        // 5. Decrement cooldown.
        let in_cooldown = self.cooldown_remaining > 0;
        if in_cooldown {
            self.cooldown_remaining -= 1;
        }

        // 6. Permission gate: determine whether increase is allowed and why not.
        let mut blocked_by: Vec<String> = Vec::new();

        if in_cooldown {
            blocked_by.push(format!("cooldown({})", self.cooldown_remaining + 1));
        }

        for eval in &evaluations {
            if eval.action >= ConstraintAction::Hold && eval.action != ConstraintAction::AllowIncrease {
                let streak = self
                    .constraints
                    .iter()
                    .find(|c| c.id() == eval.id)
                    .map(|c| c.state().violation_streak)
                    .unwrap_or(0);
                blocked_by.push(format!(
                    "{}(sev={:.1},s={})",
                    eval.id.label(),
                    eval.severity,
                    streak,
                ));
            }
        }

        let in_congestion_avoidance = !in_cooldown
            && self.last_failure_rate > 0.0
            && self.current_rps > self.last_failure_rate * CONGESTION_AVOIDANCE_THRESHOLD;

        // Compute increase rate.
        let increase_rate = if in_cooldown {
            self.current_rps // suppress increases during cooldown
        } else if in_congestion_avoidance {
            let additive = self.last_failure_rate * ADDITIVE_INCREASE_FRACTION;
            self.current_rps + additive
        } else {
            self.current_rps * INCREASE_FACTOR
        };

        // 7. Compute desired rate per constraint and find binding.
        let mut binding_idx: Option<usize> = None;
        let mut min_desired = f64::MAX;

        for (i, eval) in evaluations.iter().enumerate() {
            let desired = match eval.action {
                ConstraintAction::AllowIncrease => increase_rate,
                ConstraintAction::Hold => self.current_rps,
                action => self.current_rps * action.backoff_factor(),
            };
            if desired < min_desired {
                min_desired = desired;
                binding_idx = Some(i);
            }
        }

        let mut new_rps = if binding_idx.is_some() {
            min_desired
        } else {
            increase_rate
        };

        let binding_label = binding_idx
            .map(|i| evaluations[i].id.label())
            .unwrap_or("none");
        let binding_class = binding_idx.map(|i| evaluations[i].class);

        // 8. Apply ceiling.
        let ceiling_bound = time_ceiling < new_rps;
        if ceiling_bound {
            new_rps = time_ceiling;
        }
        let binding_label = if ceiling_bound
            && binding_idx.map_or(true, |i| {
                let desired = match evaluations[i].action {
                    ConstraintAction::AllowIncrease => increase_rate,
                    ConstraintAction::Hold => self.current_rps,
                    action => self.current_rps * action.backoff_factor(),
                };
                time_ceiling < desired
            })
        {
            "ceiling"
        } else {
            binding_label
        };

        // 9. Apply known-good floor — bypassed for Catastrophic binding constraints.
        self.update_known_good();
        let floor =
            (self.max_recent_rate * KNOWN_GOOD_FLOOR_FRACTION).max(self.config.min_rps);
        let floored = match binding_class {
            Some(ConstraintClass::Catastrophic) => false,
            _ => {
                let f = new_rps < floor;
                new_rps = new_rps.max(floor);
                f
            }
        };

        // 10. Clamp to bounds.
        new_rps = new_rps.clamp(self.config.min_rps, self.config.max_rps);

        // 11. Backoff tracking: record failure rate, start per-constraint recovery,
        //     engage unified cooldown.
        let is_backoff = new_rps < self.current_rps * 0.98;
        if is_backoff {
            // Update failure rate EMA.
            if self.last_failure_rate == 0.0 {
                self.last_failure_rate = self.current_rps;
            } else {
                self.last_failure_rate = RECOVERY_EMA_ALPHA * self.current_rps
                    + (1.0 - RECOVERY_EMA_ALPHA) * self.last_failure_rate;
            }
            self.backoff_count += 1;

            // Start/restart recovery tracking for constraints that triggered backoff.
            // Collect triggered constraint ids first (can't borrow constraints while iterating evaluations).
            let triggered_ids: Vec<ConstraintId> = evaluations
                .iter()
                .filter(|e| e.action.is_backoff())
                .map(|e| e.id.clone())
                .collect();

            let mut max_cooldown_estimate = 0.0_f64;
            for constraint in &mut self.constraints {
                let cid = constraint.id();
                if triggered_ids.contains(&cid) {
                    constraint.start_recovery_tracking();
                    max_cooldown_estimate =
                        max_cooldown_estimate.max(constraint.state().cooldown_estimate());
                }
            }

            // Cooldown = max(COOLDOWN_MIN, 1.5 × max(cooldown_estimate across triggered)).
            let cooldown_ticks = if max_cooldown_estimate > 0.0 {
                let estimated =
                    (max_cooldown_estimate * COOLDOWN_RECOVERY_MULTIPLIER).ceil() as u32;
                estimated.max(COOLDOWN_MIN_TICKS)
            } else {
                COOLDOWN_MIN_TICKS
            };
            self.cooldown_remaining = cooldown_ticks;

            tracing::info!(
                failure_rate = format!("{:.0}", self.last_failure_rate),
                cooldown_ticks,
                backoff_count = self.backoff_count,
                max_cooldown_estimate = format!("{:.1}", max_cooldown_estimate),
                "ramp: backoff cooldown engaged"
            );
        }

        // 12. Track whether this tick was an increase (for self-caused detection).
        if new_rps > self.current_rps {
            self.ticks_since_increase = 0;
        } else {
            self.ticks_since_increase = self.ticks_since_increase.saturating_add(1);
        }

        // 13. Log unified tick line.
        // Build compact constraint summary: [lat:1.4/hold(s=2), err:0.3/ok, ...]
        let constraints_summary: String = evaluations
            .iter()
            .map(|e| {
                let streak = self
                    .constraints
                    .iter()
                    .find(|c| c.id() == e.id)
                    .map(|c| c.state().violation_streak)
                    .unwrap_or(0);
                let short_id = match &e.id {
                    ConstraintId::Timeout => "to",
                    ConstraintId::InFlightDrop => "ifd",
                    ConstraintId::Latency => "lat",
                    ConstraintId::ErrorRate => "err",
                    ConstraintId::External(name) => name.as_str(),
                };
                format!("{}:{:.1}/{}(s={})", short_id, e.severity, e.action.label(), streak)
            })
            .collect::<Vec<_>>()
            .join(", ");

        let blocked_by_str = if blocked_by.is_empty() {
            "none".to_string()
        } else {
            blocked_by.join(", ")
        };

        let raw_p99 = summary.latency_p99_ns as f64 / 1_000_000.0;

        tracing::info!(
            rate = format!("{:.0}→{:.0}", self.current_rps, new_rps),
            binding = binding_label,
            constraints = constraints_summary,
            blocked_by = blocked_by_str,
            smoothed_p99_ms = format!("{:.2}", self.last_smoothed_p99),
            raw_p99_ms = format!("{:.2}", raw_p99),
            target_p99_ms = format!("{:.2}", self.target_p99_ms),
            self_caused,
            ceiling = format!("{:.0}", time_ceiling),
            floor = format!("{:.0}", floor),
            floored,
            in_cooldown,
            failure_rate = format!("{:.0}", self.last_failure_rate),
            "ramp AIMD tick"
        );

        self.current_rps = new_rps;

        RateDecision {
            target_rps: self.current_rps,
        }
    }
}

impl RateController for RampRateController {
    fn update(&mut self, summary: &MetricsSummary) -> RateDecision {
        match self.state {
            RampState::Warmup => {
                if summary.total_requests > 0 {
                    let p99_ms = summary.latency_p99_ns as f64 / 1_000_000.0;
                    if p99_ms > 0.0 {
                        self.warmup_p99_samples.push(p99_ms);
                        tracing::debug!(
                            p99_ms = format!("{:.2}", p99_ms),
                            samples = self.warmup_p99_samples.len(),
                            elapsed_secs = format!("{:.1}", self.warmup_start.elapsed().as_secs_f64()),
                            remaining_secs = format!("{:.1}", self.config.warmup_duration
                                .saturating_sub(self.warmup_start.elapsed()).as_secs_f64()),
                            "ramp warmup sample"
                        );
                    }
                }

                if self.warmup_start.elapsed() >= self.config.warmup_duration
                    && self.warmup_p99_samples.len() >= 3
                {
                    self.transition_to_ramping();
                    return self.ramping_tick(summary);
                }

                RateDecision {
                    target_rps: self.config.warmup_rps,
                }
            }
            RampState::Ramping => self.ramping_tick(summary),
        }
    }

    fn current_rate(&self) -> f64 {
        self.current_rps
    }

    fn set_rate(&mut self, rps: f64) {
        self.current_rps = rps.clamp(self.config.min_rps, self.config.max_rps);
    }

    fn set_max_rps(&mut self, max_rps: f64) {
        self.config.max_rps = max_rps.max(self.config.min_rps);
        self.ceiling.set_end_value(self.config.max_rps);
    }

    fn controller_state(&self) -> Vec<(&'static str, f64)> {
        if self.state != RampState::Ramping {
            return Vec::new();
        }
        vec![
            ("netanvil_ramp_time_ceiling", self.last_time_ceiling),
            ("netanvil_ramp_smoothed_p99_ms", self.last_smoothed_p99),
            (
                "netanvil_ramp_latency_ratio",
                if self.target_p99_ms > 0.0 {
                    self.last_smoothed_p99 / self.target_p99_ms
                } else {
                    0.0
                },
            ),
            (
                "netanvil_ramp_known_good_floor",
                self.max_recent_rate * KNOWN_GOOD_FLOOR_FRACTION,
            ),
            ("netanvil_ramp_failure_rate", self.last_failure_rate),
            (
                "netanvil_ramp_in_cooldown",
                if self.cooldown_remaining > 0 {
                    1.0
                } else {
                    0.0
                },
            ),
            (
                "netanvil_ramp_recovery_ema_ticks",
                self.constraints
                    .iter()
                    .find(|c| matches!(c, Constraint::Latency { .. }))
                    .map(|c| c.state().recovery_ema_ticks)
                    .unwrap_or(0.0),
            ),
        ]
    }

    fn controller_info(&self) -> ControllerInfo {
        let phase = match self.state {
            RampState::Warmup => "warmup",
            RampState::Ramping => "ramping",
        };

        // Build per-constraint details for the info JSON.
        let constraint_details: Vec<serde_json::Value> = self
            .constraints
            .iter()
            .map(|c| {
                let state = c.state();
                serde_json::json!({
                    "id": c.id().label(),
                    "violation_streak": state.violation_streak,
                    "persistence": state.persistence,
                    "recovery_ema_ticks": state.recovery_ema_ticks,
                    "recovery_samples": state.recovery_samples,
                })
            })
            .collect();

        let latency_streak = self
            .constraints
            .iter()
            .find(|c| matches!(c, Constraint::Latency { .. }))
            .map(|c| c.state().violation_streak)
            .unwrap_or(0);
        let error_streak = self
            .constraints
            .iter()
            .find(|c| matches!(c, Constraint::ErrorRate { .. }))
            .map(|c| c.state().violation_streak)
            .unwrap_or(0);

        ControllerInfo {
            controller_type: ControllerType::Ramp,
            current_rps: self.current_rps,
            editable_actions: vec![
                "set_latency_multiplier".into(),
                "set_max_error_rate".into(),
                "set_max_rps".into(),
                "set_min_rps".into(),
            ],
            params: serde_json::json!({
                "phase": phase,
                "controller": "aimd",
                "baseline_p99_ms": self.baseline_p99_ms,
                "target_p99_ms": self.target_p99_ms,
                "latency_multiplier": self.config.latency_multiplier,
                "max_error_rate": self.config.max_error_rate,
                "min_rps": self.config.min_rps,
                "max_rps": self.config.max_rps,
                "time_ceiling": self.last_time_ceiling,
                "smoothed_p99_ms": self.last_smoothed_p99,
                "latency_violation_streak": latency_streak,
                "error_violation_streak": error_streak,
                "max_recent_rate": self.max_recent_rate,
                "last_failure_rate": self.last_failure_rate,
                "backoff_count": self.backoff_count,
                "in_cooldown": self.cooldown_remaining > 0,
                "constraints": constraint_details,
            }),
        }
    }

    fn apply_update(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match action {
            "set_latency_multiplier" => {
                let multiplier = params
                    .get("multiplier")
                    .and_then(|v| v.as_f64())
                    .ok_or("missing 'multiplier' field")?;
                let old = self.config.latency_multiplier;
                self.set_latency_multiplier(multiplier);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_multiplier": old,
                    "new_multiplier": multiplier,
                    "new_target_p99_ms": self.target_p99_ms,
                }))
            }
            "set_max_error_rate" => {
                let rate = params
                    .get("max_error_rate")
                    .and_then(|v| v.as_f64())
                    .ok_or("missing 'max_error_rate' field")?;
                let old = self.config.max_error_rate;
                self.set_max_error_rate(rate);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_max_error_rate": old,
                    "new_max_error_rate": rate,
                }))
            }
            "set_max_rps" => {
                let max = params
                    .get("max_rps")
                    .and_then(|v| v.as_f64())
                    .ok_or("missing 'max_rps' field")?;
                let old = self.config.max_rps;
                self.set_max_rps(max);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_max_rps": old,
                    "new_max_rps": self.config.max_rps,
                }))
            }
            "set_min_rps" => {
                let min = params
                    .get("min_rps")
                    .and_then(|v| v.as_f64())
                    .ok_or("missing 'min_rps' field")?;
                let old = self.config.min_rps;
                self.set_min_rps(min);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_min_rps": old,
                    "new_min_rps": self.config.min_rps,
                }))
            }
            _ => Err(format!(
                "action '{}' is not valid for controller type 'ramp'",
                action
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graduated_action_boundaries() {
        // AllowIncrease: severity < 0.7
        assert_eq!(graduated_action(0.0), ConstraintAction::AllowIncrease);
        assert_eq!(graduated_action(0.5), ConstraintAction::AllowIncrease);
        assert_eq!(graduated_action(0.69), ConstraintAction::AllowIncrease);

        // Hold: [0.7, 1.0)
        assert_eq!(graduated_action(0.7), ConstraintAction::Hold);
        assert_eq!(graduated_action(0.85), ConstraintAction::Hold);
        assert_eq!(graduated_action(0.99), ConstraintAction::Hold);

        // GentleBackoff: [1.0, 1.5)
        assert_eq!(graduated_action(1.0), ConstraintAction::GentleBackoff);
        assert_eq!(graduated_action(1.33), ConstraintAction::GentleBackoff);
        assert_eq!(graduated_action(1.49), ConstraintAction::GentleBackoff);

        // ModerateBackoff: [1.5, 2.0)
        assert_eq!(graduated_action(1.5), ConstraintAction::ModerateBackoff);
        assert_eq!(graduated_action(1.75), ConstraintAction::ModerateBackoff);
        assert_eq!(graduated_action(1.99), ConstraintAction::ModerateBackoff);

        // HardBackoff: >= 2.0
        assert_eq!(graduated_action(2.0), ConstraintAction::HardBackoff);
        assert_eq!(graduated_action(5.0), ConstraintAction::HardBackoff);
        assert_eq!(graduated_action(100.0), ConstraintAction::HardBackoff);
    }

    #[test]
    fn streak_hysteresis_rules() {
        let mut streak = 0u32;

        // Violation zone: severity >= 1.0 → increment
        update_streak(&mut streak, 1.0);
        assert_eq!(streak, 1);
        update_streak(&mut streak, 1.5);
        assert_eq!(streak, 2);

        // Hold band: [0.7, 1.0) → maintain
        update_streak(&mut streak, 0.85);
        assert_eq!(streak, 2, "hold band should not change streak");

        // Clean zone: < 0.7 → reset
        update_streak(&mut streak, 0.5);
        assert_eq!(streak, 0, "clean zone should reset streak");
    }

    #[test]
    fn persistence_gating() {
        // AllowIncrease passes through
        assert_eq!(
            apply_persistence(ConstraintAction::AllowIncrease, 0, 2, 0.5),
            ConstraintAction::AllowIncrease
        );

        // Hold passes through
        assert_eq!(
            apply_persistence(ConstraintAction::Hold, 0, 2, 0.85),
            ConstraintAction::Hold
        );

        // Backoff with streak < persistence → Hold
        assert_eq!(
            apply_persistence(ConstraintAction::GentleBackoff, 1, 2, 1.2),
            ConstraintAction::Hold
        );

        // Backoff with streak >= persistence → passes
        assert_eq!(
            apply_persistence(ConstraintAction::GentleBackoff, 2, 2, 1.2),
            ConstraintAction::GentleBackoff
        );

        // Backoff with severity > 3.0 bypasses persistence
        assert_eq!(
            apply_persistence(ConstraintAction::HardBackoff, 0, 5, 3.5),
            ConstraintAction::HardBackoff
        );
    }

    #[test]
    fn constraint_action_ordering() {
        // Verify Ord gives ascending severity
        assert!(ConstraintAction::AllowIncrease < ConstraintAction::Hold);
        assert!(ConstraintAction::Hold < ConstraintAction::GentleBackoff);
        assert!(ConstraintAction::GentleBackoff < ConstraintAction::ModerateBackoff);
        assert!(ConstraintAction::ModerateBackoff < ConstraintAction::HardBackoff);

        // min caps at the less severe action
        assert_eq!(
            ConstraintAction::GentleBackoff.min(ConstraintAction::Hold),
            ConstraintAction::Hold
        );
    }

    #[test]
    fn constraint_state_cooldown_estimate() {
        // Fresh state: uses seed
        let state = ConstraintState::new(1, 20.0);
        assert_eq!(state.cooldown_estimate(), 20.0);

        // After some recovery samples (< MIN_SAMPLES): max(ema, seed)
        let mut state = ConstraintState::new(1, 20.0);
        state.recovery_ema_ticks = 10.0;
        state.recovery_samples = 2; // < RECOVERY_MIN_SAMPLES (3)
        assert_eq!(state.cooldown_estimate(), 20.0); // max(10, 20)

        // After enough samples: use EMA directly
        state.recovery_samples = 3;
        assert_eq!(state.cooldown_estimate(), 10.0); // trust EMA
    }
}
