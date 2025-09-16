use std::time::{Duration, Instant};

use netanvil_types::{PidGains, RateConfig, RateController};

use super::{
    AutotuneParams, AutotuningPidController, CompositePidController, PidGainValues,
    PidRateController, RampConfig, RampRateController, StaticRateController, StepRateController,
};

/// Build a `Box<dyn RateController>` from a [`RateConfig`].
///
/// This is the single source of truth for `RateConfig` -> `RateController` mapping.
/// Used by both the local engine (`run_test_core`) and the distributed leader.
pub fn build_rate_controller(
    rate: &RateConfig,
    control_interval: Duration,
    start_time: Instant,
) -> Box<dyn RateController> {
    match rate {
        RateConfig::Static { rps } => Box::new(StaticRateController::new(*rps)),
        RateConfig::Step { steps } => Box::new(StepRateController::with_start_time(
            steps.clone(),
            start_time,
        )),
        RateConfig::Pid {
            initial_rps,
            target,
        } => match &target.gains {
            PidGains::Manual { kp, ki, kd } => Box::new(PidRateController::new(
                target.metric.clone(),
                target.value,
                *initial_rps,
                target.min_rps,
                target.max_rps,
                PidGainValues {
                    kp: *kp,
                    ki: *ki,
                    kd: *kd,
                },
            )),
            PidGains::Auto {
                autotune_duration,
                smoothing,
            } => Box::new(AutotuningPidController::new(
                target.metric.clone(),
                target.value,
                *initial_rps,
                target.min_rps,
                target.max_rps,
                AutotuneParams {
                    autotune_duration: *autotune_duration,
                    smoothing: *smoothing,
                    control_interval,
                },
            )),
        },
        RateConfig::CompositePid {
            initial_rps,
            constraints,
            min_rps,
            max_rps,
        } => Box::new(CompositePidController::new(
            constraints,
            *initial_rps,
            *min_rps,
            *max_rps,
            control_interval,
        )),
        RateConfig::Ramp {
            warmup_rps,
            warmup_duration,
            latency_multiplier,
            max_error_rate,
            min_rps,
            max_rps,
        } => Box::new(RampRateController::new(RampConfig {
            warmup_rps: *warmup_rps,
            warmup_duration: *warmup_duration,
            latency_multiplier: *latency_multiplier,
            max_error_rate: *max_error_rate,
            min_rps: *min_rps,
            max_rps: *max_rps,
            control_interval,
        })),
    }
}
