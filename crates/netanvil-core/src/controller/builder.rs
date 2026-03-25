use std::sync::Arc;
use std::time::{Duration, Instant};

use netanvil_types::{PidGains, RateConfig, RateController, TargetMetric};

use super::arbiter::{Arbiter, ArbiterConfig};
use super::clock::Clock;
use super::constraint::Constraint;
use super::pid_constraint::PidConstraint;
use super::smoothing::Smoother;
use super::threshold::ThresholdConstraint;
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
/// via progressive ceiling. Ramp controllers use AIMD (additive increase /
/// multiplicative decrease) with their own progressive ceiling. Static and
/// step controllers are not wrapped.
pub fn build_rate_controller(
    rate: &RateConfig,
    control_interval: Duration,
    start_time: Instant,
    test_duration: Duration,
    clock: Arc<dyn Clock>,
) -> Box<dyn RateController> {
    let controller: Box<dyn RateController> = match rate {
        RateConfig::Static { rps } => {
            tracing::info!(rps, "rate controller: static");
            Box::new(StaticRateController::new(*rps))
        }
        RateConfig::Step { steps } => {
            tracing::info!(num_steps = steps.len(), "rate controller: step");
            Box::new(StepRateController::with_start_time_and_clock(
                steps.clone(),
                start_time,
                clock,
            ))
        }
        RateConfig::Pid {
            initial_rps,
            target,
        } => match &target.gains {
            PidGains::Manual { kp, ki, kd } => {
                tracing::info!(
                    initial_rps,
                    metric = ?target.metric,
                    target_value = target.value,
                    min_rps = target.min_rps,
                    max_rps = target.max_rps,
                    kp, ki, kd,
                    "rate controller: PID (manual gains, slow-start wrapped)"
                );
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
                Box::new(SlowStart::new_with_clock(
                    pid,
                    *initial_rps,
                    target.max_rps,
                    test_duration,
                    control_interval,
                    clock,
                ))
            }
            PidGains::Auto {
                autotune_duration,
                smoothing,
            } => {
                tracing::info!(
                    initial_rps,
                    metric = ?target.metric,
                    target_value = target.value,
                    min_rps = target.min_rps,
                    max_rps = target.max_rps,
                    ?autotune_duration,
                    smoothing,
                    "rate controller: PID (auto-tune, slow-start wrapped)"
                );
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
                Box::new(SlowStart::new_with_clock(
                    pid,
                    *initial_rps,
                    target.max_rps,
                    test_duration,
                    control_interval,
                    clock,
                ))
            }
        },
        RateConfig::CompositePid {
            initial_rps,
            constraints,
            min_rps,
            max_rps,
        } => {
            tracing::info!(
                initial_rps,
                num_constraints = constraints.len(),
                min_rps,
                max_rps,
                "rate controller: composite PID (slow-start wrapped)"
            );
            let pid = CompositePidController::new(
                constraints,
                *initial_rps,
                *min_rps,
                *max_rps,
                control_interval,
            );
            Box::new(SlowStart::new_with_clock(
                pid,
                *initial_rps,
                *max_rps,
                test_duration,
                control_interval,
                clock,
            ))
        }
        RateConfig::Ramp {
            warmup_rps,
            warmup_duration,
            latency_multiplier,
            max_error_rate,
            min_rps,
            max_rps,
            external_constraints,
        } => {
            tracing::info!(
                warmup_rps,
                ?warmup_duration,
                latency_multiplier,
                max_error_rate,
                min_rps,
                max_rps,
                num_external_constraints = external_constraints.len(),
                "rate controller: ramp (AIMD)"
            );
            Box::new(RampRateController::new_with_clock(
                RampConfig {
                    warmup_rps: *warmup_rps,
                    warmup_duration: *warmup_duration,
                    latency_multiplier: *latency_multiplier,
                    max_error_rate: *max_error_rate,
                    min_rps: *min_rps,
                    max_rps: *max_rps,
                    control_interval,
                    test_duration,
                    smoothing_window: 0,
                    enable_ratio_freeze: true,
                    external_constraints: external_constraints.clone(),
                },
                clock,
            ))
        }
    };
    controller
}

/// Build an `Arbiter` from a [`RateConfig`].
///
/// This constructs the unified constraint-arbitrating controller for any
/// feedback-based rate config. Static and Step configs are not supported
/// (they have no constraints to arbitrate).
///
/// This coexists with `build_rate_controller` during migration. Once
/// validated, the old builder paths can be replaced.
pub fn build_arbiter(
    rate: &RateConfig,
    _control_interval: Duration,
    _start_time: Instant,
    test_duration: Duration,
    clock: Arc<dyn Clock>,
) -> Option<Box<dyn RateController>> {
    match rate {
        RateConfig::Static { .. } | RateConfig::Step { .. } => None,

        RateConfig::Ramp {
            warmup_rps,
            warmup_duration,
            latency_multiplier,
            max_error_rate,
            min_rps,
            max_rps,
            external_constraints,
        } => {
            tracing::info!(
                warmup_rps,
                ?warmup_duration,
                latency_multiplier,
                max_error_rate,
                min_rps,
                max_rps,
                num_external_constraints = external_constraints.len(),
                "rate controller: arbiter (ramp config)"
            );

            let mut constraints: Vec<Box<dyn Constraint>> = vec![
                Box::new(ThresholdConstraint::timeout(*max_error_rate)),
                Box::new(ThresholdConstraint::inflight()),
                Box::new(ThresholdConstraint::latency(0.0, 3)),
                Box::new(ThresholdConstraint::error_rate(*max_error_rate)),
            ];

            for ext in external_constraints {
                constraints.push(Box::new(ThresholdConstraint::external(ext.clone())));
            }

            Some(Box::new(Arbiter::new(
                ArbiterConfig::new(constraints, *warmup_rps, *min_rps, *max_rps, test_duration)
                    .with_warmup(*warmup_rps, *warmup_duration, *latency_multiplier)
                    .with_clock(clock),
            )))
        }

        RateConfig::Pid {
            initial_rps,
            target,
        } => {
            let (kp, ki, kd) = match &target.gains {
                PidGains::Manual { kp, ki, kd } => (*kp, *ki, *kd),
                PidGains::Auto { .. } => {
                    // Auto-tuning PID via Arbiter not yet supported — fall back to legacy.
                    return None;
                }
            };

            tracing::info!(
                initial_rps,
                metric = ?target.metric,
                target_value = target.value,
                kp, ki, kd,
                "rate controller: arbiter (PID config)"
            );

            let smoother = match &target.metric {
                TargetMetric::LatencyP50 | TargetMetric::LatencyP90 | TargetMetric::LatencyP99 => {
                    Smoother::median(3)
                }
                _ => Smoother::ema(0.3),
            };

            let constraints: Vec<Box<dyn Constraint>> = vec![Box::new(PidConstraint::new(
                format!("{:?}", target.metric),
                target.metric.clone(),
                target.value,
                kp,
                ki,
                kd,
                smoother,
                false, // manual gains, no scheduling
            ))];

            Some(Box::new(Arbiter::new(
                ArbiterConfig::new(
                    constraints,
                    *initial_rps,
                    target.min_rps,
                    target.max_rps,
                    test_duration,
                )
                .with_clock(clock),
            )))
        }

        RateConfig::CompositePid {
            initial_rps,
            constraints: pid_constraints,
            min_rps,
            max_rps,
        } => {
            // Only support all-manual gains via Arbiter for now.
            let all_manual = pid_constraints
                .iter()
                .all(|c| matches!(c.gains, PidGains::Manual { .. }));
            if !all_manual {
                return None;
            }

            tracing::info!(
                initial_rps,
                num_constraints = pid_constraints.len(),
                min_rps,
                max_rps,
                "rate controller: arbiter (composite PID config)"
            );

            let constraints: Vec<Box<dyn Constraint>> = pid_constraints
                .iter()
                .map(|c| {
                    let (kp, ki, kd) = match &c.gains {
                        PidGains::Manual { kp, ki, kd } => (*kp, *ki, *kd),
                        _ => unreachable!(), // checked above
                    };
                    let smoother = match &c.metric {
                        TargetMetric::LatencyP50
                        | TargetMetric::LatencyP90
                        | TargetMetric::LatencyP99 => Smoother::median(3),
                        _ => Smoother::ema(0.3),
                    };
                    Box::new(PidConstraint::new(
                        format!("{:?}", c.metric),
                        c.metric.clone(),
                        c.limit,
                        kp,
                        ki,
                        kd,
                        smoother,
                        false,
                    )) as Box<dyn Constraint>
                })
                .collect();

            Some(Box::new(Arbiter::new(
                ArbiterConfig::new(
                    constraints,
                    *initial_rps,
                    *min_rps,
                    *max_rps,
                    test_duration,
                )
                .with_clock(clock),
            )))
        }
    }
}
