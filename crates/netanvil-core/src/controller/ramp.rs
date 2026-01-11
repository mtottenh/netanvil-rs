//! Adaptive ramp rate controller using gentle AIMD.
//!
//! Phase 1 (warmup): Runs at a low fixed RPS to learn the baseline p99 latency.
//! Phase 2 (ramp): Uses AIMD (Additive Increase / Multiplicative Decrease) with
//!   graduated backoff, median-smoothed p99, and violation streak tracking to
//!   discover the maximum sustainable rate.
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
    ControllerInfo, ControllerType, MetricsSummary, RateController, RateDecision,
};

use super::ceiling::ProgressiveCeiling;

// ---------------------------------------------------------------------------
// AIMD parameters — all interpretable and independently tunable.
// ---------------------------------------------------------------------------

/// Multiplicative increase factor per clean tick (10% per tick).
const INCREASE_FACTOR: f64 = 1.10;

/// Graduated backoff factors based on violation severity (p99/target ratio).
/// Smaller violations get gentler cuts; severe violations get hard cuts.
const BACKOFF_GENTLE: f64 = 0.90; // ratio 1.0–1.5: gentle cut
const BACKOFF_MODERATE: f64 = 0.75; // ratio 1.5–2.0: real cut
const BACKOFF_HARD: f64 = 0.50; // ratio > 2.0: emergency cut

/// Error rate backoff factors.
const ERROR_BACKOFF_GENTLE: f64 = 0.90; // approaching limit
const ERROR_BACKOFF_HARD: f64 = 0.50; // exceeded limit

/// Default number of p99 samples to keep for median smoothing.
/// At a 3-second control interval, 3 samples = 9 seconds of history.
/// More samples reject more noise but add lag to real capacity changes.
const DEFAULT_SMOOTHING_WINDOW: usize = 3;

/// Don't drop below this fraction of the best recent rate.
const KNOWN_GOOD_FLOOR_FRACTION: f64 = 0.80;

/// How long to remember the best known-good rate.
const KNOWN_GOOD_WINDOW: Duration = Duration::from_secs(60);

/// Minimum cooldown (in ticks) after a backoff before allowing any rate
/// increase. Prevents the controller from ramping back into a plant that
/// hasn't finished recovering from the previous disturbance.
/// At a 3s control interval, 2 ticks = 6 seconds.
const COOLDOWN_MIN_TICKS: u32 = 2;

/// Multiplier applied to the observed recovery time (in ticks) to compute
/// cooldown. 1.5× gives a safety margin over the plant's actual recovery.
const COOLDOWN_RECOVERY_MULTIPLIER: f64 = 1.5;

/// EMA smoothing factor for recovery time estimation.
/// Lower values = more weight to history, slower adaptation.
const RECOVERY_EMA_ALPHA: f64 = 0.3;

/// When approaching the last known failure rate (within this fraction),
/// switch from multiplicative increase to additive increase.
const CONGESTION_AVOIDANCE_THRESHOLD: f64 = 0.85;

/// Additive increase per tick in congestion avoidance mode, as a fraction
/// of the failure rate. 1% per tick = cautious linear probing.
const ADDITIVE_INCREASE_FRACTION: f64 = 0.01;

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
    /// More samples = better noise rejection but slower response to
    /// real capacity changes. At 3s control interval: 3 samples = 9s,
    /// 5 samples = 15s, 7 samples = 21s of history.
    pub smoothing_window: usize,
    /// When true, freeze rate decisions if achieved/target > 1.3.
    /// This is a band-aid for timeout-wave measurement corruption:
    /// when completions/s vastly exceeds the target (because old requests
    /// are timing out en masse), all metrics are unreliable. Freezing
    /// prevents the controller from reacting to ghosts.
    ///
    /// Intended to be disabled once a per-core in-flight limit is deployed
    /// (which bounds the timeout wave at the source). Leave the field in
    /// place as a rollback lever; delete after 6 months if unused.
    pub enable_ratio_freeze: bool,
    /// External signal constraints. Empty = no external constraints (default).
    pub external_constraints: Vec<netanvil_types::ExternalConstraintConfig>,
}

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

    // -- AIMD state --
    /// Recent raw p99 values for median smoothing.
    p99_window: VecDeque<f64>,
    /// Consecutive ticks where smoothed p99 >= target.
    latency_violation_streak: u32,
    /// Consecutive ticks where error rate >= limit.
    error_violation_streak: u32,
    /// Best rate seen in the recent window (known-good floor).
    max_recent_rate: f64,
    /// When the max_recent_rate was last updated.
    max_recent_rate_time: Instant,
    /// Last tick's smoothed p99 (for logging).
    last_smoothed_p99: f64,
    /// Ticks since the last rate increase. Used to distinguish self-caused
    /// latency spikes (capacity signal → backoff) from external noise
    /// (hold and observe). If a spike arrives within 2 ticks of an increase,
    /// it's attributed to the increase; otherwise we hold.
    ticks_since_increase: u32,
    /// Total ticks spent in the Ramping state. Used to skip ratio-freeze
    /// during the startup transient when current_rps hasn't caught up to
    /// the agents' actual rate yet.
    ramping_ticks: u32,

    // -- Progressive ceiling --
    ceiling: ProgressiveCeiling,
    last_time_ceiling: f64,

    // -- Post-backoff cooldown --
    /// Remaining cooldown ticks (suppress rate increases). Decremented each
    /// tick; increases are suppressed while > 0.
    cooldown_remaining: u32,
    /// Ticks since the most recent backoff started (for measuring recovery).
    /// `None` when not tracking a recovery.
    ticks_since_backoff: Option<u32>,
    /// EMA of observed recovery time in ticks (how many ticks after backoff
    /// before p99 returns to acceptable levels). Self-tunes the cooldown.
    recovery_time_ema_ticks: f64,

    // -- Congestion avoidance --
    /// EMA of the rate at which backoffs were triggered. When the current
    /// rate approaches this, the controller switches from multiplicative
    /// increase (slow start) to additive increase (congestion avoidance).
    last_failure_rate: f64,
    /// Number of backoffs observed. Used to detect repeated failures.
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
        Self {
            warmup_start: Instant::now(),
            warmup_p99_samples: Vec::with_capacity(64),
            baseline_p99_ms: 0.0,
            target_p99_ms: 0.0,
            current_rps,
            p99_window: VecDeque::with_capacity(smoothing + 1),
            latency_violation_streak: 0,
            error_violation_streak: 0,
            max_recent_rate: current_rps,
            max_recent_rate_time: Instant::now(),
            last_smoothed_p99: 0.0,
            ticks_since_increase: 0,
            ramping_ticks: 0,
            ceiling,
            last_time_ceiling: 0.0,
            cooldown_remaining: 0,
            ticks_since_backoff: None,
            recovery_time_ema_ticks: 0.0,
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

    /// Compute the median of the p99 smoothing window.
    fn smoothed_p99(&self) -> f64 {
        if self.p99_window.is_empty() {
            return 0.0;
        }
        let mut sorted: Vec<f64> = self.p99_window.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        sorted[sorted.len() / 2]
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
        // the target rate, the feedback signal is corrupted by a timeout
        // completion wave. Freeze until measurements catch up.
        //
        // Note: summary.request_rate is completions/s (itself corrupted),
        // so this detects the symptom, not the cause. Correct action regardless.
        //
        // Skip during the first 10 ticks: the AIMD's internal current_rps
        // starts at warmup_rps and needs time to ramp up. During this
        // transient, achieved/target can be >1 simply because the agents'
        // measurement windows span a period of increasing rate.
        //
        // Threshold 2.0: normal distributed operation has 10-30% variance.
        // The timeout wave this band-aid targets produces 150%+, so 2.0
        // avoids false positives while catching the real pathology.
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

        // 0b. Timeout-based backoff: timeouts are a distinct failure mode
        // (target unresponsive, not just slow). Graduated response anchored
        // to the configured max_error_rate.
        if summary.total_requests > 0 {
            let timeout_fraction =
                summary.timeout_count as f64 / summary.total_requests as f64;
            let hard_threshold = (self.config.max_error_rate / 100.0 * 2.5).max(0.05);
            let soft_threshold = (self.config.max_error_rate / 100.0 * 0.5).max(0.01);

            if timeout_fraction > hard_threshold {
                tracing::warn!(
                    timeout_fraction = format!("{:.4}", timeout_fraction),
                    hard_threshold = format!("{:.4}", hard_threshold),
                    "timeout rate critical, hard backoff"
                );
                self.current_rps = (self.current_rps * BACKOFF_HARD)
                    .clamp(self.config.min_rps, self.config.max_rps);
                return RateDecision {
                    target_rps: self.current_rps,
                };
            } else if timeout_fraction > soft_threshold {
                tracing::info!(
                    timeout_fraction = format!("{:.4}", timeout_fraction),
                    soft_threshold = format!("{:.4}", soft_threshold),
                    "timeout rate elevated, gentle backoff"
                );
                self.current_rps = (self.current_rps * BACKOFF_GENTLE)
                    .clamp(self.config.min_rps, self.config.max_rps);
                return RateDecision {
                    target_rps: self.current_rps,
                };
            }
        }

        // 0c. In-flight drop check: the per-core InFlightLimit declined
        // requests because the target is too slow. Graduated response to avoid
        // locking below the true edge during steady-state AIMD oscillation.
        if self.current_rps > 0.0 {
            let drop_rate = summary.in_flight_drops as f64
                / summary.window_duration.as_secs_f64().max(0.001);
            let drop_fraction = drop_rate / self.current_rps;

            if drop_fraction > 0.01 {
                // >1% drops: real saturation, gentle backoff
                tracing::info!(
                    drop_fraction = format!("{:.4}", drop_fraction),
                    in_flight_drops = summary.in_flight_drops,
                    "in-flight limit: >1% drops, gentle backoff"
                );
                self.current_rps = (self.current_rps * BACKOFF_GENTLE)
                    .clamp(self.config.min_rps, self.config.max_rps);
                return RateDecision {
                    target_rps: self.current_rps,
                };
            } else if drop_fraction > 0.001 {
                // 0.1-1% drops: at the edge, hold
                tracing::info!(
                    drop_fraction = format!("{:.4}", drop_fraction),
                    in_flight_drops = summary.in_flight_drops,
                    "in-flight limit: at edge, holding rate"
                );
                return RateDecision {
                    target_rps: self.current_rps,
                };
            }
            // <0.1% drops: noise (transient burst, GC pause), continue normal AIMD
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

        // 2. Update p99 smoothing window.
        let raw_p99 = summary.latency_p99_ns as f64 / 1_000_000.0;
        let window_size = if self.config.smoothing_window > 0 {
            self.config.smoothing_window
        } else {
            DEFAULT_SMOOTHING_WINDOW
        };
        self.p99_window.push_back(raw_p99);
        while self.p99_window.len() > window_size {
            self.p99_window.pop_front();
        }
        let smoothed_p99 = self.smoothed_p99();
        self.last_smoothed_p99 = smoothed_p99;

        // 3. Compute latency violation ratio.
        let latency_ratio = if self.target_p99_ms > 0.0 {
            smoothed_p99 / self.target_p99_ms
        } else {
            0.0
        };

        // 4. Track violation streaks.
        if latency_ratio >= 1.0 {
            self.latency_violation_streak += 1;
        } else {
            self.latency_violation_streak = 0;
        }

        let error_pct = summary.error_rate * 100.0;
        if error_pct >= self.config.max_error_rate {
            self.error_violation_streak += 1;
        } else {
            self.error_violation_streak = 0;
        }

        // 5. Classify violations as self-caused vs external.
        //
        // A spike within 2 ticks of a rate increase is likely a capacity
        // signal (we pushed too hard) → back off. A spike during a stable
        // period is likely external noise (GC, neighbor, network) → hold
        // and observe. This prevents wasting ramp progress on transients
        // the controller didn't cause.
        let self_caused = self.ticks_since_increase <= 2;

        // 5b. Advance recovery tracking timer.
        if let Some(ref mut ticks) = self.ticks_since_backoff {
            *ticks += 1;
        }

        // 5c. Recovery detection: if we're tracking a backoff and latency
        // has returned to acceptable levels, measure the recovery time.
        if let Some(ticks) = self.ticks_since_backoff {
            if latency_ratio < 1.1 && self.latency_violation_streak == 0 {
                let recovery_ticks = ticks as f64;
                if self.recovery_time_ema_ticks == 0.0 {
                    self.recovery_time_ema_ticks = recovery_ticks;
                } else {
                    self.recovery_time_ema_ticks = RECOVERY_EMA_ALPHA * recovery_ticks
                        + (1.0 - RECOVERY_EMA_ALPHA) * self.recovery_time_ema_ticks;
                }
                tracing::info!(
                    recovery_ticks = ticks,
                    ema_ticks = format!("{:.1}", self.recovery_time_ema_ticks),
                    "ramp: plant recovery detected"
                );
                self.ticks_since_backoff = None;
            }
        }

        // 5d. Decrement cooldown counter.
        let in_cooldown = self.cooldown_remaining > 0;
        if in_cooldown {
            self.cooldown_remaining -= 1;
        }

        // 6. Compute latency-driven rate adjustment.
        let (latency_rate, latency_action) = if self.latency_violation_streak == 0 {
            if in_cooldown {
                // During cooldown: hold, don't increase. Let the plant settle.
                (self.current_rps, "hold_cooldown")
            } else if self.last_failure_rate > 0.0
                && self.current_rps > self.last_failure_rate * CONGESTION_AVOIDANCE_THRESHOLD
            {
                // Congestion avoidance: approaching known failure rate.
                // Switch from multiplicative to additive increase.
                let additive = self.last_failure_rate * ADDITIVE_INCREASE_FRACTION;
                (self.current_rps + additive, "increase_ca")
            } else {
                (self.current_rps * INCREASE_FACTOR, "increase")
            }
        } else if !self_caused && latency_ratio < 1.5 {
            // External noise (no recent increase) and not severe: hold.
            (self.current_rps, "hold_external")
        } else if self.latency_violation_streak == 1 && latency_ratio < 1.3 {
            // Single mild self-caused violation: hold and observe.
            (self.current_rps, "hold_mild")
        } else if latency_ratio < 1.5 {
            (self.current_rps * BACKOFF_GENTLE, "backoff_gentle")
        } else if latency_ratio < 2.0 {
            (self.current_rps * BACKOFF_MODERATE, "backoff_moderate")
        } else {
            // Severe violations always trigger hard backoff regardless of cause.
            (self.current_rps * BACKOFF_HARD, "backoff_hard")
        };

        // 7. Compute error-driven rate adjustment.
        let (error_rate, error_action) = if self.error_violation_streak == 0 {
            if in_cooldown {
                (self.current_rps, "hold_cooldown")
            } else {
                (self.current_rps * INCREASE_FACTOR, "increase")
            }
        } else if error_pct < self.config.max_error_rate * 1.5 {
            (self.current_rps * ERROR_BACKOFF_GENTLE, "backoff_gentle")
        } else {
            (self.current_rps * ERROR_BACKOFF_HARD, "backoff_hard")
        };

        // 8. Take the most conservative (minimum) of all constraints.
        let unconstrained = latency_rate.min(error_rate);
        let binding = if latency_rate <= error_rate && latency_rate <= time_ceiling {
            "latency"
        } else if error_rate <= time_ceiling {
            "error_rate"
        } else {
            "ceiling"
        };
        let mut new_rps = unconstrained.min(time_ceiling);

        // 9. Apply known-good floor: don't drop below 80% of best recent rate.
        self.update_known_good();
        let floor = (self.max_recent_rate * KNOWN_GOOD_FLOOR_FRACTION)
            .max(self.config.min_rps);
        let floored = new_rps < floor;
        new_rps = new_rps.max(floor);

        // 10. Clamp to bounds.
        new_rps = new_rps.clamp(self.config.min_rps, self.config.max_rps);

        // 11. On backoff: record failure rate and engage cooldown.
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

            // Start recovery timer (if not already tracking one).
            if self.ticks_since_backoff.is_none() {
                self.ticks_since_backoff = Some(0);
            }

            // Engage cooldown: max(minimum, 1.5× observed recovery time in ticks).
            let cooldown_ticks = if self.recovery_time_ema_ticks > 0.0 {
                let estimated =
                    (self.recovery_time_ema_ticks * COOLDOWN_RECOVERY_MULTIPLIER).ceil() as u32;
                estimated.max(COOLDOWN_MIN_TICKS)
            } else {
                COOLDOWN_MIN_TICKS
            };
            self.cooldown_remaining = cooldown_ticks;

            tracing::info!(
                failure_rate = format!("{:.0}", self.last_failure_rate),
                cooldown_ticks,
                backoff_count = self.backoff_count,
                recovery_ema_ticks = format!("{:.1}", self.recovery_time_ema_ticks),
                "ramp: backoff cooldown engaged"
            );
        }

        // Track whether this tick was an increase (for self-caused detection).
        if new_rps > self.current_rps {
            self.ticks_since_increase = 0;
        } else {
            self.ticks_since_increase = self.ticks_since_increase.saturating_add(1);
        }

        tracing::info!(
            previous_rps = format!("{:.1}", self.current_rps),
            new_rps = format!("{:.1}", new_rps),
            raw_p99_ms = format!("{:.2}", raw_p99),
            smoothed_p99_ms = format!("{:.2}", smoothed_p99),
            target_p99_ms = format!("{:.2}", self.target_p99_ms),
            latency_ratio = format!("{:.2}", latency_ratio),
            latency_action,
            latency_streak = self.latency_violation_streak,
            error_rate_pct = format!("{:.4}", error_pct),
            error_action,
            error_streak = self.error_violation_streak,
            self_caused,
            ticks_since_increase = self.ticks_since_increase,
            binding,
            ceiling = format!("{:.0}", time_ceiling),
            floor = format!("{:.0}", floor),
            floored,
            known_good_rps = format!("{:.0}", self.max_recent_rate),
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
                if self.cooldown_remaining > 0 { 1.0 } else { 0.0 },
            ),
            (
                "netanvil_ramp_recovery_ema_ticks",
                self.recovery_time_ema_ticks,
            ),
        ]
    }

    fn controller_info(&self) -> ControllerInfo {
        let phase = match self.state {
            RampState::Warmup => "warmup",
            RampState::Ramping => "ramping",
        };

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
                "latency_violation_streak": self.latency_violation_streak,
                "error_violation_streak": self.error_violation_streak,
                "max_recent_rate": self.max_recent_rate,
                "last_failure_rate": self.last_failure_rate,
                "backoff_count": self.backoff_count,
                "recovery_ema_ticks": self.recovery_time_ema_ticks,
                "in_cooldown": self.cooldown_remaining > 0,
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
