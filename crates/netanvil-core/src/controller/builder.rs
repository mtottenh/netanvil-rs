use std::sync::Arc;
use std::time::{Duration, Instant};

use netanvil_types::{
    BackoffConfig as BackoffConfigType, ConstraintClassConfig, ConstraintConfig,
    CooldownPolicyConfig, FloorPolicyConfig, GainsConfig, IncreasePolicyConfig, InternalMetric,
    MetricRef, RateChangeLimitsConfig, RateConfig, RateController, SmootherConfig, TargetMetric,
    ThresholdSource,
};

use super::arbiter::{Arbiter, ArbiterConfig};
use super::autotune::{ExplorationManager, MetricExploration};
use super::clock::Clock;
use super::constraint::Constraint;
use super::constraints::ConstraintClass;
use super::pid_constraint::PidConstraint;
use super::smoothing::Smoother;
use super::static_rate::StaticRateController;
use super::step_rate::StepRateController;
use super::threshold::{
    BackoffSchedule, MetricExtractor, SeverityMapping, ThresholdConfig, ThresholdConstraint,
};

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
    trace_path: Option<&str>,
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

        RateConfig::Adaptive {
            bounds,
            warmup,
            initial_rps,
            constraints: constraint_configs,
            increase,
            cooldown,
            floor,
            rate_change_limits,
        } => {
            let effective_initial_rps = initial_rps
                .unwrap_or_else(|| warmup.as_ref().map(|w| w.rps).unwrap_or(bounds.min_rps));

            tracing::info!(
                initial_rps = effective_initial_rps,
                num_constraints = constraint_configs.len(),
                min_rps = bounds.min_rps,
                max_rps = bounds.max_rps,
                has_warmup = warmup.is_some(),
                "rate controller: arbiter (adaptive config)"
            );

            let mut auto_explorations: Vec<MetricExploration> = Vec::new();
            let constraints: Vec<Box<dyn Constraint>> = constraint_configs
                .iter()
                .map(|cc| build_adaptive_constraint(cc, warmup.is_some(), &mut auto_explorations))
                .collect();

            let mut config = ArbiterConfig::new(
                constraints,
                effective_initial_rps,
                bounds.min_rps,
                bounds.max_rps,
                test_duration,
            );

            if let Some(ref w) = warmup {
                config = config.with_warmup(w.rps, w.duration);
            }

            if !auto_explorations.is_empty() {
                // Use the maximum autotune_duration from all auto-tuning constraints.
                let autotune_duration = constraint_configs
                    .iter()
                    .filter_map(|cc| match cc {
                        ConstraintConfig::Setpoint(sc) => match &sc.gains {
                            GainsConfig::Auto {
                                autotune_duration, ..
                            } => Some(*autotune_duration),
                            _ => None,
                        },
                        _ => None,
                    })
                    .max()
                    .unwrap_or(Duration::from_secs(3));

                let exploration = ExplorationManager::new(
                    auto_explorations,
                    effective_initial_rps,
                    autotune_duration,
                    control_interval,
                );
                config = config.with_exploration(exploration);
            }

            // Apply optional controller-level policies.
            if let Some(ref inc) = increase {
                let internal = match inc {
                    IncreasePolicyConfig::Multiplicative {
                        factor,
                        congestion_avoidance,
                    } => super::arbiter::IncreasePolicyConfig {
                        increase_factor: *factor,
                        congestion_avoidance: congestion_avoidance.as_ref().map(|ca| {
                            super::arbiter::CongestionAvoidanceConfig {
                                trigger_threshold: ca.trigger_threshold,
                                additive_fraction: ca.additive_fraction,
                                failure_rate_alpha: ca.failure_rate_alpha,
                            }
                        }),
                    },
                    IncreasePolicyConfig::Additive { increment } => {
                        let _ = increment; // Additive: use factor=1.0 + additive via congestion avoidance
                        super::arbiter::IncreasePolicyConfig {
                            increase_factor: 1.0,
                            congestion_avoidance: None,
                        }
                    }
                };
                tracing::info!(
                    increase_policy = ?internal,
                    "arbiter: final increase policy"
                );
                config = config.with_increase_policy(internal);
            }

            if let Some(ref cd) = cooldown {
                config = config.with_cooldown(cd.min_ticks, cd.recovery_multiplier);
            }

            if let Some(ref fl) = floor {
                config = config.with_known_good_floor(fl.fraction, fl.window);
            }

            if let Some(ref rcl) = rate_change_limits {
                config = config.with_rate_change_limits(super::arbiter::RateChangeLimits {
                    max_increase_pct: rcl.max_increase_pct,
                    max_decrease_pct: rcl.max_decrease_pct,
                });
            }

            if let Some(path) = trace_path {
                config = config.with_trace_path(path.to_string());
            }

            Box::new(Arbiter::new(config.with_clock(clock)))
        }
    }
}

// ---------------------------------------------------------------------------
// Adaptive constraint mapping helpers
// ---------------------------------------------------------------------------

/// Map a `ConstraintConfig` to a `Box<dyn Constraint>`.
///
/// Also accumulates `MetricExploration` entries for auto-tuning PID constraints.
fn build_adaptive_constraint(
    cc: &ConstraintConfig,
    has_warmup: bool,
    auto_explorations: &mut Vec<MetricExploration>,
) -> Box<dyn Constraint> {
    match cc {
        ConstraintConfig::Threshold(tc) => {
            let metric = map_metric_ref_to_extractor(&tc.metric);
            let smoother = map_smoother_config(&tc.smoother, &tc.metric);
            let class = map_constraint_class(&tc.class_override, &tc.metric);
            let direction = map_metric_direction(&tc.metric);
            let backoff = map_backoff_config(&tc.backoff);

            let (threshold, baseline_multiplier, baseline_floor_ms) = match &tc.threshold_source {
                ThresholdSource::Absolute { threshold } => (*threshold, None, None),
                ThresholdSource::FromBaseline {
                    threshold_from_baseline,
                } => {
                    if !has_warmup {
                        panic!(
                            "constraint '{}' uses threshold_from_baseline but no warmup is \
                             configured -- add a warmup block or use threshold: <value>",
                            tc.id
                        );
                    }
                    (
                        0.0,
                        Some(threshold_from_baseline.multiplier),
                        Some(threshold_from_baseline.baseline_floor_ms),
                    )
                }
            };

            let severity_mapping = map_severity_for_threshold(&tc.metric, threshold);

            Box::new(ThresholdConstraint::new(ThresholdConfig {
                id: tc.id.clone(),
                class,
                metric,
                smoother,
                threshold,
                severity_mapping,
                persistence: tc.persistence,
                recovery_seed: default_recovery_seed(class),
                backoff,
                external: map_external_constraint_config(&tc.metric),
                self_caused_cap: tc.self_caused_cap,
                direction,
                baseline_multiplier,
                baseline_floor_ms,
            }))
        }
        ConstraintConfig::Setpoint(sc) => {
            // Validate: setpoint on TimeoutFraction/InFlightDropFraction is nonsensical.
            if let MetricRef::Internal(ref im) = sc.metric {
                if matches!(
                    im,
                    InternalMetric::TimeoutFraction | InternalMetric::InFlightDropFraction
                ) {
                    panic!(
                        "constraint '{}': Setpoint on {:?} is not meaningful -- \
                         use a Threshold constraint instead",
                        sc.id, im
                    );
                }
            }

            let target_metric = map_metric_ref_to_target(&sc.metric);
            let smoother = map_smoother_config_for_pid(&sc.smoother, &sc.metric, &sc.gains);

            match &sc.gains {
                GainsConfig::Manual { kp, ki, kd } => {
                    let mut constraint = PidConstraint::new(
                        sc.id.clone(),
                        target_metric,
                        sc.target,
                        *kp,
                        *ki,
                        *kd,
                        smoother,
                        false,
                    );
                    constraint.set_tracking_gain(sc.tracking_gain);
                    Box::new(constraint)
                }
                GainsConfig::Auto { smoothing, .. } => {
                    // Use the smoothing value if the user didn't specify a custom smoother.
                    let smoother = if sc.smoother.is_some() {
                        smoother
                    } else {
                        Smoother::ema(*smoothing)
                    };
                    auto_explorations
                        .push(MetricExploration::new(target_metric.clone(), sc.target));
                    let mut constraint = PidConstraint::auto_tuning(
                        sc.id.clone(),
                        target_metric,
                        sc.target,
                        smoother,
                    );
                    constraint.set_tracking_gain(sc.tracking_gain);
                    Box::new(constraint)
                }
            }
        }
    }
}

/// Map `MetricRef` to `MetricExtractor` (for threshold constraints).
fn map_metric_ref_to_extractor(metric: &MetricRef) -> MetricExtractor {
    match metric {
        MetricRef::Internal(im) => match im {
            InternalMetric::LatencyP50 => MetricExtractor::LatencyP50Ms,
            InternalMetric::LatencyP90 => MetricExtractor::LatencyP90Ms,
            InternalMetric::LatencyP99 => MetricExtractor::LatencyP99Ms,
            InternalMetric::ErrorRate => MetricExtractor::ErrorRatePct,
            InternalMetric::ThroughputSend | InternalMetric::ThroughputRecv => {
                // Threshold on throughput is unusual but possible.
                // Use ErrorRatePct as a fallback — throughput thresholds should
                // generally be expressed as Setpoint constraints.
                panic!(
                    "Threshold constraints on {:?} are not supported -- use a Setpoint constraint",
                    im
                );
            }
            InternalMetric::TimeoutFraction => MetricExtractor::TimeoutFraction,
            InternalMetric::InFlightDropFraction => MetricExtractor::InFlightDropFraction,
        },
        MetricRef::External(ext) => MetricExtractor::External {
            signal_name: ext.name.clone(),
            direction: ext.direction,
        },
    }
}

/// Map `MetricRef` to `TargetMetric` (for PID setpoint constraints).
fn map_metric_ref_to_target(metric: &MetricRef) -> TargetMetric {
    match metric {
        MetricRef::Internal(im) => match im {
            InternalMetric::LatencyP50 => TargetMetric::LatencyP50,
            InternalMetric::LatencyP90 => TargetMetric::LatencyP90,
            InternalMetric::LatencyP99 => TargetMetric::LatencyP99,
            InternalMetric::ErrorRate => TargetMetric::ErrorRate,
            InternalMetric::ThroughputSend => TargetMetric::ThroughputSend,
            InternalMetric::ThroughputRecv => TargetMetric::ThroughputRecv,
            InternalMetric::TimeoutFraction | InternalMetric::InFlightDropFraction => {
                // Validated by caller — should not reach here.
                unreachable!("TimeoutFraction/InFlightDropFraction should be caught by validation")
            }
        },
        MetricRef::External(ext) => TargetMetric::External {
            name: ext.name.clone(),
        },
    }
}

/// Map `SmootherConfig` to `Smoother`, with per-metric defaults.
fn map_smoother_config(config: &Option<SmootherConfig>, metric: &MetricRef) -> Smoother {
    match config {
        Some(SmootherConfig::None) => Smoother::none(),
        Some(SmootherConfig::Ema { alpha }) => Smoother::ema(*alpha),
        Some(SmootherConfig::Median { size }) => Smoother::median(*size),
        None => default_smoother_for_metric(metric),
    }
}

/// Map `SmootherConfig` to `Smoother` for PID constraints.
fn map_smoother_config_for_pid(
    config: &Option<SmootherConfig>,
    metric: &MetricRef,
    gains: &GainsConfig,
) -> Smoother {
    match config {
        Some(SmootherConfig::None) => Smoother::none(),
        Some(SmootherConfig::Ema { alpha }) => Smoother::ema(*alpha),
        Some(SmootherConfig::Median { size }) => Smoother::median(*size),
        None => match gains {
            GainsConfig::Auto { smoothing, .. } => Smoother::ema(*smoothing),
            GainsConfig::Manual { .. } => default_smoother_for_metric(metric),
        },
    }
}

/// Default smoother by metric type: median for latency, EMA for everything else.
fn default_smoother_for_metric(metric: &MetricRef) -> Smoother {
    match metric {
        MetricRef::Internal(
            InternalMetric::LatencyP50 | InternalMetric::LatencyP90 | InternalMetric::LatencyP99,
        ) => Smoother::median(3),
        _ => Smoother::ema(0.3),
    }
}

/// Map constraint class from config override or derive from metric.
fn map_constraint_class(
    class_override: &Option<ConstraintClassConfig>,
    metric: &MetricRef,
) -> ConstraintClass {
    match class_override {
        Some(ConstraintClassConfig::OperatingPoint) => ConstraintClass::OperatingPoint,
        Some(ConstraintClassConfig::Catastrophic) => ConstraintClass::Catastrophic,
        None => match metric {
            MetricRef::Internal(
                InternalMetric::TimeoutFraction | InternalMetric::InFlightDropFraction,
            ) => ConstraintClass::Catastrophic,
            _ => ConstraintClass::OperatingPoint,
        },
    }
}

/// Map metric direction from MetricRef.
fn map_metric_direction(metric: &MetricRef) -> netanvil_types::SignalDirection {
    match metric {
        MetricRef::Internal(InternalMetric::ThroughputSend | InternalMetric::ThroughputRecv) => {
            netanvil_types::SignalDirection::LowerIsWorse
        }
        MetricRef::External(ext) => ext.direction,
        _ => netanvil_types::SignalDirection::HigherIsWorse,
    }
}

/// Map backoff config to BackoffSchedule.
fn map_backoff_config(config: &Option<BackoffConfigType>) -> BackoffSchedule {
    match config {
        Some(bc) => BackoffSchedule {
            gentle: bc.gentle,
            moderate: bc.moderate,
            hard: bc.hard,
        },
        None => BackoffSchedule::default(),
    }
}

/// Map severity mapping based on metric type and threshold.
fn map_severity_for_threshold(metric: &MetricRef, threshold: f64) -> SeverityMapping {
    match metric {
        MetricRef::Internal(InternalMetric::TimeoutFraction) => {
            // Convert threshold (fraction) to percentage for TimeoutDirect.
            SeverityMapping::TimeoutDirect {
                max_error_rate_pct: threshold * 100.0,
            }
        }
        MetricRef::Internal(InternalMetric::InFlightDropFraction) => {
            SeverityMapping::InFlightDirect
        }
        MetricRef::Internal(InternalMetric::ErrorRate) => SeverityMapping::ErrorRateDirect {
            max_error_rate_pct: threshold,
        },
        _ => SeverityMapping::Graduated,
    }
}

/// Build an `ExternalConstraintConfig` from a `MetricRef::External`, if applicable.
fn map_external_constraint_config(
    metric: &MetricRef,
) -> Option<netanvil_types::ExternalConstraintConfig> {
    match metric {
        MetricRef::External(ext) => Some(netanvil_types::ExternalConstraintConfig {
            signal_name: ext.name.clone(),
            threshold: 0.0, // Will be overridden by the calling code.
            direction: ext.direction,
            on_missing: ext.on_missing,
            stale_after_ticks: ext.stale_after_ticks,
            persistence: 1,
        }),
        MetricRef::Internal(_) => None,
    }
}

/// Default recovery seed by constraint class.
fn default_recovery_seed(class: ConstraintClass) -> f64 {
    match class {
        ConstraintClass::Catastrophic => 20.0,
        ConstraintClass::OperatingPoint => 5.0,
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
    trace_path: Option<&str>,
) -> Option<Box<dyn RateController>> {
    match rate {
        RateConfig::Static { .. } | RateConfig::Step { .. } => None,
        _ => Some(build_rate_controller(
            rate,
            control_interval,
            start_time,
            test_duration,
            clock,
            trace_path,
        )),
    }
}
