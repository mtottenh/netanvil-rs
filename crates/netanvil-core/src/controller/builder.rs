use std::time::{Duration, Instant};

use netanvil_types::{PidGains, RateConfig, RateController};

use super::{
    AutotuneParams, AutotuningPidController, CompositePidController, PidGainValues,
    PidRateController, RampConfig, RampRateController, SlowStart, StaticRateController,
    StepRateController,
};

/// Build a `Box<dyn RateController>` from a [`RateConfig`].
///
/// This is the single source of truth for `RateConfig` -> `RateController` mapping.
/// Used by both the local engine (`run_test_core`) and the distributed leader.
///
/// PID controllers are wrapped in `SlowStart<C>` for gradual rate ramp-up
/// and per-tick slew limiting. Ramp controllers manage their own progressive
/// ceiling, slew cap, and error-ratcheted ceiling internally. Static and step
/// controllers are not wrapped (users expect immediate rate changes).
pub fn build_rate_controller(
    rate: &RateConfig,
    control_interval: Duration,
    start_time: Instant,
    test_duration: Duration,
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
            PidGains::Manual { kp, ki, kd } => {
                let pid = PidRateController::new(
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
                );
                Box::new(SlowStart::new(
                    pid,
                    *initial_rps,
                    target.max_rps,
                    test_duration,
                    control_interval,
                ))
            }
            PidGains::Auto {
                autotune_duration,
                smoothing,
            } => {
                let pid = AutotuningPidController::new(
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
                );
                Box::new(SlowStart::new(
                    pid,
                    *initial_rps,
                    target.max_rps,
                    test_duration,
                    control_interval,
                ))
            }
        },
        RateConfig::CompositePid {
            initial_rps,
            constraints,
            min_rps,
            max_rps,
        } => {
            let pid = CompositePidController::new(
                constraints,
                *initial_rps,
                *min_rps,
                *max_rps,
                control_interval,
            );
            Box::new(SlowStart::new(
                pid,
                *initial_rps,
                *max_rps,
                test_duration,
                control_interval,
            ))
        }
        RateConfig::Ramp {
            warmup_rps,
            warmup_duration,
            latency_multiplier,
            max_error_rate,
            min_rps,
            max_rps,
        } => {
            // Ramp controller manages its own progressive ceiling, slew cap,
            // and error-ratcheted ceiling internally — no SlowStart wrapper.
            Box::new(RampRateController::new(RampConfig {
                warmup_rps: *warmup_rps,
                warmup_duration: *warmup_duration,
                latency_multiplier: *latency_multiplier,
                max_error_rate: *max_error_rate,
                min_rps: *min_rps,
                max_rps: *max_rps,
                control_interval,
                test_duration,
            }))
        }
    }
}
