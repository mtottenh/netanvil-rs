//! Single-metric PID controller with automatic gain tuning.
//!
//! Runs a short exploration phase to characterize the system's response,
//! then uses gain-scheduled PID control with the computed gains.

use std::time::Duration;

use netanvil_types::{MetricsSummary, RateController, RateDecision, TargetMetric};

use super::autotune::{
    self, ComputedGains, ExplorationManager, ExplorationPhase, MetricExploration, PidState,
    PidStepInput, CONSERVATIVE_GAINS,
};

/// Autotuning state for the single-metric controller.
enum Phase {
    /// Running exploration to characterize the system.
    Exploring(ExplorationManager),
    /// Exploration complete, running gain-scheduled PID.
    Active {
        gains: ComputedGains,
        state: PidState,
    },
}

/// Parameters for the autotuning exploration phase.
pub struct AutotuneParams {
    pub autotune_duration: Duration,
    pub smoothing: f64,
    pub control_interval: Duration,
}

/// PID rate controller that automatically determines its own gains.
///
/// Phase 1 (exploration): Applies a controlled step in rate and measures
/// the metric's response to compute system gain, dead time, and time
/// constant. Gains are derived via adapted Cohen-Coon formulae.
///
/// Phase 2 (control): Gain-scheduled PID where Kp/Ki/Kd multipliers
/// adapt based on distance from the target (5 operating regions from
/// "ramp-up" through "critical").
pub struct AutotuningPidController {
    target_metric: TargetMetric,
    target_value: f64,
    current_rps: f64,
    min_rps: f64,
    max_rps: f64,
    smoothing: f64,
    phase: Phase,
}

impl AutotuningPidController {
    pub fn new(
        target_metric: TargetMetric,
        target_value: f64,
        initial_rps: f64,
        min_rps: f64,
        max_rps: f64,
        params: AutotuneParams,
    ) -> Self {
        let exploration = MetricExploration::new(target_metric.clone(), target_value);
        let manager = ExplorationManager::new(
            vec![exploration],
            initial_rps,
            params.autotune_duration,
            params.control_interval,
        );

        Self {
            target_metric,
            target_value,
            current_rps: initial_rps * 0.5, // start at baseline rate
            min_rps,
            max_rps,
            smoothing: params.smoothing,
            phase: Phase::Exploring(manager),
        }
    }
}

impl RateController for AutotuningPidController {
    fn update(&mut self, summary: &MetricsSummary) -> RateDecision {
        let interval = Duration::from_millis(100);

        match &mut self.phase {
            Phase::Exploring(manager) => {
                if let Some(rate) = manager.tick(summary) {
                    self.current_rps = rate.clamp(self.min_rps, self.max_rps);
                } else {
                    // Exploration done — compute gains and transition
                    let gains = match manager.phase {
                        ExplorationPhase::Complete => manager.compute_gains_for(0),
                        _ => CONSERVATIVE_GAINS,
                    };

                    tracing::info!(
                        kp = gains.kp,
                        ki = gains.ki,
                        kd = gains.kd,
                        dead_time_ticks = gains.dead_time_ticks,
                        "autotuning: exploration complete, switching to gain-scheduled PID"
                    );

                    // Do a first PID step immediately
                    let current_value = autotune::extract_metric(&self.target_metric, summary);
                    let mut state = PidState::new(self.smoothing);
                    let smoothed = state.smooth(current_value);
                    self.current_rps = autotune::pid_step_with_scheduling(
                        &PidStepInput {
                            current_value: smoothed,
                            target_value: self.target_value,
                            current_rps: self.current_rps,
                            min_rps: self.min_rps,
                            max_rps: self.max_rps,
                            kp: gains.kp,
                            ki: gains.ki,
                            kd: gains.kd,
                        },
                        &mut state,
                    );
                    self.phase = Phase::Active { gains, state };
                }

                RateDecision {
                    target_rps: self.current_rps,
                    next_update_interval: interval,
                }
            }

            Phase::Active { gains, state } => {
                if summary.total_requests < 5 {
                    return RateDecision {
                        target_rps: self.current_rps,
                        next_update_interval: interval,
                    };
                }

                let raw_value = autotune::extract_metric(&self.target_metric, summary);
                let smoothed = state.smooth(raw_value);

                self.current_rps = autotune::pid_step_with_scheduling(
                    &PidStepInput {
                        current_value: smoothed,
                        target_value: self.target_value,
                        current_rps: self.current_rps,
                        min_rps: self.min_rps,
                        max_rps: self.max_rps,
                        kp: gains.kp,
                        ki: gains.ki,
                        kd: gains.kd,
                    },
                    state,
                );

                RateDecision {
                    target_rps: self.current_rps,
                    next_update_interval: interval,
                }
            }
        }
    }

    fn current_rate(&self) -> f64 {
        self.current_rps
    }

    fn set_rate(&mut self, rps: f64) {
        self.current_rps = rps.clamp(self.min_rps, self.max_rps);
        // External rate override aborts exploration
        match &mut self.phase {
            Phase::Exploring(_) => {
                tracing::info!(
                    rps,
                    "autotuning: external set_rate during exploration, falling back to conservative gains"
                );
                self.phase = Phase::Active {
                    gains: CONSERVATIVE_GAINS,
                    state: PidState::new(self.smoothing),
                };
            }
            Phase::Active { state, .. } => {
                state.reset();
            }
        }
    }
}
