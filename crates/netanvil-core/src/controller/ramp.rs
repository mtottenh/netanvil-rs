//! Adaptive ramp rate controller.
//!
//! Phase 1 (warmup): Runs at a low fixed RPS to learn the baseline p99 latency.
//! Phase 2 (ramp): Creates an autotuning PID controller targeting
//! `baseline_p99 × latency_multiplier`. The PID ramps up naturally when
//! latency is below target and backs off when above.
//!
//! The result is "find the maximum RPS where latency stays within N× of normal"
//! without the user specifying an absolute latency target.

use std::time::{Duration, Instant};

use netanvil_types::{MetricsSummary, RateDecision, TargetMetric};

use super::pid_autotune::{AutotuneParams, AutotuningPidController};

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
}

pub struct RampRateController {
    config: RampConfig,
    state: RampState,
    warmup_start: Instant,
    warmup_p99_samples: Vec<f64>,
    current_rps: f64,
    inner_pid: Option<AutotuningPidController>,
    /// Learned baseline p99 in milliseconds.
    baseline_p99_ms: f64,
    /// Computed target p99 in milliseconds.
    target_p99_ms: f64,
    /// Error-ratcheted ceiling. Once errors are detected, this persists
    /// across ticks and cannot be raised by external `set_max_rps` calls.
    ratcheted_ceiling: Option<f64>,
    /// Max per-tick rate increase (slew cap). Derived from the ramp slope.
    max_increase_per_tick: f64,
    /// When the ramping phase started (for progressive ceiling computation).
    ramp_start_time: Option<Instant>,
    /// Last logged ceiling ramp milestone (0, 25, 50, 75, 100).
    last_ceiling_milestone: u8,
    /// Last computed time-based ceiling (for Prometheus export).
    last_time_ceiling: f64,
    /// Last computed effective ceiling (for Prometheus export).
    last_effective_ceiling: f64,
    /// Whether slew cap fired on the last tick (for Prometheus export).
    last_slew_capped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RampState {
    /// Collecting baseline latency at low RPS.
    Warmup,
    /// PID-driven rate adjustment.
    Ramping,
}

impl RampRateController {
    pub fn new(config: RampConfig) -> Self {
        let current_rps = config.warmup_rps;
        let ramp_duration_secs = (config.test_duration / 2).as_secs_f64();
        let max_increase_per_tick = if ramp_duration_secs > 0.0 {
            (config.max_rps - config.warmup_rps) * config.control_interval.as_secs_f64()
                / ramp_duration_secs
        } else {
            f64::INFINITY
        };
        Self {
            config,
            state: RampState::Warmup,
            warmup_start: Instant::now(),
            warmup_p99_samples: Vec::with_capacity(64),
            current_rps,
            inner_pid: None,
            baseline_p99_ms: 0.0,
            target_p99_ms: 0.0,
            ratcheted_ceiling: None,
            max_increase_per_tick,
            ramp_start_time: None,
            last_ceiling_milestone: 0,
            last_time_ceiling: 0.0,
            last_effective_ceiling: 0.0,
            last_slew_capped: false,
        }
    }

    fn transition_to_ramping(&mut self) {
        // Compute baseline p99 from warmup samples (use median for robustness)
        self.warmup_p99_samples
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = self.warmup_p99_samples.len();
        self.baseline_p99_ms = if n == 0 {
            1.0 // fallback: 1ms if no samples
        } else {
            self.warmup_p99_samples[n / 2]
        };

        self.target_p99_ms = self.baseline_p99_ms * self.config.latency_multiplier;

        tracing::info!(
            baseline_p99_ms = format!("{:.2}", self.baseline_p99_ms),
            target_p99_ms = format!("{:.2}", self.target_p99_ms),
            multiplier = self.config.latency_multiplier,
            warmup_samples = n,
            "ramp warmup complete, transitioning to PID control"
        );

        // Create autotuning PID targeting the learned threshold.
        // Start ramping from 2× warmup RPS so the autotuner has room to explore.
        let initial_ramp_rps = (self.config.warmup_rps * 2.0).min(self.config.max_rps);

        self.inner_pid = Some(AutotuningPidController::new(
            TargetMetric::LatencyP99,
            self.target_p99_ms,
            initial_ramp_rps,
            self.config.min_rps,
            self.config.max_rps,
            AutotuneParams {
                autotune_duration: Duration::from_secs(5),
                smoothing: 0.3,
                control_interval: self.config.control_interval,
            },
        ));

        self.ramp_start_time = Some(Instant::now());
        self.state = RampState::Ramping;
    }
}

impl netanvil_types::RateController for RampRateController {
    fn update(&mut self, summary: &MetricsSummary) -> RateDecision {
        match self.state {
            RampState::Warmup => {
                // Collect p99 latency samples (skip zero-request windows)
                if summary.total_requests > 0 {
                    let p99_ms = summary.latency_p99_ns as f64 / 1_000_000.0;
                    if p99_ms > 0.0 {
                        self.warmup_p99_samples.push(p99_ms);
                    }
                }

                // Check if warmup is complete
                let elapsed = self.warmup_start.elapsed();
                if elapsed >= self.config.warmup_duration && self.warmup_p99_samples.len() >= 3 {
                    self.transition_to_ramping();
                    // Immediately delegate to the new PID
                    return self.inner_pid.as_mut().unwrap().update(summary);
                }

                // Still warming up — hold at warmup RPS
                RateDecision {
                    target_rps: self.config.warmup_rps,
                }
            }

            RampState::Ramping => {
                // 1. Progressive ceiling: ramp from warmup_rps to max_rps
                //    over the first half of the test duration.
                let elapsed = self
                    .ramp_start_time
                    .expect("ramp_start_time set in transition_to_ramping")
                    .elapsed();
                let ramp_duration = self.config.test_duration / 2;
                let progress = if ramp_duration.as_secs_f64() > 0.0 {
                    (elapsed.as_secs_f64() / ramp_duration.as_secs_f64()).min(1.0)
                } else {
                    1.0
                };
                let time_ceiling = self.config.warmup_rps
                    + progress * (self.config.max_rps - self.config.warmup_rps);

                // 2. Effective ceiling = min(time ramp, error ratchet).
                //    Single owner — no external wrapper can overwrite.
                let effective_ceiling = match self.ratcheted_ceiling {
                    Some(rc) => time_ceiling.min(rc),
                    None => time_ceiling,
                };
                self.last_time_ceiling = time_ceiling;
                self.last_effective_ceiling = effective_ceiling;

                if let Some(ref mut pid) = self.inner_pid {
                    pid.set_max_rps(effective_ceiling);
                }

                // Log ceiling ramp milestones at 25% intervals.
                let pct = (progress * 100.0) as u8;
                let milestone = (pct / 25) * 25;
                if milestone > self.last_ceiling_milestone {
                    self.last_ceiling_milestone = milestone;
                    tracing::info!(
                        progress_pct = milestone,
                        time_ceiling = format!("{:.0}", time_ceiling),
                        effective_ceiling = format!("{:.0}", effective_ceiling),
                        ratcheted_ceiling = self.ratcheted_ceiling.map(|c| format!("{:.0}", c)),
                        "ramp ceiling milestone"
                    );
                }

                // 3. Error detection: ratchet ceiling and drop rate.
                let error_pct = summary.error_rate * 100.0;
                if error_pct > self.config.max_error_rate && summary.total_requests > 10 {
                    let new_ceiling = match self.ratcheted_ceiling {
                        // Successive hit: ratchet 5% below previous ceiling
                        Some(prev) => (prev * 0.95).max(self.config.min_rps),
                        // First hit: ceiling = rate where errors appeared
                        None => self.current_rps.max(self.config.min_rps),
                    };
                    self.ratcheted_ceiling = Some(new_ceiling);
                    self.current_rps = (self.current_rps * 0.80).max(self.config.min_rps);

                    if let Some(ref mut pid) = self.inner_pid {
                        pid.set_max_rps(new_ceiling);
                        pid.set_rate(self.current_rps);
                    }

                    tracing::info!(
                        error_rate_pct = format!("{:.1}", error_pct),
                        max_error_rate = self.config.max_error_rate,
                        ceiling_rps = format!("{:.0}", new_ceiling),
                        reduced_rps = format!("{:.0}", self.current_rps),
                        "error threshold hit, ratcheting ceiling down"
                    );
                    return RateDecision {
                        target_rps: self.current_rps,
                    };
                }

                // 4. Delegate to the autotuning PID
                let decision = self.inner_pid.as_mut().unwrap().update(summary);

                // 5. Slew cap: limit per-tick increases, leave decreases uncapped.
                let max_allowed = self.current_rps + self.max_increase_per_tick;
                self.last_slew_capped = decision.target_rps > max_allowed;
                if self.last_slew_capped {
                    tracing::info!(
                        pid_wanted = format!("{:.0}", decision.target_rps),
                        slew_allowed = format!("{:.0}", max_allowed),
                        "slew rate cap engaged"
                    );
                    if let Some(ref mut pid) = self.inner_pid {
                        pid.set_rate(max_allowed); // back-calculation
                    }
                    self.current_rps = max_allowed;
                } else {
                    self.current_rps = decision.target_rps;
                }

                tracing::debug!(
                    time_ceiling = format!("{:.0}", time_ceiling),
                    ratcheted_ceiling = self.ratcheted_ceiling.map(|c| format!("{:.0}", c)),
                    effective_ceiling = format!("{:.0}", effective_ceiling),
                    pid_target_rps = format!("{:.0}", decision.target_rps),
                    slew_capped = self.last_slew_capped,
                    current_rps = format!("{:.0}", self.current_rps),
                    "ramp controller tick"
                );

                RateDecision {
                    target_rps: self.current_rps,
                }
            }
        }
    }

    fn current_rate(&self) -> f64 {
        self.current_rps
    }

    fn set_rate(&mut self, rps: f64) {
        self.current_rps = rps;
        if let Some(ref mut pid) = self.inner_pid {
            pid.set_rate(rps);
        }
    }

    fn set_max_rps(&mut self, max_rps: f64) {
        self.config.max_rps = max_rps.max(self.config.min_rps);
        // Respect the error-ratcheted ceiling: external callers cannot
        // raise the PID's ceiling above the ratcheted value.
        let effective = match self.ratcheted_ceiling {
            Some(ceil) => self.config.max_rps.min(ceil),
            None => self.config.max_rps,
        };
        if let Some(ref mut pid) = self.inner_pid {
            pid.set_max_rps(effective);
        }
        if self.current_rps > effective {
            self.current_rps = effective;
        }
    }

    fn controller_state(&self) -> Vec<(&'static str, f64)> {
        if self.state != RampState::Ramping {
            return Vec::new();
        }
        let mut state = vec![
            ("netanvil_ramp_time_ceiling", self.last_time_ceiling),
            ("netanvil_ramp_effective_ceiling", self.last_effective_ceiling),
            (
                "netanvil_ramp_slew_capped",
                if self.last_slew_capped { 1.0 } else { 0.0 },
            ),
        ];
        if let Some(rc) = self.ratcheted_ceiling {
            state.push(("netanvil_ramp_ratcheted_ceiling", rc));
        }
        state
    }
}
