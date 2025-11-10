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
        Self {
            config,
            state: RampState::Warmup,
            warmup_start: Instant::now(),
            warmup_p99_samples: Vec::with_capacity(64),
            current_rps,
            inner_pid: None,
            baseline_p99_ms: 0.0,
            target_p99_ms: 0.0,
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
                    next_update_interval: Duration::from_millis(100),
                }
            }

            RampState::Ramping => {
                // When errors exceed the threshold, ratchet the ceiling down
                // by 1% and drop the current rate by 20%.  The PID will climb
                // back up from the lower rate, but can never exceed the
                // (slightly lower) ceiling.  Each successive error hit shaves
                // another 1% off the ceiling until the PID settles in a
                // stable band just below the error cliff.
                let error_pct = summary.error_rate * 100.0;
                if error_pct > self.config.max_error_rate && summary.total_requests > 10 {
                    let new_ceiling = (self.current_rps * 0.99).max(self.config.min_rps);
                    self.current_rps = (self.current_rps * 0.80).max(self.config.min_rps);

                    if let Some(ref mut pid) = self.inner_pid {
                        pid.set_max_rps(new_ceiling);
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
                        next_update_interval: Duration::from_millis(100),
                    };
                }

                // Delegate to the autotuning PID
                let decision = self.inner_pid.as_mut().unwrap().update(summary);
                self.current_rps = decision.target_rps;
                decision
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
}
