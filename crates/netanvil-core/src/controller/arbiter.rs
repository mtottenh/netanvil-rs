//! Unified constraint-arbitrating rate controller.
//!
//! Holds N constraints (threshold, PID, or mixed), composes them via a
//! pluggable `CompositionStrategy`, and applies arbiter-level policies
//! (ceiling, floor, cooldown, rate-of-change limiting).

use std::time::{Duration, Instant};

use netanvil_types::{
    ControllerInfo, ControllerType, MetricsSummary, RateController, RateDecision,
};

use super::ceiling::ProgressiveCeiling;
use super::composition::{CompositionStrategy, ConstraintEval, MinSelector};
use super::constraint::{Constraint, ConstraintIntent, EvalContext, WarmupBaseline};
use super::constraints::ConstraintClass;

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

/// Tracks post-backoff cooldown (suppresses rate increases).
struct CooldownState {
    remaining: u32,
    min_ticks: u32,
    recovery_multiplier: f64,
}

impl CooldownState {
    fn new(min_ticks: u32, recovery_multiplier: f64) -> Self {
        Self {
            remaining: 0,
            min_ticks,
            recovery_multiplier,
        }
    }

    fn is_active(&self) -> bool {
        self.remaining > 0
    }

    fn tick(&mut self) {
        if self.remaining > 0 {
            self.remaining -= 1;
        }
    }

    /// Engage cooldown using only the constraints that triggered the backoff.
    /// Bug 3 fix: previously this took the max recovery estimate across ALL
    /// constraints, meaning the slowest signal in the system governed every
    /// cooldown regardless of relevance.
    fn engage(&mut self, triggering: &[&dyn Constraint]) {
        let max_estimate = triggering
            .iter()
            .filter_map(|c| c.recovery_estimate_ticks())
            .fold(0.0_f64, |acc, x| acc.max(x as f64));

        let cooldown_ticks = if max_estimate > 0.0 {
            let estimated = (max_estimate * self.recovery_multiplier).ceil() as u32;
            estimated.max(self.min_ticks)
        } else {
            self.min_ticks
        };
        self.remaining = cooldown_ticks;
    }
}

/// Tracks the known-good rate floor.
///
/// Uses geometric decay (95% per window) instead of reset-to-current when
/// the window expires without seeing a higher rate. This prevents the
/// "floor ratchet" failure mode where a single backoff cycle permanently
/// lowers the floor by 20% and the system collapses geometrically.
struct KnownGoodTracker {
    max_recent_rate: f64,
    max_recent_rate_time: Instant,
    floor_fraction: f64,
    window: Duration,
    decay_factor: f64,
}

impl KnownGoodTracker {
    fn new(floor_fraction: f64, window: Duration, initial_rate: f64) -> Self {
        Self {
            max_recent_rate: initial_rate,
            max_recent_rate_time: Instant::now(),
            floor_fraction,
            window,
            decay_factor: 0.95,
        }
    }

    fn update(&mut self, current_rps: f64) {
        if current_rps > self.max_recent_rate {
            // New peak: remember it.
            self.max_recent_rate = current_rps;
            self.max_recent_rate_time = Instant::now();
        } else if self.max_recent_rate_time.elapsed() > self.window {
            // Window expired without seeing a higher rate. Decay gently
            // rather than resetting to `current_rps` (which would create
            // a ratchet effect under sustained pressure).
            self.max_recent_rate *= self.decay_factor;
            self.max_recent_rate_time = Instant::now();
        }
    }

    fn floor(&self, min_rps: f64) -> f64 {
        (self.max_recent_rate * self.floor_fraction).max(min_rps)
    }
}

/// Configuration for congestion avoidance behavior.
///
/// Near the last known failure rate, the controller switches from
/// multiplicative increase to additive increase (like TCP's congestion
/// avoidance phase). These parameters control that transition.
pub struct CongestionAvoidanceConfig {
    /// Switch from multiplicative to additive increase when `current_rate`
    /// exceeds this fraction of the last failure rate. Default: 0.85.
    pub trigger_threshold: f64,
    /// Additive increase = `failure_rate × this`. Default: 0.01.
    pub additive_fraction: f64,
    /// EMA smoothing on the failure-rate estimate. Default: 0.3.
    pub failure_rate_alpha: f64,
}

impl Default for CongestionAvoidanceConfig {
    fn default() -> Self {
        Self {
            trigger_threshold: 0.85,
            additive_fraction: 0.01,
            failure_rate_alpha: 0.3,
        }
    }
}

/// Configuration for the increase policy (how rate grows when no constraint objects).
pub struct IncreasePolicyConfig {
    /// Multiplicative increase factor per clean tick. Default: 1.10 (10%/tick).
    pub increase_factor: f64,
    /// Congestion avoidance settings. `None` disables congestion avoidance
    /// (always uses multiplicative increase).
    pub congestion_avoidance: Option<CongestionAvoidanceConfig>,
}

impl Default for IncreasePolicyConfig {
    fn default() -> Self {
        Self {
            increase_factor: 1.10,
            congestion_avoidance: Some(CongestionAvoidanceConfig::default()),
        }
    }
}

/// Increase policy: multiplicative vs additive (congestion avoidance).
struct IncreasePolicy {
    config: IncreasePolicyConfig,
    failure_rate_ema: f64,
    backoff_count: u32,
}

impl IncreasePolicy {
    fn new(config: IncreasePolicyConfig) -> Self {
        Self {
            config,
            failure_rate_ema: 0.0,
            backoff_count: 0,
        }
    }

    fn compute(&self, current_rps: f64, cooldown: &CooldownState) -> f64 {
        if cooldown.is_active() {
            current_rps // suppress increases during cooldown
        } else if let Some(ref ca) = self.config.congestion_avoidance {
            if self.failure_rate_ema > 0.0
                && current_rps > self.failure_rate_ema * ca.trigger_threshold
            {
                let additive = self.failure_rate_ema * ca.additive_fraction;
                current_rps + additive
            } else {
                current_rps * self.config.increase_factor
            }
        } else {
            current_rps * self.config.increase_factor
        }
    }

    fn record_backoff(&mut self, current_rps: f64) {
        let alpha = self
            .config
            .congestion_avoidance
            .as_ref()
            .map(|ca| ca.failure_rate_alpha)
            .unwrap_or(0.3);

        if self.failure_rate_ema == 0.0 {
            self.failure_rate_ema = current_rps;
        } else {
            self.failure_rate_ema = alpha * current_rps + (1.0 - alpha) * self.failure_rate_ema;
        }
        self.backoff_count += 1;
    }
}

// ---------------------------------------------------------------------------
// Asymmetric per-tick rate-of-change clamp
// ---------------------------------------------------------------------------

/// Per-tick limits on how much the rate can change in one tick, expressed
/// as fractions of the current rate.
///
/// Asymmetric by default: increases are bounded more tightly than decreases
/// because the cost of overshoot (cascading saturation) is much higher than
/// the cost of undershoot (slower convergence). This matches the asymmetry
/// already baked into AIMD's multiplicative-decrease semantics.
#[derive(Debug, Clone, Copy)]
pub struct RateChangeLimits {
    /// Maximum upward change per tick, as a fraction of current rate.
    /// Default: 0.20 (+20%/tick).
    pub max_increase_pct: f64,
    /// Maximum downward change per tick, as a fraction of current rate.
    /// Default: 0.50 (-50%/tick), allowing aggressive multiplicative
    /// decrease for catastrophic constraints.
    pub max_decrease_pct: f64,
}

impl Default for RateChangeLimits {
    fn default() -> Self {
        Self {
            max_increase_pct: 0.20,
            max_decrease_pct: 0.50,
        }
    }
}

impl RateChangeLimits {
    fn clamp(&self, desired: f64, current: f64) -> f64 {
        let max_up = current * (1.0 + self.max_increase_pct);
        let max_down = current * (1.0 - self.max_decrease_pct);
        desired.clamp(max_down, max_up)
    }
}

// ---------------------------------------------------------------------------
// Arbiter phase
// ---------------------------------------------------------------------------

enum ArbiterPhase {
    Warmup {
        warmup_rps: f64,
        warmup_duration: Duration,
        start: Instant,
        p99_samples: Vec<f64>,
        latency_multiplier: f64,
    },
    Active,
}

// ---------------------------------------------------------------------------
// Arbiter configuration
// ---------------------------------------------------------------------------

/// Configuration for building an Arbiter.
///
/// Required fields are positional in `new()`; optional fields have
/// chainable setters with sensible defaults. This eliminates the
/// "placeholder zero" footgun where forgetting to set a field would
/// silently produce a controller running at 0 RPS.
pub struct ArbiterConfig {
    // Required
    constraints: Vec<Box<dyn Constraint>>,
    initial_rps: f64,
    min_rps: f64,
    max_rps: f64,
    test_duration: Duration,

    // Optional with defaults
    strategy: Box<dyn CompositionStrategy>,
    increase: IncreasePolicyConfig,
    rate_change_limits: RateChangeLimits,
    backoff_detection_threshold: f64,
    cooldown_min_ticks: u32,
    cooldown_recovery_multiplier: f64,
    known_good_floor_fraction: f64,
    known_good_window: Duration,
    warmup: Option<(f64, Duration, f64)>,
}

impl ArbiterConfig {
    /// Create a config with required fields. Optional fields take defaults
    /// matching the old Ramp controller behavior.
    pub fn new(
        constraints: Vec<Box<dyn Constraint>>,
        initial_rps: f64,
        min_rps: f64,
        max_rps: f64,
        test_duration: Duration,
    ) -> Self {
        Self {
            constraints,
            initial_rps,
            min_rps,
            max_rps,
            test_duration,
            strategy: Box::new(MinSelector),
            increase: IncreasePolicyConfig::default(),
            rate_change_limits: RateChangeLimits::default(),
            backoff_detection_threshold: 0.98,
            cooldown_min_ticks: 2,
            cooldown_recovery_multiplier: 1.5,
            known_good_floor_fraction: 0.80,
            known_good_window: Duration::from_secs(60),
            warmup: None,
        }
    }

    pub fn with_strategy(mut self, strategy: Box<dyn CompositionStrategy>) -> Self {
        self.strategy = strategy;
        self
    }

    pub fn with_increase_policy(mut self, increase: IncreasePolicyConfig) -> Self {
        self.increase = increase;
        self
    }

    pub fn with_rate_change_limits(mut self, limits: RateChangeLimits) -> Self {
        self.rate_change_limits = limits;
        self
    }

    pub fn with_backoff_detection_threshold(mut self, threshold: f64) -> Self {
        self.backoff_detection_threshold = threshold;
        self
    }

    pub fn with_cooldown(mut self, min_ticks: u32, recovery_multiplier: f64) -> Self {
        self.cooldown_min_ticks = min_ticks;
        self.cooldown_recovery_multiplier = recovery_multiplier;
        self
    }

    pub fn with_known_good_floor(mut self, fraction: f64, window: Duration) -> Self {
        self.known_good_floor_fraction = fraction;
        self.known_good_window = window;
        self
    }

    pub fn with_warmup(mut self, rps: f64, duration: Duration, latency_multiplier: f64) -> Self {
        self.warmup = Some((rps, duration, latency_multiplier));
        self
    }
}

// ---------------------------------------------------------------------------
// Arbiter
// ---------------------------------------------------------------------------

/// Unified constraint-arbitrating rate controller.
///
/// Holds N constraints (threshold-based, PID-based, or mixed), composes them
/// via a pluggable `CompositionStrategy`, and applies arbiter-level policies.
pub struct Arbiter {
    constraints: Vec<Box<dyn Constraint>>,
    strategy: Box<dyn CompositionStrategy>,
    current_rps: f64,

    min_rps: f64,
    max_rps: f64,

    phase: ArbiterPhase,
    ceiling: ProgressiveCeiling,
    cooldown: CooldownState,
    known_good: KnownGoodTracker,
    increase: IncreasePolicy,
    rate_change_limits: RateChangeLimits,
    backoff_detection_threshold: f64,

    ticks_since_increase: u32,
    ramping_ticks: u32,

    // Diagnostic: log every Nth tick at INFO level (state-change events
    // are always logged). Reduces log volume at high control frequencies.
    log_every_n_ticks: u32,
}

impl Arbiter {
    pub fn new(config: ArbiterConfig) -> Self {
        let phase = if let Some((rps, dur, mult)) = config.warmup {
            ArbiterPhase::Warmup {
                warmup_rps: rps,
                warmup_duration: dur,
                start: Instant::now(),
                p99_samples: Vec::with_capacity(64),
                latency_multiplier: mult,
            }
        } else {
            ArbiterPhase::Active
        };

        let current_rps = match &phase {
            ArbiterPhase::Warmup { warmup_rps, .. } => *warmup_rps,
            ArbiterPhase::Active => config.initial_rps,
        };

        let ceiling_start = current_rps;

        // If no warmup, start the ceiling immediately. With warmup,
        // the ceiling is started on transition to Active so that the
        // progressive-ramp clock doesn't tick during warmup.
        let ceiling = if matches!(phase, ArbiterPhase::Active) {
            ProgressiveCeiling::started(ceiling_start, config.max_rps, config.test_duration / 2)
        } else {
            ProgressiveCeiling::new(ceiling_start, config.max_rps, config.test_duration / 2)
        };

        Self {
            constraints: config.constraints,
            strategy: config.strategy,
            current_rps,
            min_rps: config.min_rps,
            max_rps: config.max_rps,
            phase,
            ceiling,
            cooldown: CooldownState::new(
                config.cooldown_min_ticks,
                config.cooldown_recovery_multiplier,
            ),
            known_good: KnownGoodTracker::new(
                config.known_good_floor_fraction,
                config.known_good_window,
                current_rps,
            ),
            increase: IncreasePolicy::new(config.increase),
            rate_change_limits: config.rate_change_limits,
            backoff_detection_threshold: config.backoff_detection_threshold,
            ticks_since_increase: 0,
            ramping_ticks: 0,
            log_every_n_ticks: 10,
        }
    }

    /// Override the default INFO-level tick logging cadence. Set to 0 to
    /// log every tick (verbose); set to a large value to suppress periodic
    /// logging entirely (state-change events still log).
    pub fn set_log_every_n_ticks(&mut self, n: u32) {
        self.log_every_n_ticks = n;
    }

    fn warmup_tick(&mut self, summary: &MetricsSummary) -> RateDecision {
        let (warmup_rps, warmup_duration, start, p99_samples, latency_multiplier) =
            match &mut self.phase {
                ArbiterPhase::Warmup {
                    warmup_rps,
                    warmup_duration,
                    start,
                    p99_samples,
                    latency_multiplier,
                } => (
                    *warmup_rps,
                    *warmup_duration,
                    *start,
                    p99_samples,
                    *latency_multiplier,
                ),
                ArbiterPhase::Active => unreachable!(),
            };

        // Collect warmup samples
        if summary.total_requests > 0 {
            let p99_ms = summary.latency_p99_ns as f64 / 1_000_000.0;
            if p99_ms > 0.0 {
                p99_samples.push(p99_ms);
            }
        }

        // Check transition
        if start.elapsed() >= warmup_duration && p99_samples.len() >= 3 {
            // Compute baseline (median for outlier resistance)
            p99_samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let n = p99_samples.len();
            let baseline_p99_ms = p99_samples[n / 2];
            let target_p99_ms = baseline_p99_ms * latency_multiplier;

            tracing::info!(
                baseline_p99_ms = format!("{:.2}", baseline_p99_ms),
                target_p99_ms = format!("{:.2}", target_p99_ms),
                multiplier = latency_multiplier,
                warmup_samples = n,
                "arbiter warmup complete, transitioning to active control"
            );

            // Notify constraints via lifecycle hook.
            let baseline = WarmupBaseline {
                baseline_p99_ms,
                sample_count: n,
            };
            for c in &mut self.constraints {
                c.on_warmup_complete(&baseline, latency_multiplier);
            }

            self.ceiling.start_now();
            self.known_good = KnownGoodTracker::new(
                self.known_good.floor_fraction,
                self.known_good.window,
                warmup_rps,
            );
            self.phase = ArbiterPhase::Active;

            return self.active_tick(summary);
        }

        RateDecision {
            target_rps: warmup_rps,
        }
    }

    fn active_tick(&mut self, summary: &MetricsSummary) -> RateDecision {
        self.ramping_ticks += 1;

        // 1. Compute increase rate FIRST (needed as default_rate for composition).
        let increase_rate = self.increase.compute(self.current_rps, &self.cooldown);

        // 2. Build evaluation context.
        let ctx = EvalContext {
            summary,
            current_rate: self.current_rps,
            ticks_since_increase: self.ticks_since_increase,
            min_rps: self.min_rps,
            max_rps: self.max_rps,
        };

        // 3. Evaluate all constraints.
        let mut evals = Vec::new();
        let mut abstainers = 0usize;
        for (i, constraint) in self.constraints.iter_mut().enumerate() {
            if let Some(output) = constraint.evaluate(&ctx) {
                match output.intent {
                    ConstraintIntent::NoObjection => {
                        abstainers += 1;
                    }
                    ConstraintIntent::Hold(rate) | ConstraintIntent::DesireRate(rate) => {
                        evals.push(ConstraintEval {
                            constraint_index: i,
                            desired_rate: rate,
                            severity: output.severity,
                            class: constraint.class(),
                        });
                    }
                }
            }
        }

        if evals.is_empty() && !self.constraints.is_empty() {
            tracing::debug!("arbiter: no active constraints with rate opinion this tick");
        }

        // 4. Compose via strategy.
        let result = self.strategy.compose(&evals, abstainers, increase_rate);

        // Bug 2 fix: classify backoff from the binding constraint's intent,
        // BEFORE arbiter-level clamps mask the signal. A drop caused by the
        // ceiling tightening or by set_rate is not a constraint backoff and
        // should not engage cooldown or update failure_rate_ema.
        let constraint_driven_backoff = result.binding.is_some()
            && result.rate < self.current_rps * self.backoff_detection_threshold;

        // 5. Apply arbiter-level policies.

        //    Per-tick rate-of-change clamp (asymmetric: small up, larger down)
        let mut rate = self.rate_change_limits.clamp(result.rate, self.current_rps);

        //    Ceiling
        let ceiling = self.ceiling.ceiling();
        if let Some(milestone) = self.ceiling.check_milestone() {
            tracing::info!(
                progress_pct = milestone,
                ceiling = format!("{:.0}", ceiling),
                "arbiter ceiling milestone"
            );
        }
        rate = rate.min(ceiling);

        //    Floor (bypassed if binding is Catastrophic)
        self.known_good.update(self.current_rps);
        let floor = self.known_good.floor(self.min_rps);
        let binding_class = result.binding.map(|i| self.constraints[i].class());
        if binding_class != Some(ConstraintClass::Catastrophic) {
            rate = rate.max(floor);
        }

        //    Absolute bounds
        rate = rate.clamp(self.min_rps, self.max_rps);

        // 6. Post-composition: notify constraints.
        for (i, constraint) in self.constraints.iter_mut().enumerate() {
            let is_binding = result.binding == Some(i);
            constraint.post_select(rate, self.current_rps, is_binding);
        }

        // 7. Update arbiter-level state.
        if constraint_driven_backoff {
            self.increase.record_backoff(self.current_rps);

            // Bug 3 fix: only consider triggering constraints for cooldown
            // duration. The "triggering" set is constraints whose desired
            // rate was below current (they wanted a backoff). For min-selector
            // this includes the binding constraint and any other objectors.
            let triggering: Vec<&dyn Constraint> = evals
                .iter()
                .filter(|e| e.desired_rate < self.current_rps * self.backoff_detection_threshold)
                .map(|e| self.constraints[e.constraint_index].as_ref())
                .collect();

            self.cooldown.engage(&triggering);

            tracing::info!(
                failure_rate = format!("{:.0}", self.increase.failure_rate_ema),
                cooldown_ticks = self.cooldown.remaining,
                backoff_count = self.increase.backoff_count,
                triggering_count = triggering.len(),
                "arbiter: backoff cooldown engaged"
            );
            // Bug 1 fix: do NOT call cooldown.tick() on the same tick we
            // engaged it — that would silently lose one tick of duration.
        } else {
            self.cooldown.tick();
        }

        if rate > self.current_rps {
            self.ticks_since_increase = 0;
        } else {
            self.ticks_since_increase = self.ticks_since_increase.saturating_add(1);
        }

        // 8. Periodic tick logging (DEBUG by default; INFO on milestones
        //    and state changes only).
        let should_log_tick =
            self.log_every_n_ticks > 0 && self.ramping_ticks % self.log_every_n_ticks == 0;
        if should_log_tick {
            let binding_label = result
                .binding
                .map(|i| self.constraints[i].id())
                .unwrap_or("none");
            tracing::debug!(
                rate = format!("{:.0}→{:.0}", self.current_rps, rate),
                binding = binding_label,
                ceiling = format!("{:.0}", ceiling),
                floor = format!("{:.0}", floor),
                in_cooldown = self.cooldown.is_active(),
                "arbiter tick"
            );
        }

        self.current_rps = rate;

        RateDecision {
            target_rps: self.current_rps,
        }
    }

    /// Notify all constraints that the rate has been overridden externally.
    /// Constraints with derivative state (PID) should invalidate it to
    /// prevent a derivative kick on the next evaluate.
    fn notify_rate_override(&mut self) {
        for c in &mut self.constraints {
            c.on_rate_override(self.current_rps);
        }
    }
}

impl RateController for Arbiter {
    fn update(&mut self, summary: &MetricsSummary) -> RateDecision {
        match &self.phase {
            ArbiterPhase::Warmup { .. } => self.warmup_tick(summary),
            ArbiterPhase::Active => self.active_tick(summary),
        }
    }

    fn current_rate(&self) -> f64 {
        self.current_rps
    }

    fn set_rate(&mut self, rps: f64) {
        let old = self.current_rps;
        self.current_rps = rps.clamp(self.min_rps, self.max_rps);
        if (self.current_rps - old).abs() > f64::EPSILON {
            // External override: tell constraints so PID derivatives don't
            // kick on the next evaluate.
            self.notify_rate_override();
        }
    }

    fn set_max_rps(&mut self, max_rps: f64) {
        self.max_rps = max_rps.max(self.min_rps);
        self.ceiling.set_end_value(self.max_rps);
    }

    fn controller_state(&self) -> Vec<(&'static str, f64)> {
        if matches!(self.phase, ArbiterPhase::Warmup { .. }) {
            return Vec::new();
        }
        vec![
            ("netanvil_arbiter_ceiling", self.ceiling.ceiling()),
            (
                "netanvil_arbiter_floor",
                self.known_good.floor(self.min_rps),
            ),
            (
                "netanvil_arbiter_in_cooldown",
                if self.cooldown.is_active() { 1.0 } else { 0.0 },
            ),
            (
                "netanvil_arbiter_cooldown_remaining",
                self.cooldown.remaining as f64,
            ),
            (
                "netanvil_arbiter_failure_rate",
                self.increase.failure_rate_ema,
            ),
            (
                "netanvil_arbiter_backoff_count",
                self.increase.backoff_count as f64,
            ),
            (
                "netanvil_arbiter_ticks_since_increase",
                self.ticks_since_increase as f64,
            ),
        ]
    }

    fn controller_info(&self) -> ControllerInfo {
        ControllerInfo {
            // TODO: add ControllerType::Arbiter variant in netanvil-types.
            // Reporting as Ramp until that lands; downstream dashboards
            // labeling controllers by type will need updating.
            controller_type: ControllerType::Ramp,
            current_rps: self.current_rps,
            editable_actions: vec!["set_max_rps".into(), "set_min_rps".into()],
            params: serde_json::json!({
                "phase": if matches!(self.phase, ArbiterPhase::Warmup { .. }) { "warmup" } else { "active" },
                "constraints": self.constraints.iter().map(|c| c.id()).collect::<Vec<_>>(),
                "ramping_ticks": self.ramping_ticks,
                "in_cooldown": self.cooldown.is_active(),
                "cooldown_remaining": self.cooldown.remaining,
            }),
        }
    }

    fn apply_update(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        // Arbiter-level updates
        match action {
            "set_max_rps" => {
                let max = params
                    .get("max_rps")
                    .and_then(|v| v.as_f64())
                    .ok_or("missing 'max_rps' field")?;
                let old = self.max_rps;
                self.set_max_rps(max);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_max_rps": old,
                    "new_max_rps": self.max_rps,
                }))
            }
            "set_min_rps" => {
                let min = params
                    .get("min_rps")
                    .and_then(|v| v.as_f64())
                    .ok_or("missing 'min_rps' field")?;
                let old = self.min_rps;
                self.min_rps = min;
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_min_rps": old,
                    "new_min_rps": self.min_rps,
                }))
            }
            _ => {
                // Constraint-level: "constraint.{id}.{action}"
                if let Some(rest) = action.strip_prefix("constraint.") {
                    if let Some((id, _constraint_action)) = rest.split_once('.') {
                        let c = self
                            .constraints
                            .iter_mut()
                            .find(|c| c.id() == id)
                            .ok_or_else(|| format!("no constraint with id '{id}'"))?;
                        c.apply_update(params)
                    } else {
                        Err(format!(
                            "expected 'constraint.{{id}}.{{action}}', got '{action}'"
                        ))
                    }
                } else {
                    Err(format!(
                        "action '{}' is not valid for arbiter controller",
                        action
                    ))
                }
            }
        }
    }
}
