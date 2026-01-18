//! Single-metric PID controller with automatic gain tuning.
//!
//! Runs a short exploration phase to characterize the system's response,
//! then uses gain-scheduled PID control with the computed gains.

use std::time::Duration;

use netanvil_types::{
    ControllerInfo, ControllerType, MetricsSummary, RateController, RateDecision, TargetMetric,
};

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

    /// Abort exploration, switching to active PID with conservative gains.
    /// Returns true if exploration was actually running, false if already active.
    pub fn abort_exploration(&mut self) -> bool {
        match &self.phase {
            Phase::Exploring(_) => {
                self.phase = Phase::Active {
                    gains: CONSERVATIVE_GAINS,
                    state: PidState::new(self.smoothing),
                };
                true
            }
            Phase::Active { .. } => false,
        }
    }

    pub fn set_target_value(&mut self, value: f64) {
        self.target_value = value;
    }

    pub fn set_gains(&mut self, kp: f64, ki: f64, kd: f64) -> Result<(), String> {
        match &mut self.phase {
            Phase::Active { gains, .. } => {
                gains.kp = kp;
                gains.ki = ki;
                gains.kd = kd;
                Ok(())
            }
            Phase::Exploring(_) => Err("cannot set gains during exploration".into()),
        }
    }

    pub fn reset_integrator(&mut self) -> Result<(), String> {
        match &mut self.phase {
            Phase::Active { state, .. } => {
                state.reset();
                Ok(())
            }
            Phase::Exploring(_) => Err("cannot reset integrator during exploration".into()),
        }
    }

    pub fn set_min_rps(&mut self, min_rps: f64) {
        self.min_rps = min_rps;
        if self.min_rps > self.max_rps {
            self.min_rps = self.max_rps;
        }
    }
}

impl RateController for AutotuningPidController {
    fn update(&mut self, summary: &MetricsSummary) -> RateDecision {
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
                }
            }

            Phase::Active { gains, state } => {
                if summary.total_requests < 5 {
                    return RateDecision {
                        target_rps: self.current_rps,
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

    fn set_max_rps(&mut self, max_rps: f64) {
        self.max_rps = max_rps.max(self.min_rps);
        if self.current_rps > self.max_rps {
            self.current_rps = self.max_rps;
        }
    }

    fn controller_info(&self) -> ControllerInfo {
        let is_exploring = matches!(self.phase, Phase::Exploring(_));
        let mut editable = vec!["set_max_rps".into(), "set_min_rps".into()];
        if is_exploring {
            editable.insert(0, "abort_exploration".into());
        } else {
            editable.extend_from_slice(&[
                "set_target_value".into(),
                "set_gains".into(),
                "reset_integrator".into(),
            ]);
        }

        let params = match &self.phase {
            Phase::Exploring(manager) => {
                let progress = manager.exploration_progress();
                serde_json::json!({
                    "phase": "exploring",
                    "target_metric": format!("{:?}", self.target_metric),
                    "target_value": self.target_value,
                    "min_rps": self.min_rps,
                    "max_rps": self.max_rps,
                    "exploration_progress": progress,
                })
            }
            Phase::Active { gains, state } => {
                serde_json::json!({
                    "phase": "active",
                    "target_metric": format!("{:?}", self.target_metric),
                    "target_value": self.target_value,
                    "min_rps": self.min_rps,
                    "max_rps": self.max_rps,
                    "gains": {
                        "kp": gains.kp,
                        "ki": gains.ki,
                        "kd": gains.kd,
                    },
                    "integrator": state.integral,
                    "last_error": state.last_error,
                })
            }
        };

        ControllerInfo {
            controller_type: ControllerType::PidAutotune,
            current_rps: self.current_rps,
            editable_actions: editable,
            params,
        }
    }

    fn apply_update(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match action {
            "abort_exploration" => {
                let was_exploring = self.abort_exploration();
                Ok(serde_json::json!({
                    "ok": true,
                    "was_exploring": was_exploring,
                    "gains_used": if was_exploring { "conservative" } else { "unchanged" },
                }))
            }
            "set_target_value" => {
                let value = params
                    .get("value")
                    .and_then(|v| v.as_f64())
                    .ok_or("missing 'value' field")?;
                let old = self.target_value;
                self.set_target_value(value);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_target_value": old,
                    "new_target_value": value,
                    "note": "integrator preserved",
                }))
            }
            "set_gains" => {
                let kp = params
                    .get("kp")
                    .and_then(|v| v.as_f64())
                    .ok_or("missing 'kp' field")?;
                let ki = params
                    .get("ki")
                    .and_then(|v| v.as_f64())
                    .ok_or("missing 'ki' field")?;
                let kd = params
                    .get("kd")
                    .and_then(|v| v.as_f64())
                    .ok_or("missing 'kd' field")?;
                self.set_gains(kp, ki, kd)?;
                Ok(serde_json::json!({
                    "ok": true,
                    "new_gains": {"kp": kp, "ki": ki, "kd": kd},
                }))
            }
            "reset_integrator" => {
                self.reset_integrator()?;
                Ok(serde_json::json!({
                    "ok": true,
                    "note": "PID integrator and derivative state reset",
                }))
            }
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
                self.set_min_rps(min);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_min_rps": old,
                    "new_min_rps": self.min_rps,
                }))
            }
            _ => Err(format!(
                "action '{}' is not valid for controller type 'pid_autotune'",
                action
            )),
        }
    }
}
