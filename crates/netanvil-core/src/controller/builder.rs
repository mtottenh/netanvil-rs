use std::sync::Arc;
use std::time::{Duration, Instant};

use netanvil_types::{PidGains, RateConfig, RateController, TargetMetric};

use super::arbiter::{Arbiter, ArbiterConfig};
use super::autotune::{ExplorationManager, MetricExploration};
use super::clock::Clock;
use super::constraint::Constraint;
use super::pid_constraint::PidConstraint;
use super::smoothing::Smoother;
use super::static_rate::StaticRateController;
use super::step_rate::StepRateController;
use super::threshold::ThresholdConstraint;

/// Build a `Box<dyn RateController>` from a [`RateConfig`].
///
/// This is the single source of truth for `RateConfig` -> `RateController` mapping.
/// Used by both the local engine (`run_test_core`) and the distributed leader.
///
/// Static and Step controllers are constructed directly. All feedback-based
/// configs (Pid, CompositePid, Ramp) are routed through the unified Arbiter.
pub fn build_rate_controller(
    rate: &RateConfig,
    control_interval: Duration,
    start_time: Instant,
    test_duration: Duration,
    clock: Arc<dyn Clock>,
) -> Box<dyn RateController> {
    match rate {
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

            Box::new(Arbiter::new(
                ArbiterConfig::new(constraints, *warmup_rps, *min_rps, *max_rps, test_duration)
                    .with_warmup(*warmup_rps, *warmup_duration, *latency_multiplier)
                    .with_clock(clock),
            ))
        }

        RateConfig::Pid {
            initial_rps,
            target,
        } => {
            match &target.gains {
                PidGains::Manual { kp, ki, kd } => {
                    tracing::info!(
                        initial_rps,
                        metric = ?target.metric,
                        target_value = target.value,
                        kp, ki, kd,
                        "rate controller: arbiter (PID manual)"
                    );

                    let smoother = match &target.metric {
                        TargetMetric::LatencyP50
                        | TargetMetric::LatencyP90
                        | TargetMetric::LatencyP99 => Smoother::median(3),
                        _ => Smoother::ema(0.3),
                    };

                    let constraints: Vec<Box<dyn Constraint>> =
                        vec![Box::new(PidConstraint::new(
                            format!("{:?}", target.metric),
                            target.metric.clone(),
                            target.value,
                            *kp,
                            *ki,
                            *kd,
                            smoother,
                            false,
                        ))];

                    Box::new(Arbiter::new(
                        ArbiterConfig::new(
                            constraints,
                            *initial_rps,
                            target.min_rps,
                            target.max_rps,
                            test_duration,
                        )
                        .with_clock(clock),
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
                        ?autotune_duration,
                        smoothing,
                        "rate controller: arbiter (PID auto-tune)"
                    );

                    let smoother = Smoother::ema(*smoothing);
                    let constraint = PidConstraint::auto_tuning(
                        format!("{:?}", target.metric),
                        target.metric.clone(),
                        target.value,
                        smoother,
                    );
                    let constraints: Vec<Box<dyn Constraint>> = vec![Box::new(constraint)];

                    let exploration = ExplorationManager::new(
                        vec![MetricExploration::new(target.metric.clone(), target.value)],
                        *initial_rps,
                        *autotune_duration,
                        control_interval,
                    );

                    Box::new(Arbiter::new(
                        ArbiterConfig::new(
                            constraints,
                            *initial_rps,
                            target.min_rps,
                            target.max_rps,
                            test_duration,
                        )
                        .with_exploration(exploration)
                        .with_clock(clock),
                    ))
                }
            }
        }

        RateConfig::CompositePid {
            initial_rps,
            constraints: pid_constraints,
            min_rps,
            max_rps,
        } => {
            let has_auto = pid_constraints
                .iter()
                .any(|c| matches!(c.gains, PidGains::Auto { .. }));

            tracing::info!(
                initial_rps,
                num_constraints = pid_constraints.len(),
                has_auto,
                min_rps,
                max_rps,
                "rate controller: arbiter (composite PID)"
            );

            let mut auto_explorations = Vec::new();
            let constraints: Vec<Box<dyn Constraint>> = pid_constraints
                .iter()
                .map(|c| match &c.gains {
                    PidGains::Manual { kp, ki, kd } => {
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
                            *kp,
                            *ki,
                            *kd,
                            smoother,
                            false,
                        )) as Box<dyn Constraint>
                    }
                    PidGains::Auto { smoothing, .. } => {
                        let smoother = Smoother::ema(*smoothing);
                        auto_explorations
                            .push(MetricExploration::new(c.metric.clone(), c.limit));
                        Box::new(PidConstraint::auto_tuning(
                            format!("{:?}", c.metric),
                            c.metric.clone(),
                            c.limit,
                            smoother,
                        )) as Box<dyn Constraint>
                    }
                })
                .collect();

            let mut config = ArbiterConfig::new(
                constraints,
                *initial_rps,
                *min_rps,
                *max_rps,
                test_duration,
            );

            if !auto_explorations.is_empty() {
                let autotune_duration = pid_constraints
                    .iter()
                    .filter_map(|c| match &c.gains {
                        PidGains::Auto {
                            autotune_duration, ..
                        } => Some(*autotune_duration),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(Duration::from_secs(3));

                let exploration = ExplorationManager::new(
                    auto_explorations,
                    *initial_rps,
                    autotune_duration,
                    control_interval,
                );
                config = config.with_exploration(exploration);
            }

            Box::new(Arbiter::new(config.with_clock(clock)))
        }
    }
}

/// Build an `Arbiter` from a [`RateConfig`].
///
/// Returns `None` for Static and Step configs (they have no constraints).
/// For all feedback-based configs, returns `Some(Box<dyn RateController>)`.
///
/// This is kept as a convenience for tests that need to verify arbiter
/// construction separately. Production code should use `build_rate_controller`.
pub fn build_arbiter(
    rate: &RateConfig,
    control_interval: Duration,
    start_time: Instant,
    test_duration: Duration,
    clock: Arc<dyn Clock>,
) -> Option<Box<dyn RateController>> {
    match rate {
        RateConfig::Static { .. } | RateConfig::Step { .. } => None,
        _ => Some(build_rate_controller(
            rate,
            control_interval,
            start_time,
            test_duration,
            clock,
        )),
    }
}
