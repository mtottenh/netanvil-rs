//! Unified constraint-arbitrating rate controller.
//!
//! Holds N constraints (threshold, PID, or mixed), composes them via a
//! pluggable `CompositionStrategy`, and applies arbiter-level policies
//! (ceiling, floor, cooldown, rate-of-change limiting).

use std::sync::Arc;
use std::time::{Duration, Instant};

use netanvil_types::{
    ControllerInfo, ControllerType, MetricsSummary, RateController, RateDecision,
};

use super::autotune::ExplorationManager;
use super::ceiling::ProgressiveCeiling;
use super::clock::{Clock, SystemClock};
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
    clock: Arc<dyn Clock>,
}

impl KnownGoodTracker {
    fn new(floor_fraction: f64, window: Duration, initial_rate: f64, clock: Arc<dyn Clock>) -> Self {
        let now = clock.now();
        Self {
            max_recent_rate: initial_rate,
            max_recent_rate_time: now,
            floor_fraction,
            window,
            decay_factor: 0.95,
            clock,
        }
    }

    fn update(&mut self, current_rps: f64) {
        if current_rps > self.max_recent_rate {
            self.max_recent_rate = current_rps;
            self.max_recent_rate_time = self.clock.now();
        } else if self.clock.elapsed_since(self.max_recent_rate_time) > self.window {
            self.max_recent_rate *= self.decay_factor;
            self.max_recent_rate_time = self.clock.now();
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
    Exploration(ExplorationManager),
    Warmup {
        warmup_rps: f64,
        warmup_duration: Duration,
        start: Instant,
        p99_samples: Vec<f64>,
        latency_multiplier: f64,
    },
    Active,
}

/// Abort threshold: if a catastrophic constraint desires rate below this
/// fraction of the exploration rate, abort exploration.
const EXPLORATION_ABORT_THRESHOLD: f64 = 0.85;

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
    clock: Option<Arc<dyn Clock>>,
    exploration_manager: Option<ExplorationManager>,
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
            clock: None,
            exploration_manager: None,
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

    /// Inject a custom clock for deterministic testing. If not called,
    /// `Arbiter::new()` uses `SystemClock`.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Enable an exploration phase before active control. The arbiter will
    /// drive rate according to the exploration pattern and deliver computed
    /// gains to constraints via `on_exploration_complete()`.
    pub fn with_exploration(mut self, manager: ExplorationManager) -> Self {
        self.exploration_manager = Some(manager);
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

    // Observability: which constraint was binding on the last tick.
    last_binding: Option<String>,

    clock: Arc<dyn Clock>,

    // Preserved from config for Exploration→Warmup transition.
    warmup_config: Option<(f64, Duration, f64)>,

    // Initial RPS from config, needed for post-exploration phase setup.
    initial_rps: f64,
}

impl Arbiter {
    pub fn new(config: ArbiterConfig) -> Self {
        let clock: Arc<dyn Clock> =
            config.clock.unwrap_or_else(|| Arc::new(SystemClock));

        // Determine phase: Exploration takes priority, then Warmup, then Active.
        let warmup_config = config.warmup;
        let phase = if let Some(manager) = config.exploration_manager {
            ArbiterPhase::Exploration(manager)
        } else if let Some((rps, dur, mult)) = warmup_config {
            ArbiterPhase::Warmup {
                warmup_rps: rps,
                warmup_duration: dur,
                start: clock.now(),
                p99_samples: Vec::with_capacity(64),
                latency_multiplier: mult,
            }
        } else {
            ArbiterPhase::Active
        };

        let current_rps = match &phase {
            ArbiterPhase::Exploration(mgr) => mgr.initial_rps * 0.5, // baseline rate
            ArbiterPhase::Warmup { warmup_rps, .. } => *warmup_rps,
            ArbiterPhase::Active => config.initial_rps,
        };

        let ceiling_start = config.initial_rps; // ceiling starts from initial, not exploration baseline

        // Ceiling is deferred (not started) during Exploration and Warmup.
        let ceiling = if matches!(phase, ArbiterPhase::Active) {
            ProgressiveCeiling::started_with_clock(
                ceiling_start,
                config.max_rps,
                config.test_duration / 2,
                clock.clone(),
            )
        } else {
            ProgressiveCeiling::new_with_clock(
                ceiling_start,
                config.max_rps,
                config.test_duration / 2,
                clock.clone(),
            )
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
                clock.clone(),
            ),
            increase: IncreasePolicy::new(config.increase),
            rate_change_limits: config.rate_change_limits,
            backoff_detection_threshold: config.backoff_detection_threshold,
            ticks_since_increase: 0,
            ramping_ticks: 0,
            log_every_n_ticks: 10,
            last_binding: None,
            clock,
            warmup_config,
            initial_rps: config.initial_rps,
        }
    }

    /// Override the default INFO-level tick logging cadence. Set to 0 to
    /// log every tick (verbose); set to a large value to suppress periodic
    /// logging entirely (state-change events still log).
    pub fn set_log_every_n_ticks(&mut self, n: u32) {
        self.log_every_n_ticks = n;
    }

    /// Extract the ExplorationManager, deliver gains to constraints, and
    /// transition to the next phase (Warmup if configured, else Active).
    ///
    /// Used by both the natural completion path and the abort path.
    fn finish_exploration(&mut self) {
        let manager = match std::mem::replace(&mut self.phase, ArbiterPhase::Active) {
            ArbiterPhase::Exploration(m) => m,
            _ => return,
        };

        let phase_label = format!("{:?}", manager.phase);
        tracing::info!(
            exploration_phase = phase_label,
            "arbiter: exploration finished, delivering gains to constraints"
        );

        // Sequence point: all tick() calls are done before this point.
        // The manager is now owned (moved out of the phase via mem::replace),
        // so we can freely iterate &mut self.constraints while borrowing &manager.
        for c in &mut self.constraints {
            c.on_exploration_complete(&manager);
        }
        self.transition_post_exploration();
    }

    /// After exploration completes, transition to Warmup (if configured) or Active.
    fn transition_post_exploration(&mut self) {
        if let Some((rps, dur, mult)) = self.warmup_config.take() {
            self.current_rps = rps;
            self.phase = ArbiterPhase::Warmup {
                warmup_rps: rps,
                warmup_duration: dur,
                start: self.clock.now(),
                p99_samples: Vec::with_capacity(64),
                latency_multiplier: mult,
            };
            tracing::info!("arbiter: exploration → warmup");
        } else {
            self.current_rps = self.initial_rps;
            self.ceiling.start_now();
            // phase is already Active from the mem::replace in abort_exploration
            // or set by the caller
            tracing::info!("arbiter: exploration → active");
        }
    }

    fn exploration_tick(&mut self, summary: &MetricsSummary) -> RateDecision {
        // Borrow scope: &mut self.phase is borrowed only for the manager.tick()
        // call inside the Some(rate) arm. The borrow ends at the arm boundary,
        // so the else branch can call finish_exploration() which does mem::replace
        // on self.phase without conflict.
        let tick_result = match &mut self.phase {
            ArbiterPhase::Exploration(m) => m.tick(summary),
            _ => unreachable!("exploration_tick called outside exploration phase"),
        };

        if let Some(rate) = tick_result {
            // Still exploring. Safety: evaluate only Catastrophic constraints.
            // Operating-point constraints are NOT evaluated to avoid contaminating
            // their smoother/persistence state with exploration's artificial step.
            let mut safe_rate = rate;
            let ctx = EvalContext {
                summary,
                current_rate: self.current_rps,
                ticks_since_increase: 0,
                min_rps: self.min_rps,
                max_rps: self.max_rps,
            };

            let mut should_abort = false;
            for c in &mut self.constraints {
                if c.requires_exploration() || c.class() != ConstraintClass::Catastrophic {
                    continue;
                }
                if let Some(output) = c.evaluate(&ctx) {
                    let abort = match output.intent {
                        ConstraintIntent::NoObjection => false,
                        ConstraintIntent::Hold(_) => output.severity >= 1.0,
                        ConstraintIntent::DesireRate(d) => {
                            d < safe_rate * EXPLORATION_ABORT_THRESHOLD
                        }
                    };
                    if abort {
                        tracing::warn!(
                            constraint = %c.id(),
                            severity = format!("{:.2}", output.severity),
                            "catastrophic constraint fired during exploration, aborting"
                        );
                        should_abort = true;
                        if let ConstraintIntent::DesireRate(d) = output.intent {
                            safe_rate = safe_rate.min(d);
                        }
                    }
                }
            }

            if should_abort {
                self.current_rps = safe_rate.clamp(self.min_rps, self.max_rps);
                self.finish_exploration();
                return RateDecision {
                    target_rps: self.current_rps,
                };
            }

            self.current_rps = safe_rate.clamp(self.min_rps, self.max_rps);
            RateDecision {
                target_rps: self.current_rps,
            }
        } else {
            // Exploration complete (or failed — compute_gains_for handles both).
            self.finish_exploration();

            // Run the next phase's first tick immediately. Note: if warmup_tick
            // or active_tick panics, the stack trace will show exploration_tick
            // as the caller — this is expected, not a bug.
            match &self.phase {
                ArbiterPhase::Warmup { .. } => self.warmup_tick(summary),
                ArbiterPhase::Active => self.active_tick(summary),
                ArbiterPhase::Exploration(_) => unreachable!(),
            }
        }
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
                _ => unreachable!("warmup_tick called outside warmup phase"),
            };

        // Collect warmup samples
        if summary.total_requests > 0 {
            let p99_ms = summary.latency_p99_ns as f64 / 1_000_000.0;
            if p99_ms > 0.0 {
                p99_samples.push(p99_ms);
            }
        }

        // Check transition
        if self.clock.elapsed_since(start) >= warmup_duration && p99_samples.len() >= 3 {
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
                self.clock.clone(),
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

        self.last_binding = result
            .binding
            .map(|i| self.constraints[i].id().to_string());

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
            ArbiterPhase::Exploration(_) => self.exploration_tick(summary),
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
        // Abort exploration on external rate override.
        if matches!(self.phase, ArbiterPhase::Exploration(_)) {
            tracing::info!(rps, "arbiter: external set_rate during exploration, aborting");
            self.finish_exploration();
        }
    }

    fn set_max_rps(&mut self, max_rps: f64) {
        self.max_rps = max_rps.max(self.min_rps);
        self.ceiling.set_end_value(self.max_rps);
    }

    fn last_binding(&self) -> Option<&str> {
        self.last_binding.as_deref()
    }

    fn controller_state(&self) -> Vec<(&'static str, f64)> {
        if let ArbiterPhase::Exploration(ref manager) = self.phase {
            return vec![(
                "netanvil_arbiter_exploration_progress",
                manager.exploration_progress(),
            )];
        }
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
            controller_type: ControllerType::Arbiter,
            current_rps: self.current_rps,
            editable_actions: vec!["set_max_rps".into(), "set_min_rps".into()],
            params: serde_json::json!({
                "phase": match &self.phase {
                    ArbiterPhase::Exploration(_) => "exploration",
                    ArbiterPhase::Warmup { .. } => "warmup",
                    ArbiterPhase::Active => "active",
                },
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
                    if let Some((id, constraint_action)) = rest.split_once('.') {
                        let c = self
                            .constraints
                            .iter_mut()
                            .find(|c| c.id() == id)
                            .ok_or_else(|| format!("no constraint with id '{id}'"))?;
                        c.apply_update(constraint_action, params)
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
