//! Constraint trait and supporting types for override control.
//!
//! A constraint independently interprets a signal from `MetricsSummary` and
//! produces a rate intent. The composition strategy selects among constraints;
//! the arbiter applies policy (ceiling, floor, cooldown) to the composed result.

use netanvil_types::MetricsSummary;

use super::autotune::ExplorationManager;
use super::constraints::ConstraintClass;

// ---------------------------------------------------------------------------
// EvalContext
// ---------------------------------------------------------------------------

/// Shared controller state passed to constraints at evaluation time.
///
/// Constraints read whatever they need from here. This is how cross-cutting
/// concerns (self-caused detection, rate bounds) flow to constraints without
/// the controller reaching in to modify specific constraint outputs.
pub struct EvalContext<'a> {
    pub summary: &'a MetricsSummary,
    pub current_rate: f64,
    pub ticks_since_increase: u32,
    pub min_rps: f64,
    pub max_rps: f64,
}

// ---------------------------------------------------------------------------
// ConstraintIntent
// ---------------------------------------------------------------------------

/// What a constraint wants to happen to the rate.
///
/// Explicit enum rather than a sentinel f64 value. This makes composition
/// strategies correct by construction: `NoObjection` is handled explicitly
/// (MinSelector skips it, WeightedBlend excludes it from the sum, Quorum
/// treats it as abstention) rather than poisoning aggregates via infinity.
#[derive(Debug, Clone, Copy)]
pub enum ConstraintIntent {
    /// "I have no objection — let other constraints or the increase policy decide."
    NoObjection,
    /// "I want the rate held at exactly this value."
    Hold(f64),
    /// "I want the rate set to this value (may be above or below current)."
    DesireRate(f64),
}

// ---------------------------------------------------------------------------
// ConstraintOutput
// ---------------------------------------------------------------------------

/// Output of evaluating a single constraint.
#[derive(Debug, Clone)]
pub struct ConstraintOutput {
    /// What this constraint wants to happen to the rate.
    pub intent: ConstraintIntent,
    /// How stressed this constraint is (0.0 = relaxed, 1.0 = at threshold).
    /// Used for observability and for strategies that weight by severity.
    pub severity: f64,
}

// ---------------------------------------------------------------------------
// Constraint trait
// ---------------------------------------------------------------------------

/// A single constraint in an override control system.
///
/// Constraints independently interpret a signal and produce a rate intent.
/// The composition strategy selects among them; the arbiter applies
/// policy (ceiling, floor, cooldown) to the composed result.
pub trait Constraint {
    /// Evaluate current metrics against this constraint.
    ///
    /// Updates signal-interpretation state (smoothing, streaks, staleness).
    /// Does NOT update control-law state (integrals, recovery starts) —
    /// that happens in `post_select`.
    ///
    /// Returns `None` if the constraint is inactive (e.g., missing external
    /// signal with Ignore policy, or still in exploration phase).
    fn evaluate(&mut self, ctx: &EvalContext) -> Option<ConstraintOutput>;

    /// Called after composition selects a rate.
    ///
    /// - `selected_rate`: the actual rate the arbiter will use
    /// - `is_binding`: whether this constraint was the binding one
    ///
    /// Binding constraints do full state updates (PID: accumulate integral;
    /// threshold: start recovery tracking). Non-binding PID constraints do
    /// back-calculation tracking to prevent integrator windup.
    fn post_select(&mut self, selected_rate: f64, current_rate: f64, is_binding: bool);

    /// Stable identifier for logging, Prometheus labels, and runtime API.
    fn id(&self) -> &str;

    /// Whether this constraint bypasses the known-good floor on backoff.
    fn class(&self) -> ConstraintClass;

    /// Estimated ticks until this constraint recovers after a backoff.
    /// Used by the arbiter to size cooldown duration.
    /// Default: None (no estimate available, arbiter uses its own default).
    fn recovery_estimate_ticks(&self) -> Option<f64> {
        None
    }

    /// Export constraint-specific state for observability.
    /// Keys will be namespaced by the arbiter as `"{constraint_id}_{key}"`.
    fn constraint_state(&self) -> Vec<(String, f64)> {
        Vec::new()
    }

    /// Apply a runtime parameter update. Returns a JSON result.
    ///
    /// `action` is the constraint-level action string (e.g., "set_threshold",
    /// "set_gains"). `params` carries the action's parameters as JSON.
    fn apply_update(
        &mut self,
        _action: &str,
        _params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        Err("no updates supported".into())
    }

    /// Whether this constraint requires an exploration phase to determine its
    /// control parameters (e.g., auto-tuned PID gains). During exploration,
    /// `evaluate()` should return `None` (passive). After exploration, the
    /// arbiter calls `on_exploration_complete()` with the shared manager.
    fn requires_exploration(&self) -> bool {
        false
    }

    /// Called when the arbiter's exploration phase completes. Constraints that
    /// returned `true` from `requires_exploration()` should extract their
    /// gains from the manager by matching on their target metric.
    fn on_exploration_complete(&mut self, _manager: &ExplorationManager) {}

    /// Called when warmup completes, with baseline data.
    ///
    /// Constraints that need warmup data (e.g., latency threshold derived from
    /// baseline × multiplier) override this to initialize. Each constraint
    /// stores its own multiplier if needed. Other constraints ignore this.
    fn on_warmup_complete(&mut self, _baseline: &WarmupBaseline) {}

    /// Called when the rate is overridden externally (e.g., hold/release API,
    /// `set_rate`). Constraints with derivative state (PID) should invalidate
    /// it to prevent a derivative kick on the next evaluate. Threshold
    /// constraints can ignore this. Default: no-op.
    fn on_rate_override(&mut self, _new_rate: f64) {}
}

/// Baseline data from the warmup phase, provided to constraints that need it.
pub struct WarmupBaseline {
    /// Median p99 latency observed during warmup (milliseconds).
    pub baseline_p99_ms: f64,
    /// Number of warmup samples collected.
    pub sample_count: usize,
}
