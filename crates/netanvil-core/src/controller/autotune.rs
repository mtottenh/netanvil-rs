//! Autotuning logic for PID rate controllers.
//!
//! Provides the exploration state machine and Cohen-Coon gain computation.
//! Used by both `AutotuningPidController` (single-metric) and
//! `CompositePidController` (multi-constraint).
//!
//! PID math (gain scheduling, PID state, step computation) lives in
//! [`super::pid_math`] and is re-exported here for backwards compatibility.

use std::time::Duration;

use netanvil_types::{MetricsSummary, TargetMetric};

// Re-export PID math so existing `use super::autotune::*` imports keep working.
pub use super::pid_math::{
    extract_metric, gain_schedule, pid_compute, pid_step_fixed, pid_step_with_scheduling,
    pid_update_state, GainMultipliers, PidOutput, PidState, PidStepInput,
};

// ---------------------------------------------------------------------------
// Exploration state machine
// ---------------------------------------------------------------------------

/// Per-metric data collected during the exploration phase.
#[derive(Debug, Clone)]
pub struct MetricExploration {
    pub metric: TargetMetric,
    pub target_value: f64,
    pub baseline_measurements: Vec<f64>,
    pub step_measurements: Vec<(u32, f64)>, // (tick_offset, value)
    pub dead_time_detected: Option<u32>,    // tick when metric first moved
}

impl MetricExploration {
    pub fn new(metric: TargetMetric, target_value: f64) -> Self {
        Self {
            metric,
            target_value,
            baseline_measurements: Vec::new(),
            step_measurements: Vec::new(),
            dead_time_detected: None,
        }
    }

    pub fn baseline_mean(&self) -> f64 {
        if self.baseline_measurements.is_empty() {
            return 0.0;
        }
        self.baseline_measurements.iter().sum::<f64>() / self.baseline_measurements.len() as f64
    }

    pub fn noise_threshold(&self) -> f64 {
        let mean = self.baseline_mean();
        (mean * 0.05).max(1.0)
    }
}

/// State of the exploration phase.
#[derive(Debug)]
pub enum ExplorationPhase {
    /// Measuring baseline metric at reduced rate.
    Baseline { start_tick: u32 },
    /// Applied step, waiting for metric response.
    StepResponse {
        step_tick: u32,
        baseline_rate: f64,
        step_rate: f64,
    },
    /// Exploration complete — gains computed.
    Complete,
    /// Exploration failed — use conservative defaults.
    Failed,
}

/// Result of computing gains from exploration data.
#[derive(Debug, Clone, Copy)]
pub struct ComputedGains {
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
    pub dead_time_ticks: u32,
}

/// Manages the exploration phase for one or more metrics.
pub struct ExplorationManager {
    pub phase: ExplorationPhase,
    pub metrics: Vec<MetricExploration>,
    pub tick_count: u32,
    pub control_interval: Duration,
    pub autotune_duration: Duration,
    pub initial_rps: f64,
}

impl ExplorationManager {
    pub fn new(
        metrics: Vec<MetricExploration>,
        initial_rps: f64,
        autotune_duration: Duration,
        control_interval: Duration,
    ) -> Self {
        Self {
            phase: ExplorationPhase::Baseline { start_tick: 0 },
            metrics,
            tick_count: 0,
            control_interval,
            autotune_duration,
            initial_rps,
        }
    }

    /// Returns exploration progress as a fraction [0.0, 1.0].
    pub fn exploration_progress(&self) -> f64 {
        let total_ticks = if self.control_interval.as_secs_f64() > 0.0 {
            (self.autotune_duration.as_secs_f64() / self.control_interval.as_secs_f64()) as u32
        } else {
            1
        };
        if total_ticks == 0 {
            return 1.0;
        }
        (self.tick_count as f64 / total_ticks as f64).min(1.0)
    }

    /// Returns the rate to use this tick, or None if exploration is done.
    pub fn tick(&mut self, summary: &MetricsSummary) -> Option<f64> {
        self.tick_count += 1;

        // Read current values for all metrics
        let values: Vec<f64> = self
            .metrics
            .iter()
            .map(|m| extract_metric(&m.metric, summary))
            .collect();

        match &self.phase {
            ExplorationPhase::Baseline { start_tick } => {
                let start_tick = *start_tick;
                // Collect baseline at 50% of initial rate
                for (i, &val) in values.iter().enumerate() {
                    self.metrics[i].baseline_measurements.push(val);
                }

                let baseline_ticks = self.baseline_duration_ticks();
                if self.tick_count - start_tick >= baseline_ticks {
                    let baseline_rate = self.initial_rps * 0.5;
                    let step_rate = self.initial_rps;

                    tracing::info!(
                        baseline_rate,
                        step_rate,
                        tick = self.tick_count,
                        "autotuning: baseline complete, applying step"
                    );

                    self.phase = ExplorationPhase::StepResponse {
                        step_tick: self.tick_count,
                        baseline_rate,
                        step_rate,
                    };
                    Some(step_rate)
                } else {
                    Some(self.initial_rps * 0.5)
                }
            }

            ExplorationPhase::StepResponse {
                step_tick,
                baseline_rate: _,
                step_rate: _,
            } => {
                let step_tick = *step_tick;

                for (i, &val) in values.iter().enumerate() {
                    let tick_offset = self.tick_count - step_tick;
                    self.metrics[i].step_measurements.push((tick_offset, val));

                    // Detect dead time: metric moves beyond noise threshold from baseline
                    if self.metrics[i].dead_time_detected.is_none() {
                        let baseline = self.metrics[i].baseline_mean();
                        let threshold = self.metrics[i].noise_threshold();
                        if (val - baseline).abs() > threshold {
                            self.metrics[i].dead_time_detected = Some(tick_offset);
                        }
                    }
                }

                let max_step_ticks = self.step_duration_ticks();
                let elapsed = self.tick_count - step_tick;

                // Check if all metrics have settled
                let settled = self.metrics.iter().all(|m| {
                    m.step_measurements.len() >= 3 && {
                        let recent: Vec<f64> = m
                            .step_measurements
                            .iter()
                            .rev()
                            .take(3)
                            .map(|(_, v)| *v)
                            .collect();
                        let spread = recent.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
                            - recent.iter().cloned().fold(f64::INFINITY, f64::min);
                        spread < m.noise_threshold()
                    }
                });

                if elapsed >= max_step_ticks || settled {
                    self.phase = ExplorationPhase::Complete;
                    None
                } else {
                    Some(self.initial_rps)
                }
            }

            ExplorationPhase::Complete | ExplorationPhase::Failed => None,
        }
    }

    /// Compute gains for a specific metric after exploration completes.
    pub fn compute_gains_for(&self, metric_idx: usize) -> ComputedGains {
        let m = &self.metrics[metric_idx];
        let baseline_value = m.baseline_mean();
        let baseline_rate = self.initial_rps * 0.5;
        let step_rate = self.initial_rps;

        let final_value = m
            .step_measurements
            .last()
            .map(|(_, v)| *v)
            .unwrap_or(baseline_value);

        let delta_metric = final_value - baseline_value;
        let delta_rate = step_rate - baseline_rate;

        // System gain K = delta_metric / delta_rate
        let system_gain = if delta_rate.abs() > 1.0 {
            delta_metric / delta_rate
        } else {
            0.0
        };

        if system_gain.abs() < 1e-9 {
            tracing::warn!(
                metric = ?m.metric,
                delta_metric,
                delta_rate,
                "autotuning: metric did not respond to step, using conservative defaults"
            );
            return CONSERVATIVE_GAINS;
        }

        // Dead time in ticks
        let dead_time_ticks = m.dead_time_detected.unwrap_or(3).max(1);

        // Time constant: tick where metric reaches 63.2% of final change
        let target_63pct = baseline_value + 0.632 * delta_metric;
        let time_constant_ticks = m
            .step_measurements
            .iter()
            .find(|(_, v)| {
                if delta_metric > 0.0 {
                    *v >= target_63pct
                } else {
                    *v <= target_63pct
                }
            })
            .map(|(tick, _)| *tick)
            .unwrap_or(dead_time_ticks * 2)
            .max(1);

        compute_cohen_coon_gains(m.target_value, dead_time_ticks, time_constant_ticks)
    }

    fn baseline_duration_ticks(&self) -> u32 {
        let ticks = 500u64 / self.control_interval.as_millis().max(1) as u64;
        (ticks as u32).max(3)
    }

    fn step_duration_ticks(&self) -> u32 {
        let baseline_ms = 500u64;
        let total_ms = self.autotune_duration.as_millis() as u64;
        let step_ms = total_ms.saturating_sub(baseline_ms).max(1000);
        let ticks = step_ms / self.control_interval.as_millis().max(1) as u64;
        (ticks as u32).max(5)
    }
}

// ---------------------------------------------------------------------------
// Cohen-Coon gain computation
// ---------------------------------------------------------------------------

/// Conservative fallback gains — slow but stable for any system.
pub const CONSERVATIVE_GAINS: ComputedGains = ComputedGains {
    kp: 0.02,
    ki: 0.001,
    kd: 0.01,
    dead_time_ticks: 3,
};

/// Compute PID gains from dead time and time constant using adapted Cohen-Coon.
///
/// Gains are normalized for our PID formulation where:
///   output = kp * error + ki * integral + kd * derivative
///   adjustment = (output * 0.05).clamp(-0.20, 0.20)
///   new_rps = current_rps * (1 + adjustment)
pub fn compute_cohen_coon_gains(
    target_value: f64,
    dead_time_ticks: u32,
    time_constant_ticks: u32,
) -> ComputedGains {
    let r = (dead_time_ticks as f64 / time_constant_ticks as f64).clamp(0.1, 2.0);
    let typical_error = (target_value * 0.5).max(1.0);

    // Base Kp: normalized so kp * typical_error ≈ 2-3 (producing 10-15% adjustment)
    let base_kp = (1.5 / typical_error) * (1.0 + 0.5 / r);

    // Ki from Cohen-Coon integral time
    let ti_denom = 1.0 - 0.39 * r;
    let ti = if ti_denom.abs() > 0.01 {
        (dead_time_ticks as f64 * (2.5 - 2.0 * r) / ti_denom).max(dead_time_ticks as f64)
    } else {
        dead_time_ticks as f64 * 5.0 // fallback: slow integral
    };
    let base_ki = base_kp / ti;

    // Kd from Cohen-Coon derivative time (clamp to >= 0)
    let td_denom = 1.0 - 0.81 * r;
    let td = if td_denom.abs() > 0.01 {
        dead_time_ticks as f64 * (0.37 - r) / td_denom
    } else {
        0.0
    };
    let base_kd = if td > 0.0 { base_kp * td } else { 0.0 };

    tracing::info!(
        dead_time_ticks,
        time_constant_ticks,
        r,
        base_kp,
        base_ki,
        base_kd,
        "autotuning: computed Cohen-Coon gains"
    );

    ComputedGains {
        kp: base_kp,
        ki: base_ki,
        kd: base_kd,
        dead_time_ticks,
    }
}
