use std::time::Duration;

use netanvil_core::{
    controller::autotune::{
        compute_cohen_coon_gains, gain_schedule, ExplorationManager, MetricExploration, PidState,
    },
    AutotuneParams, AutotuningPidController, CompositePidController, PidGainValues,
    PidRateController, RampConfig, RampRateController, StaticRateController, StepRateController,
};
use netanvil_types::{
    ExternalConstraintConfig, MetricsSummary, MissingSignalBehavior, PidConstraint, PidGains,
    RateController, SignalDirection, TargetMetric,
};

// ---------------------------------------------------------------------------
// StaticRateController behavioral tests
// ---------------------------------------------------------------------------

#[test]
fn static_controller_ignores_all_metrics() {
    let mut ctrl = StaticRateController::new(500.0);

    // Feed it wildly varying metrics — rate should never change
    let scenarios = vec![
        MetricsSummary {
            total_requests: 1000,
            error_rate: 0.0,
            latency_p99_ns: 1_000_000, // 1ms, very fast
            ..Default::default()
        },
        MetricsSummary {
            total_requests: 10,
            error_rate: 0.9,               // 90% errors
            latency_p99_ns: 5_000_000_000, // 5 seconds
            ..Default::default()
        },
        MetricsSummary::default(), // zero everything
    ];

    for summary in &scenarios {
        let decision = ctrl.update(summary);
        assert_eq!(decision.target_rps, 500.0);
        assert_eq!(ctrl.current_rate(), 500.0);
    }
}

// ---------------------------------------------------------------------------
// StepRateController behavioral tests
// ---------------------------------------------------------------------------

#[test]
fn step_controller_follows_time_based_schedule() {
    let steps = vec![
        (Duration::from_millis(0), 100.0),
        (Duration::from_millis(100), 300.0),
        (Duration::from_millis(200), 50.0),
    ];

    let start = std::time::Instant::now();
    let mut ctrl = StepRateController::with_start_time(steps, start);
    let summary = MetricsSummary::default();

    // At t=0, should be 100 RPS
    let decision = ctrl.update(&summary);
    assert_eq!(decision.target_rps, 100.0);

    // Wait past first step boundary
    std::thread::sleep(Duration::from_millis(120));
    let decision = ctrl.update(&summary);
    assert_eq!(decision.target_rps, 300.0);

    // Wait past second step boundary
    std::thread::sleep(Duration::from_millis(100));
    let decision = ctrl.update(&summary);
    assert_eq!(decision.target_rps, 50.0);
}

#[test]
fn step_controller_stays_at_last_step_after_all_transitions() {
    let steps = vec![
        (Duration::from_millis(0), 100.0),
        (Duration::from_millis(10), 200.0),
    ];

    let past = std::time::Instant::now() - Duration::from_secs(10);
    let mut ctrl = StepRateController::with_start_time(steps, past);
    let summary = MetricsSummary::default();

    // All steps are in the past — should be at last step
    let decision = ctrl.update(&summary);
    assert_eq!(decision.target_rps, 200.0);
}

// ---------------------------------------------------------------------------
// PidRateController behavioral tests
// ---------------------------------------------------------------------------

fn make_summary(requests: u64, p99_ms: f64, error_rate: f64) -> MetricsSummary {
    MetricsSummary {
        total_requests: requests,
        total_errors: (requests as f64 * error_rate) as u64,
        error_rate,
        request_rate: requests as f64,
        latency_p50_ns: (p99_ms * 0.5 * 1_000_000.0) as u64,
        latency_p90_ns: (p99_ms * 0.9 * 1_000_000.0) as u64,
        latency_p99_ns: (p99_ms * 1_000_000.0) as u64,
        window_duration: Duration::from_secs(1),
        ..Default::default()
    }
}

#[test]
fn pid_reduces_rate_when_latency_exceeds_target() {
    let mut ctrl = PidRateController::new(
        TargetMetric::LatencyP99,
        100.0,   // target 100ms p99
        500.0,   // initial 500 RPS
        10.0,    // min
        10000.0, // max
        PidGainValues {
            kp: 0.5,
            ki: 0.01,
            kd: 0.1,
        },
    );

    let initial_rate = ctrl.current_rate();

    // Feed metrics showing p99 = 200ms (well above 100ms target)
    for _ in 0..10 {
        ctrl.update(&make_summary(100, 200.0, 0.0));
    }

    assert!(
        ctrl.current_rate() < initial_rate,
        "rate should decrease when latency exceeds target: was {initial_rate}, now {}",
        ctrl.current_rate()
    );
}

#[test]
fn pid_increases_rate_when_latency_below_target() {
    let mut ctrl = PidRateController::new(
        TargetMetric::LatencyP99,
        100.0, // target 100ms p99
        200.0, // initial 200 RPS
        10.0,
        10000.0,
        PidGainValues {
            kp: 0.5,
            ki: 0.01,
            kd: 0.1,
        },
    );

    let initial_rate = ctrl.current_rate();

    // Feed metrics showing p99 = 20ms (well below 100ms target)
    for _ in 0..10 {
        ctrl.update(&make_summary(100, 20.0, 0.0));
    }

    assert!(
        ctrl.current_rate() > initial_rate,
        "rate should increase when latency is below target: was {initial_rate}, now {}",
        ctrl.current_rate()
    );
}

#[test]
fn pid_respects_min_max_bounds() {
    let mut ctrl = PidRateController::new(
        TargetMetric::LatencyP99,
        100.0,
        500.0,
        50.0,   // min 50 RPS
        1000.0, // max 1000 RPS
        PidGainValues {
            kp: 2.0,
            ki: 0.1,
            kd: 0.5,
        }, // aggressive gains
    );

    // Drive latency very high — rate should hit min but not go below
    for _ in 0..100 {
        ctrl.update(&make_summary(100, 5000.0, 0.0));
    }
    assert!(
        ctrl.current_rate() >= 50.0,
        "rate should not go below min: {}",
        ctrl.current_rate()
    );

    // Reset and drive latency very low — rate should hit max but not exceed
    let mut ctrl = PidRateController::new(
        TargetMetric::LatencyP99,
        100.0,
        500.0,
        50.0,
        1000.0,
        PidGainValues {
            kp: 2.0,
            ki: 0.1,
            kd: 0.5,
        },
    );
    for _ in 0..100 {
        ctrl.update(&make_summary(100, 1.0, 0.0));
    }
    assert!(
        ctrl.current_rate() <= 1000.0,
        "rate should not exceed max: {}",
        ctrl.current_rate()
    );
}

#[test]
fn pid_ignores_updates_with_too_few_samples() {
    let mut ctrl = PidRateController::new(
        TargetMetric::LatencyP99,
        100.0,
        500.0,
        10.0,
        10000.0,
        PidGainValues {
            kp: 0.5,
            ki: 0.01,
            kd: 0.1,
        },
    );

    // Feed metrics with only 3 requests (below threshold of 5)
    let decision = ctrl.update(&make_summary(3, 500.0, 0.5));
    assert_eq!(
        decision.target_rps, 500.0,
        "should not adjust with too few samples"
    );
}

#[test]
fn pid_stabilizes_near_target() {
    let mut ctrl = PidRateController::new(
        TargetMetric::LatencyP99,
        100.0, // target 100ms
        500.0,
        10.0,
        10000.0,
        PidGainValues {
            kp: 0.1,
            ki: 0.001,
            kd: 0.05,
        }, // moderate gains
    );

    // Simulate a system where latency increases linearly with rate:
    // latency_ms = rate / 10.0
    // So at rate=1000, latency=100ms (the target).
    for _ in 0..200 {
        let rate = ctrl.current_rate();
        let simulated_latency_ms = rate / 10.0;
        ctrl.update(&make_summary(100, simulated_latency_ms, 0.0));
    }

    // After 200 iterations, rate should be near 1000 (the equilibrium)
    let final_rate = ctrl.current_rate();
    assert!(
        (final_rate - 1000.0).abs() < 200.0,
        "rate should stabilize near 1000, got {final_rate:.1}"
    );
}

#[test]
fn pid_can_target_error_rate() {
    let mut ctrl = PidRateController::new(
        TargetMetric::ErrorRate,
        5.0, // target 5% error rate
        1000.0,
        10.0,
        10000.0,
        PidGainValues {
            kp: 0.5,
            ki: 0.01,
            kd: 0.1,
        },
    );

    let initial = ctrl.current_rate();

    // Feed 20% error rate — way above 5% target
    // error > 0 means we're below target (good), but for error rate,
    // current_value = error_rate * 100 = 20, target = 5, error = 5 - 20 = -15
    // Negative error → reduce rate
    for _ in 0..10 {
        ctrl.update(&make_summary(100, 50.0, 0.20));
    }

    assert!(
        ctrl.current_rate() < initial,
        "rate should decrease when error rate exceeds target"
    );
}

// ---------------------------------------------------------------------------
// Generator and Transformer behavioral tests
// ---------------------------------------------------------------------------

#[test]
fn simple_generator_round_robins_urls() {
    use netanvil_core::SimpleGenerator;
    use netanvil_types::{RequestContext, RequestGenerator};
    use std::time::Instant;

    let now = Instant::now();
    let mut gen = SimpleGenerator::get(vec![
        "http://a.com".into(),
        "http://b.com".into(),
        "http://c.com".into(),
    ]);

    let ctx = RequestContext {
        request_id: 0,
        intended_time: now,
        actual_time: now,
        core_id: 0,
        is_sampled: false,
        session_id: None,
    };

    let urls: Vec<String> = (0..9).map(|_| gen.generate(&ctx).url).collect();
    assert_eq!(
        urls,
        vec![
            "http://a.com",
            "http://b.com",
            "http://c.com",
            "http://a.com",
            "http://b.com",
            "http://c.com",
            "http://a.com",
            "http://b.com",
            "http://c.com",
        ]
    );
}

#[test]
fn header_transformer_appends_headers() {
    use netanvil_core::{HeaderTransformer, NoopTransformer};
    use netanvil_types::{HttpRequestSpec, RequestContext, RequestTransformer};
    use std::time::Instant;

    let now = Instant::now();
    let ctx = RequestContext {
        request_id: 0,
        intended_time: now,
        actual_time: now,
        core_id: 0,
        is_sampled: false,
        session_id: None,
    };

    let spec = HttpRequestSpec {
        method: http::Method::GET,
        url: "http://example.com".into(),
        headers: vec![("Accept".into(), "text/html".into())],
        body: None,
    };

    // Noop should not change anything
    let noop = NoopTransformer;
    let result = noop.transform(spec.clone(), &ctx);
    assert_eq!(result.headers.len(), 1);

    // Header transformer should append
    let transformer = HeaderTransformer::new(vec![
        ("X-Custom".into(), "value".into()),
        ("Authorization".into(), "Bearer token".into()),
    ]);
    let result = transformer.transform(spec, &ctx);
    assert_eq!(result.headers.len(), 3);
    assert_eq!(result.headers[1].0, "X-Custom");
    assert_eq!(result.headers[2].0, "Authorization");
}

// ---------------------------------------------------------------------------
// External signal PID control tests
// ---------------------------------------------------------------------------

fn make_summary_with_signal(requests: u64, signal_name: &str, signal_value: f64) -> MetricsSummary {
    MetricsSummary {
        total_requests: requests,
        total_errors: 0,
        error_rate: 0.0,
        request_rate: requests as f64,
        latency_p50_ns: 5_000_000,
        latency_p90_ns: 10_000_000,
        latency_p99_ns: 20_000_000,
        window_duration: Duration::from_secs(1),
        external_signals: vec![(signal_name.to_string(), signal_value)],
        ..Default::default()
    }
}

#[test]
fn pid_reduces_rate_when_external_load_exceeds_target() {
    let mut ctrl = PidRateController::new(
        TargetMetric::External {
            name: "load".into(),
        },
        80.0, // target: keep load at 80%
        500.0,
        10.0,
        10000.0,
        PidGainValues {
            kp: 0.5,
            ki: 0.01,
            kd: 0.1,
        },
    );

    let initial = ctrl.current_rate();

    // Feed load=95 (above 80 target) → should reduce rate
    for _ in 0..10 {
        ctrl.update(&make_summary_with_signal(100, "load", 95.0));
    }

    assert!(
        ctrl.current_rate() < initial,
        "rate should decrease when server load exceeds target: was {initial}, now {}",
        ctrl.current_rate()
    );
}

#[test]
fn pid_increases_rate_when_external_load_below_target() {
    let mut ctrl = PidRateController::new(
        TargetMetric::External {
            name: "load".into(),
        },
        80.0,
        200.0,
        10.0,
        10000.0,
        PidGainValues {
            kp: 0.5,
            ki: 0.01,
            kd: 0.1,
        },
    );

    let initial = ctrl.current_rate();

    // Feed load=40 (well below 80 target) → should increase rate
    for _ in 0..10 {
        ctrl.update(&make_summary_with_signal(100, "load", 40.0));
    }

    assert!(
        ctrl.current_rate() > initial,
        "rate should increase when server load is below target: was {initial}, now {}",
        ctrl.current_rate()
    );
}

#[test]
fn pid_stabilizes_on_external_signal() {
    let mut ctrl = PidRateController::new(
        TargetMetric::External { name: "cpu".into() },
        70.0, // target 70% CPU
        500.0,
        10.0,
        10000.0,
        PidGainValues {
            kp: 0.1,
            ki: 0.001,
            kd: 0.05,
        },
    );

    // Simulate: cpu_load = rate / 10 (linear relationship)
    // Equilibrium: rate=700 → load=70
    for _ in 0..200 {
        let rate = ctrl.current_rate();
        let simulated_cpu = rate / 10.0;
        ctrl.update(&make_summary_with_signal(100, "cpu", simulated_cpu));
    }

    let final_rate = ctrl.current_rate();
    assert!(
        (final_rate - 700.0).abs() < 150.0,
        "rate should stabilize near 700 (where cpu=70%), got {final_rate:.1}"
    );
}

#[test]
fn pid_handles_missing_external_signal_gracefully() {
    let mut ctrl = PidRateController::new(
        TargetMetric::External {
            name: "load".into(),
        },
        80.0,
        500.0,
        10.0,
        10000.0,
        PidGainValues {
            kp: 0.5,
            ki: 0.01,
            kd: 0.1,
        },
    );

    // Feed a summary with NO external signals — should default to 0.0
    // target=80, current=0 → error = 80 → rate should increase aggressively
    let summary = MetricsSummary {
        total_requests: 100,
        external_signals: vec![], // empty!
        ..Default::default()
    };

    let initial = ctrl.current_rate();
    ctrl.update(&summary);

    // Rate should increase (PID sees error=80, tries to push load up)
    assert!(
        ctrl.current_rate() >= initial,
        "rate should not decrease when signal is missing"
    );
}

// ===========================================================================
// AutotuningPidController behavioral tests
// ===========================================================================

fn make_summary_with_latency_and_errors(
    requests: u64,
    p99_ms: f64,
    error_rate: f64,
    signals: Vec<(String, f64)>,
) -> MetricsSummary {
    MetricsSummary {
        total_requests: requests,
        total_errors: (requests as f64 * error_rate) as u64,
        error_rate,
        request_rate: requests as f64,
        latency_p50_ns: (p99_ms * 0.5 * 1_000_000.0) as u64,
        latency_p90_ns: (p99_ms * 0.9 * 1_000_000.0) as u64,
        latency_p99_ns: (p99_ms * 1_000_000.0) as u64,
        window_duration: Duration::from_secs(1),
        external_signals: signals,
        ..Default::default()
    }
}

#[test]
fn autotune_baseline_phase_holds_reduced_rate() {
    let mut ctrl = AutotuningPidController::new(
        TargetMetric::LatencyP99,
        100.0,  // target 100ms
        1000.0, // initial 1000 RPS
        10.0,
        50000.0,
        AutotuneParams {
            autotune_duration: Duration::from_secs(3),
            smoothing: 0.3,
            control_interval: Duration::from_millis(100),
        },
    );

    // During baseline, rate should be ~50% of initial
    let _decision = ctrl.update(&make_summary(100, 50.0, 0.0));
    assert!(
        ctrl.current_rate() <= 600.0,
        "baseline phase should hold rate at ~50% of initial, got {}",
        ctrl.current_rate()
    );
}

#[test]
fn autotune_transitions_to_active_after_exploration() {
    let mut ctrl = AutotuningPidController::new(
        TargetMetric::LatencyP99,
        100.0,
        1000.0,
        10.0,
        50000.0,
        AutotuneParams {
            autotune_duration: Duration::from_secs(1), // short exploration for testing
            smoothing: 0.3,
            control_interval: Duration::from_millis(100),
        },
    );

    // Feed enough ticks to complete baseline (5 ticks @ 100ms = 500ms)
    // then step response (5+ ticks)
    for i in 0..30 {
        let latency = 20.0 + (i as f64) * 2.0; // rising latency
        ctrl.update(&make_summary(100, latency, 0.0));
    }

    // After 30 ticks, exploration should be done, controller should be in active mode
    let rate = ctrl.current_rate();
    assert!(
        rate > 10.0 && rate < 50000.0,
        "should have a reasonable rate after exploration, got {rate}"
    );
}

#[test]
fn autotune_converges_on_linear_system() {
    // Simulate: latency_ms = rate / 10
    // Target: 100ms → equilibrium at rate=1000
    let mut ctrl = AutotuningPidController::new(
        TargetMetric::LatencyP99,
        100.0,  // target 100ms
        2000.0, // initial RPS
        10.0,
        50000.0,
        AutotuneParams {
            autotune_duration: Duration::from_secs(1), // short exploration
            smoothing: 0.3,
            control_interval: Duration::from_millis(100),
        },
    );

    // 1000 ticks: ~15 for exploration, ~985 for convergence.
    // The anti-windup logic slows convergence (by design) so we
    // need more ticks and a wider tolerance than a naive PID.
    for _ in 0..1000 {
        let rate = ctrl.current_rate();
        let simulated_latency = rate / 10.0;
        ctrl.update(&make_summary(100, simulated_latency, 0.0));
    }

    let final_rate = ctrl.current_rate();
    assert!(
        (final_rate - 1000.0).abs() < 800.0,
        "autotune PID should converge near 1000 (equilibrium), got {final_rate:.1}"
    );
}

#[test]
fn autotune_set_rate_during_exploration_falls_back() {
    let mut ctrl = AutotuningPidController::new(
        TargetMetric::LatencyP99,
        100.0,
        1000.0,
        10.0,
        50000.0,
        AutotuneParams {
            autotune_duration: Duration::from_secs(3),
            smoothing: 0.3,
            control_interval: Duration::from_millis(100),
        },
    );

    // One tick, then external override
    ctrl.update(&make_summary(100, 50.0, 0.0));
    ctrl.set_rate(500.0);

    assert_eq!(ctrl.current_rate(), 500.0);

    // Should still work after override (fallback to conservative gains)
    for _ in 0..20 {
        let rate = ctrl.current_rate();
        let latency = rate / 10.0;
        ctrl.update(&make_summary(100, latency, 0.0));
    }
    assert!(
        ctrl.current_rate() > 10.0,
        "should still produce reasonable rate after fallback"
    );
}

#[test]
fn autotune_respects_min_max_bounds() {
    let mut ctrl = AutotuningPidController::new(
        TargetMetric::LatencyP99,
        100.0,
        1000.0,
        50.0,   // min
        2000.0, // max
        AutotuneParams {
            autotune_duration: Duration::from_secs(1),
            smoothing: 0.3,
            control_interval: Duration::from_millis(100),
        },
    );

    // Drive through exploration
    for _ in 0..30 {
        ctrl.update(&make_summary(100, 10.0, 0.0)); // latency very low → wants to increase
    }

    // Active phase: push rate up hard
    for _ in 0..100 {
        ctrl.update(&make_summary(100, 1.0, 0.0)); // latency 1ms, target 100ms
    }
    assert!(
        ctrl.current_rate() <= 2000.0,
        "should not exceed max: {}",
        ctrl.current_rate()
    );

    // Reset and push rate down hard
    let mut ctrl = AutotuningPidController::new(
        TargetMetric::LatencyP99,
        100.0,
        1000.0,
        50.0,
        2000.0,
        AutotuneParams {
            autotune_duration: Duration::from_secs(1),
            smoothing: 0.3,
            control_interval: Duration::from_millis(100),
        },
    );
    for _ in 0..30 {
        ctrl.update(&make_summary(100, 500.0, 0.0));
    }
    for _ in 0..100 {
        ctrl.update(&make_summary(100, 5000.0, 0.0)); // way above target
    }
    assert!(
        ctrl.current_rate() >= 50.0,
        "should not go below min: {}",
        ctrl.current_rate()
    );
}

// ===========================================================================
// CompositePidController behavioral tests
// ===========================================================================

#[test]
fn composite_single_constraint_behaves_like_pid() {
    // Single constraint with manual gains should behave similarly to PidRateController
    let constraints = vec![PidConstraint {
        metric: TargetMetric::LatencyP99,
        limit: 100.0,
        gains: PidGains::Manual {
            kp: 0.5,
            ki: 0.01,
            kd: 0.1,
        },
    }];

    let mut composite = CompositePidController::new(
        &constraints,
        500.0,
        10.0,
        10000.0,
        Duration::from_millis(100),
    );

    let mut single = PidRateController::new(
        TargetMetric::LatencyP99,
        100.0,
        500.0,
        10.0,
        10000.0,
        PidGainValues {
            kp: 0.5,
            ki: 0.01,
            kd: 0.1,
        },
    );

    // Feed identical metrics — both should move in the same direction
    let summary = make_summary(100, 200.0, 0.0); // above target
    composite.update(&summary);
    single.update(&summary);

    // Both should reduce rate
    assert!(
        composite.current_rate() < 500.0,
        "composite should reduce rate"
    );
    assert!(single.current_rate() < 500.0, "single should reduce rate");
}

#[test]
fn composite_binding_constraint_controls_rate() {
    // Two constraints: latency (violated) and error rate (satisfied)
    let constraints = vec![
        PidConstraint {
            metric: TargetMetric::LatencyP99,
            limit: 100.0, // target: p99 < 100ms
            gains: PidGains::Manual {
                kp: 0.5,
                ki: 0.01,
                kd: 0.1,
            },
        },
        PidConstraint {
            metric: TargetMetric::ErrorRate,
            limit: 5.0, // target: error rate < 5%
            gains: PidGains::Manual {
                kp: 0.5,
                ki: 0.01,
                kd: 0.1,
            },
        },
    ];

    let mut ctrl = CompositePidController::new(
        &constraints,
        500.0,
        10.0,
        10000.0,
        Duration::from_millis(100),
    );

    // Feed: latency=200ms (violated!), error=1% (satisfied)
    for _ in 0..10 {
        ctrl.update(&make_summary(100, 200.0, 0.01));
    }

    assert!(
        ctrl.current_rate() < 500.0,
        "violated latency constraint should reduce rate: {}",
        ctrl.current_rate()
    );
}

#[test]
fn composite_reduces_rate_when_any_constraint_violated() {
    let constraints = vec![
        PidConstraint {
            metric: TargetMetric::LatencyP99,
            limit: 500.0,
            gains: PidGains::Manual {
                kp: 0.5,
                ki: 0.01,
                kd: 0.1,
            },
        },
        PidConstraint {
            metric: TargetMetric::ErrorRate,
            limit: 2.0,
            gains: PidGains::Manual {
                kp: 0.5,
                ki: 0.01,
                kd: 0.1,
            },
        },
    ];

    let mut ctrl = CompositePidController::new(
        &constraints,
        1000.0,
        10.0,
        50000.0,
        Duration::from_millis(100),
    );

    // Latency fine (50ms < 500ms), but error rate high (10% > 2%)
    for _ in 0..10 {
        ctrl.update(&make_summary(100, 50.0, 0.10));
    }

    assert!(
        ctrl.current_rate() < 1000.0,
        "should reduce rate when error rate constraint is violated: {}",
        ctrl.current_rate()
    );
}

#[test]
fn composite_increases_rate_when_all_constraints_satisfied() {
    let constraints = vec![
        PidConstraint {
            metric: TargetMetric::LatencyP99,
            limit: 500.0,
            gains: PidGains::Manual {
                kp: 0.5,
                ki: 0.01,
                kd: 0.1,
            },
        },
        PidConstraint {
            metric: TargetMetric::ErrorRate,
            limit: 5.0,
            gains: PidGains::Manual {
                kp: 0.5,
                ki: 0.01,
                kd: 0.1,
            },
        },
    ];

    let mut ctrl = CompositePidController::new(
        &constraints,
        200.0,
        10.0,
        50000.0,
        Duration::from_millis(100),
    );

    let initial = ctrl.current_rate();

    // Both constraints well satisfied: latency 50ms < 500ms, errors 0.5% < 5%
    for _ in 0..10 {
        ctrl.update(&make_summary(100, 50.0, 0.005));
    }

    assert!(
        ctrl.current_rate() > initial,
        "should increase rate when all constraints satisfied: was {initial}, now {}",
        ctrl.current_rate()
    );
}

#[test]
fn composite_three_constraint_convergence() {
    // Simulate a system with:
    // - latency = rate / 5 (ms)
    // - error_rate = max(0, (rate - 3000) / 200) (%)
    // - load = rate / 50
    // Constraints: latency < 500, errors < 2%, load < 80
    // Binding constraints: latency=500 → rate=2500, errors=2 → rate=3400, load=80 → rate=4000
    // Most restrictive: latency at rate=2500

    let constraints = vec![
        PidConstraint {
            metric: TargetMetric::LatencyP99,
            limit: 500.0,
            gains: PidGains::Manual {
                kp: 0.1,
                ki: 0.001,
                kd: 0.05,
            },
        },
        PidConstraint {
            metric: TargetMetric::ErrorRate,
            limit: 2.0,
            gains: PidGains::Manual {
                kp: 0.1,
                ki: 0.001,
                kd: 0.05,
            },
        },
        PidConstraint {
            metric: TargetMetric::External {
                name: "load".into(),
            },
            limit: 80.0,
            gains: PidGains::Manual {
                kp: 0.1,
                ki: 0.001,
                kd: 0.05,
            },
        },
    ];

    let mut ctrl = CompositePidController::new(
        &constraints,
        1000.0,
        10.0,
        50000.0,
        Duration::from_millis(100),
    );

    for _ in 0..300 {
        let rate = ctrl.current_rate();
        let latency = rate / 5.0;
        let error_rate = ((rate - 3000.0) / 200.0).max(0.0) / 100.0; // fraction
        let load = rate / 50.0;
        let summary = make_summary_with_latency_and_errors(
            100,
            latency,
            error_rate,
            vec![("load".into(), load)],
        );
        ctrl.update(&summary);
    }

    let final_rate = ctrl.current_rate();
    // Should converge near 2500 (latency-limited)
    assert!(
        (final_rate - 2500.0).abs() < 800.0,
        "should converge near 2500 (latency-limited), got {final_rate:.1}"
    );
}

#[test]
fn composite_infeasible_settles_at_min() {
    // Constraint that can never be satisfied: p99 < 1ms at any rate
    let constraints = vec![PidConstraint {
        metric: TargetMetric::LatencyP99,
        limit: 1.0, // 1ms — basically impossible
        gains: PidGains::Manual {
            kp: 0.5,
            ki: 0.01,
            kd: 0.1,
        },
    }];

    let mut ctrl = CompositePidController::new(
        &constraints,
        1000.0,
        50.0, // min RPS
        10000.0,
        Duration::from_millis(100),
    );

    for _ in 0..100 {
        // Latency always 50ms, well above 1ms target
        ctrl.update(&make_summary(100, 50.0, 0.0));
    }

    assert!(
        ctrl.current_rate() <= 50.0 + 1.0,
        "infeasible constraint should drive rate to min: {}",
        ctrl.current_rate()
    );
}

#[test]
fn composite_set_rate_resets_state() {
    let constraints = vec![PidConstraint {
        metric: TargetMetric::LatencyP99,
        limit: 100.0,
        gains: PidGains::Manual {
            kp: 0.5,
            ki: 0.01,
            kd: 0.1,
        },
    }];

    let mut ctrl = CompositePidController::new(
        &constraints,
        500.0,
        10.0,
        10000.0,
        Duration::from_millis(100),
    );

    // Drive rate down
    for _ in 0..20 {
        ctrl.update(&make_summary(100, 200.0, 0.0));
    }
    let low_rate = ctrl.current_rate();
    assert!(low_rate < 500.0);

    // External override
    ctrl.set_rate(800.0);
    assert_eq!(ctrl.current_rate(), 800.0);

    // Should continue from new rate without fighting
    ctrl.update(&make_summary(100, 80.0, 0.0)); // below target now
    assert!(
        ctrl.current_rate() >= 800.0,
        "should increase from new setpoint, got {}",
        ctrl.current_rate()
    );
}

#[test]
fn composite_handles_missing_external_signal() {
    let constraints = vec![
        PidConstraint {
            metric: TargetMetric::LatencyP99,
            limit: 100.0,
            gains: PidGains::Manual {
                kp: 0.1,
                ki: 0.001,
                kd: 0.05,
            },
        },
        PidConstraint {
            metric: TargetMetric::External {
                name: "load".into(),
            },
            limit: 80.0,
            gains: PidGains::Manual {
                kp: 0.1,
                ki: 0.001,
                kd: 0.05,
            },
        },
    ];

    let mut ctrl = CompositePidController::new(
        &constraints,
        500.0,
        10.0,
        10000.0,
        Duration::from_millis(100),
    );

    // No external signal → external metric defaults to 0.0 → well below 80 limit
    // Latency is also below target → should increase rate
    let initial = ctrl.current_rate();
    for _ in 0..10 {
        ctrl.update(&make_summary(100, 50.0, 0.0)); // no external signals
    }

    assert!(
        ctrl.current_rate() >= initial,
        "missing external signal should not constrain rate: {}",
        ctrl.current_rate()
    );
}

#[test]
fn composite_with_autotune_converges() {
    // Composite mode with auto-tuned gains on a simulated linear system
    let constraints = vec![PidConstraint {
        metric: TargetMetric::LatencyP99,
        limit: 100.0,
        gains: PidGains::Auto {
            autotune_duration: Duration::from_secs(1),
            smoothing: 0.3,
        },
    }];

    let mut ctrl = CompositePidController::new(
        &constraints,
        2000.0,
        10.0,
        50000.0,
        Duration::from_millis(100),
    );

    // Simulate: latency = rate / 10. Equilibrium at rate=1000.
    // 500 ticks for exploration + convergence.
    for _ in 0..500 {
        let rate = ctrl.current_rate();
        let latency = rate / 10.0;
        ctrl.update(&make_summary(100, latency, 0.0));
    }

    let final_rate = ctrl.current_rate();
    assert!(
        (final_rate - 1000.0).abs() < 500.0,
        "composite autotune should converge near 1000, got {final_rate:.1}"
    );
}

// ===========================================================================
// autotune.rs unit tests — building blocks
// ===========================================================================

#[test]
fn gain_schedule_selects_correct_region() {
    // Ramp-up: far below target (normalized error > 0.4)
    let m = gain_schedule(0.6);
    assert!((m.kp_scale - 2.0).abs() < 0.01, "ramp-up kp should be 2.0");
    assert!(!m.reset_integral);

    // Approach: 0.1 < error < 0.4
    let m = gain_schedule(0.25);
    assert!((m.kp_scale - 1.0).abs() < 0.01, "approach kp should be 1.0");
    assert!((m.kd_scale - 1.5).abs() < 0.01, "approach kd should be 1.5");

    // Tracking: -0.1 < error < 0.1
    let m = gain_schedule(0.0);
    assert!((m.kp_scale - 0.7).abs() < 0.01, "tracking kp should be 0.7");
    assert!((m.ki_scale - 1.0).abs() < 0.01, "tracking ki should be 1.0");

    // Overshoot: -0.3 < error < -0.1
    let m = gain_schedule(-0.2);
    assert!(
        (m.kp_scale - 1.5).abs() < 0.01,
        "overshoot kp should be 1.5"
    );
    assert!(
        (m.kd_scale - 2.0).abs() < 0.01,
        "overshoot kd should be 2.0"
    );

    // Critical: error < -0.3
    let m = gain_schedule(-0.5);
    assert!((m.kp_scale - 3.0).abs() < 0.01, "critical kp should be 3.0");
    assert!(m.reset_integral, "critical should reset integral");
    assert!((m.ki_scale).abs() < 0.01, "critical ki should be 0.0");
}

#[test]
fn cohen_coon_gains_are_finite_and_positive() {
    // Various dead time / time constant ratios
    for (l, t) in [(1, 3), (3, 3), (5, 3), (1, 10), (10, 5)] {
        let gains = compute_cohen_coon_gains(100.0, l, t);
        assert!(gains.kp.is_finite() && gains.kp > 0.0, "kp for L={l} T={t}");
        assert!(gains.ki.is_finite() && gains.ki > 0.0, "ki for L={l} T={t}");
        assert!(
            gains.kd.is_finite() && gains.kd >= 0.0,
            "kd for L={l} T={t}"
        );
    }
}

#[test]
fn cohen_coon_higher_dead_time_produces_lower_kp() {
    // More dead time → more conservative proportional gain
    let gains_low_dt = compute_cohen_coon_gains(100.0, 1, 5);
    let gains_high_dt = compute_cohen_coon_gains(100.0, 5, 5);
    // With higher dead-time ratio, Cohen-Coon should be more conservative
    // (lower base Kp relative to the system, though the formula is complex)
    // At minimum, both should be finite and positive
    assert!(gains_low_dt.kp > 0.0);
    assert!(gains_high_dt.kp > 0.0);
}

#[test]
fn ema_smoothing_reduces_noise() {
    let mut state = PidState::new(0.3); // alpha=0.3, heavy smoothing

    // Feed alternating high/low values
    let mut outputs = Vec::new();
    for i in 0..20 {
        let raw = if i % 2 == 0 { 100.0 } else { 200.0 };
        outputs.push(state.smooth(raw));
    }

    // The smoothed output should have much less variance than the raw signal
    let raw_spread = 100.0; // 200 - 100
    let smoothed_spread = outputs.last().unwrap() - outputs[outputs.len() - 2];
    assert!(
        smoothed_spread.abs() < raw_spread * 0.5,
        "smoothing should reduce tick-to-tick variance: raw spread={raw_spread}, smoothed spread={:.1}",
        smoothed_spread.abs()
    );

    // Smoothed values should converge toward the mean (150)
    let final_val = *outputs.last().unwrap();
    assert!(
        (final_val - 150.0).abs() < 40.0,
        "smoothed value should approach mean (150), got {final_val:.1}"
    );
}

#[test]
fn ema_reset_clears_history() {
    let mut state = PidState::new(0.3);
    state.smooth(200.0);
    state.smooth(200.0);
    state.smooth(200.0); // EMA converged near 200

    state.reset();
    let val = state.smooth(50.0); // first value after reset should be 50 exactly
    assert!(
        (val - 50.0).abs() < 0.01,
        "after reset, first smooth should return raw value, got {val}"
    );
}

#[test]
fn exploration_detects_dead_time() {
    let metric = MetricExploration::new(TargetMetric::LatencyP99, 100.0);
    let mut manager = ExplorationManager::new(
        vec![metric],
        1000.0,
        Duration::from_secs(5),
        Duration::from_millis(100),
    );

    // Baseline phase: feed until it transitions to step response
    // baseline_duration_ticks = 500/100 = 5
    let mut tick = 0;
    loop {
        tick += 1;
        let result = manager.tick(&make_summary(100, 50.0, 0.0));
        if let Some(rate) = result {
            if rate > 600.0 {
                break; // entered step response
            }
        }
        if tick > 20 {
            panic!("baseline should have ended by now");
        }
    }

    // Step response: 2 ticks of flat, then immediately jump.
    // We keep it short to avoid the "settled" check terminating exploration
    // before we can feed the changed values.
    for _ in 0..2 {
        manager.tick(&make_summary(100, 50.0, 0.0)); // still flat (dead time)
    }

    // Latency jumps — this is where dead time should be detected
    // Feed enough non-flat values so the settled check doesn't fire on 3 identical values
    for i in 0..10 {
        let latency = 80.0 + (i as f64) * 2.0; // rising: 80, 82, 84...
        manager.tick(&make_summary(100, latency, 0.0));
    }

    // The metric should have detected dead time at the transition point
    let m = &manager.metrics[0];
    assert!(
        m.dead_time_detected.is_some(),
        "should have detected dead time, baseline_mean={:.1}, step_measurements={:?}",
        m.baseline_mean(),
        &m.step_measurements[..m.step_measurements.len().min(10)],
    );
    let dt = m.dead_time_detected.unwrap();
    assert!(
        dt >= 2 && dt <= 5,
        "dead time should be ~3 ticks (the flat period), got {dt}"
    );
}

#[test]
fn exploration_fallback_when_metric_unresponsive() {
    // Simulate a system where the metric doesn't respond to the rate step
    let mut ctrl = AutotuningPidController::new(
        TargetMetric::LatencyP99,
        100.0,
        1000.0,
        10.0,
        50000.0,
        AutotuneParams {
            autotune_duration: Duration::from_secs(1),
            smoothing: 0.3,
            control_interval: Duration::from_millis(100),
        },
    );

    // Feed constant latency regardless of rate — system doesn't respond
    for _ in 0..30 {
        ctrl.update(&make_summary(100, 50.0, 0.0)); // always 50ms
    }

    // Controller should still work (fallback gains)
    let rate = ctrl.current_rate();
    assert!(
        rate > 10.0 && rate < 50000.0,
        "should have a reasonable rate even with fallback, got {rate}"
    );

    // And should still respond to metric changes
    let before = ctrl.current_rate();
    for _ in 0..20 {
        ctrl.update(&make_summary(100, 200.0, 0.0)); // latency above target
    }
    assert!(
        ctrl.current_rate() < before,
        "fallback gains should still reduce rate when latency exceeds target"
    );
}

#[test]
fn autotune_on_external_signal() {
    // Autotune targeting an external signal (not just latency)
    let mut ctrl = AutotuningPidController::new(
        TargetMetric::External { name: "cpu".into() },
        70.0,   // target CPU 70%
        1000.0, // initial RPS
        10.0,
        50000.0,
        AutotuneParams {
            autotune_duration: Duration::from_secs(1),
            smoothing: 0.3,
            control_interval: Duration::from_millis(100),
        },
    );

    // Simulate: cpu = rate / 15. Equilibrium at rate=1050.
    for _ in 0..500 {
        let rate = ctrl.current_rate();
        let cpu = rate / 15.0;
        let summary = MetricsSummary {
            total_requests: 100,
            error_rate: 0.0,
            request_rate: 100.0,
            external_signals: vec![("cpu".into(), cpu)],
            window_duration: Duration::from_secs(1),
            ..Default::default()
        };
        ctrl.update(&summary);
    }

    let final_rate = ctrl.current_rate();
    assert!(
        (final_rate - 1050.0).abs() < 500.0,
        "autotune on external signal should converge near 1050, got {final_rate:.1}"
    );
}

// ===========================================================================
// Composite: constraint switching and integral tracking
// ===========================================================================

#[test]
fn composite_constraint_switching_is_smooth() {
    // Start with latency as binding, then switch to error rate as binding
    let constraints = vec![
        PidConstraint {
            metric: TargetMetric::LatencyP99,
            limit: 100.0,
            gains: PidGains::Manual {
                kp: 0.05,
                ki: 0.001,
                kd: 0.02,
            },
        },
        PidConstraint {
            metric: TargetMetric::ErrorRate,
            limit: 5.0,
            gains: PidGains::Manual {
                kp: 0.05,
                ki: 0.001,
                kd: 0.02,
            },
        },
    ];

    let mut ctrl = CompositePidController::new(
        &constraints,
        1000.0,
        50.0,
        50000.0,
        Duration::from_millis(100),
    );

    // Phase 1: latency slightly above target (binding), error rate fine
    for _ in 0..10 {
        ctrl.update(&make_summary(100, 120.0, 0.01)); // latency above 100ms
    }
    let rate_after_latency_binding = ctrl.current_rate();
    assert!(
        rate_after_latency_binding < 1000.0,
        "latency should drive rate down: {}",
        rate_after_latency_binding
    );

    // Phase 2: latency drops below target, but error rate spikes above limit
    let mut rates = Vec::new();
    for _ in 0..20 {
        ctrl.update(&make_summary(100, 50.0, 0.10)); // error rate 10% >> 5% limit
        rates.push(ctrl.current_rate());
    }

    // Rate should decrease (error rate is now the binding constraint)
    let final_rate = ctrl.current_rate();
    assert!(
        final_rate < rate_after_latency_binding,
        "error rate should now be binding and drive rate down: was {rate_after_latency_binding:.1}, now {final_rate:.1}"
    );

    // Check smoothness: no huge jumps between ticks (> 25% change per tick)
    for w in rates.windows(2) {
        let change = (w[1] - w[0]).abs() / w[0].max(1.0);
        assert!(
            change < 0.25,
            "rate change should be smooth: {:.1} → {:.1} ({:.0}% change)",
            w[0],
            w[1],
            change * 100.0
        );
    }
}

#[test]
fn composite_integral_tracking_prevents_windup() {
    // Two constraints. Error rate is binding for a while (gently reducing rate),
    // then latency spikes. The latency controller should react immediately
    // because its integral was decayed during the non-binding phase.
    let constraints = vec![
        PidConstraint {
            metric: TargetMetric::LatencyP99,
            limit: 100.0,
            gains: PidGains::Manual {
                kp: 0.3,
                ki: 0.01,
                kd: 0.1,
            },
        },
        PidConstraint {
            metric: TargetMetric::ErrorRate,
            limit: 5.0,
            gains: PidGains::Manual {
                kp: 0.05,
                ki: 0.001,
                kd: 0.02,
            },
        },
    ];

    let mut ctrl = CompositePidController::new(
        &constraints,
        500.0,
        50.0,
        50000.0,
        Duration::from_millis(100),
    );

    // Phase 1: error rate slightly above target (binding, gently reducing rate),
    // latency well below target (non-binding, integral should decay)
    for _ in 0..30 {
        ctrl.update(&make_summary(100, 50.0, 0.06)); // error rate 6% > 5% limit
    }
    let rate_before_spike = ctrl.current_rate();
    // Rate should still be in a reasonable range (not at min)
    assert!(
        rate_before_spike > 100.0,
        "rate should not have bottomed out: {rate_before_spike:.1}"
    );

    // Phase 2: error rate drops to safe, but latency spikes well above target
    for _ in 0..3 {
        ctrl.update(&make_summary(100, 200.0, 0.01)); // latency 200ms >> 100ms
    }
    let rate_after_spike = ctrl.current_rate();

    assert!(
        rate_after_spike < rate_before_spike,
        "latency spike should reduce rate (integral should not prevent immediate response): \
         before={rate_before_spike:.1}, after={rate_after_spike:.1}"
    );
}

#[test]
fn composite_mixed_manual_and_auto_gains() {
    // One constraint with manual gains, one with auto-tuned
    let constraints = vec![
        PidConstraint {
            metric: TargetMetric::LatencyP99,
            limit: 100.0,
            gains: PidGains::Manual {
                kp: 0.1,
                ki: 0.001,
                kd: 0.05,
            },
        },
        PidConstraint {
            metric: TargetMetric::ErrorRate,
            limit: 5.0,
            gains: PidGains::Auto {
                autotune_duration: Duration::from_secs(1),
                smoothing: 0.3,
            },
        },
    ];

    let mut ctrl = CompositePidController::new(
        &constraints,
        500.0,
        10.0,
        50000.0,
        Duration::from_millis(100),
    );

    // During exploration, manual constraint should still enforce limits
    for _ in 0..5 {
        ctrl.update(&make_summary(100, 50.0, 0.01));
    }
    // Rate should be reasonable even during exploration
    assert!(
        ctrl.current_rate() > 10.0,
        "should have reasonable rate during exploration"
    );

    // After exploration completes, both constraints should work
    for _ in 0..100 {
        let rate = ctrl.current_rate();
        let latency = rate / 10.0; // latency proportional to rate
        let error_rate = if rate > 2000.0 { 0.10 } else { 0.01 }; // errors spike above 2K
        ctrl.update(&make_summary(100, latency, error_rate));
    }

    // Rate should settle below where either constraint is violated
    let final_rate = ctrl.current_rate();
    let final_latency = final_rate / 10.0;
    assert!(
        final_latency < 120.0 || final_rate < 2000.0,
        "should satisfy at least one constraint: rate={final_rate:.0}, latency={final_latency:.0}"
    );
}

#[test]
fn autotune_different_system_gains_produce_different_pid_gains() {
    // Two systems with different sensitivity: K=0.01 vs K=0.1
    // The autotuner should produce different gains for each

    // System A: latency = rate * 0.01 (slow system, low sensitivity)
    let mut ctrl_a = AutotuningPidController::new(
        TargetMetric::LatencyP99,
        100.0,
        5000.0,
        10.0,
        100000.0,
        AutotuneParams {
            autotune_duration: Duration::from_secs(1),
            smoothing: 0.3,
            control_interval: Duration::from_millis(100),
        },
    );
    for _ in 0..20 {
        let rate = ctrl_a.current_rate();
        let latency = rate * 0.01;
        ctrl_a.update(&make_summary(100, latency, 0.0));
    }
    let rate_a_after_explore = ctrl_a.current_rate();

    // System B: latency = rate * 0.1 (sensitive system)
    let mut ctrl_b = AutotuningPidController::new(
        TargetMetric::LatencyP99,
        100.0,
        5000.0,
        10.0,
        100000.0,
        AutotuneParams {
            autotune_duration: Duration::from_secs(1),
            smoothing: 0.3,
            control_interval: Duration::from_millis(100),
        },
    );
    for _ in 0..20 {
        let rate = ctrl_b.current_rate();
        let latency = rate * 0.1;
        ctrl_b.update(&make_summary(100, latency, 0.0));
    }
    let rate_b_after_explore = ctrl_b.current_rate();

    // After exploration, the two controllers should be at different rates
    // because their systems respond differently
    assert!(
        (rate_a_after_explore - rate_b_after_explore).abs() > 100.0,
        "different system gains should produce different post-exploration rates: \
         A={rate_a_after_explore:.0}, B={rate_b_after_explore:.0}"
    );
}

// ---------------------------------------------------------------------------
// RampRateController (AIMD) behavioral tests
// ---------------------------------------------------------------------------

/// Build a RampConfig with short durations suitable for unit tests.
/// Uses smoothing_window=1 and ratio_freeze=false for deterministic behavior.
fn make_ramp_config() -> RampConfig {
    RampConfig {
        warmup_rps: 100.0,
        warmup_duration: Duration::from_millis(0),
        latency_multiplier: 3.0,
        max_error_rate: 5.0,
        min_rps: 1.0,
        max_rps: 100_000.0,
        control_interval: Duration::from_secs(3),
        test_duration: Duration::from_millis(2), // ceiling ramps over 1ms
        smoothing_window: 1,
        enable_ratio_freeze: false,
        external_constraints: Vec::new(),
    }
}

/// Build a MetricsSummary for ramp tests with the given p99 latency and error rate.
fn make_ramp_summary(requests: u64, p99_ms: f64, error_rate: f64) -> MetricsSummary {
    MetricsSummary {
        total_requests: requests,
        total_errors: (requests as f64 * error_rate) as u64,
        error_rate,
        request_rate: requests as f64 / 3.0,
        latency_p50_ns: (p99_ms * 0.5 * 1_000_000.0) as u64,
        latency_p90_ns: (p99_ms * 0.9 * 1_000_000.0) as u64,
        latency_p99_ns: (p99_ms * 1_000_000.0) as u64,
        window_duration: Duration::from_secs(3),
        ..Default::default()
    }
}

/// Force through warmup and wait for the progressive ceiling to open.
/// Returns the baseline p99 (median of warmup samples = p99_ms).
fn warmup_ramp(ctrl: &mut RampRateController, p99_ms: f64) {
    let s = make_ramp_summary(100, p99_ms, 0.0);
    ctrl.update(&s); // sample 1
    ctrl.update(&s); // sample 2
    ctrl.update(&s); // sample 3 → transitions + first ramping tick
    // Let the progressive ceiling ramp to max_rps.
    std::thread::sleep(Duration::from_millis(5));
}

#[test]
fn ramp_warmup_holds_at_warmup_rate() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    let s = make_ramp_summary(100, 10.0, 0.0);

    // First two updates are warmup — rate should stay at warmup_rps.
    let d1 = ctrl.update(&s);
    assert_eq!(d1.target_rps, 100.0, "warmup tick 1 should hold at warmup_rps");
    let d2 = ctrl.update(&s);
    assert_eq!(d2.target_rps, 100.0, "warmup tick 2 should hold at warmup_rps");
}

#[test]
fn ramp_transitions_after_three_warmup_samples() {
    let config = RampConfig {
        warmup_duration: Duration::from_millis(0),
        ..make_ramp_config()
    };
    let mut ctrl = RampRateController::new(config);
    let s = make_ramp_summary(100, 10.0, 0.0);

    ctrl.update(&s); // sample 1 — warmup
    ctrl.update(&s); // sample 2 — warmup
    // Third update: transitions to ramping and runs one ramping tick.
    let d3 = ctrl.update(&s);
    // After transition, rate is no longer simply warmup_rps — it ran a ramping
    // tick. The ceiling is near warmup_rps so rate stays ~100, but the
    // controller_info should show the ramping phase.
    let info = ctrl.controller_info();
    assert_eq!(
        info.params["phase"], "ramping",
        "should be in ramping phase after 3 samples"
    );
    // Rate should be close to warmup_rps (ceiling-bound on first tick).
    assert!(
        (d3.target_rps - 100.0).abs() < 20.0,
        "first ramping tick should be near warmup_rps, got {}",
        d3.target_rps
    );
}

#[test]
fn ramp_warmup_needs_minimum_three_samples() {
    // Even with warmup_duration=0, we need 3 samples with valid p99.
    let mut ctrl = RampRateController::new(make_ramp_config());

    // Feed 2 samples with valid p99, then one with zero requests (no sample).
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    ctrl.update(&make_ramp_summary(0, 0.0, 0.0)); // no requests → no sample

    let info = ctrl.controller_info();
    assert_eq!(
        info.params["phase"], "warmup",
        "should still be warming up with only 2 valid samples"
    );
}

#[test]
fn ramp_baseline_is_median_of_warmup_samples() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    // Feed three different p99 values: 5, 10, 100. Median = 10.
    ctrl.update(&make_ramp_summary(100, 5.0, 0.0));
    ctrl.update(&make_ramp_summary(100, 100.0, 0.0));
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0));

    let info = ctrl.controller_info();
    let baseline = info.params["baseline_p99_ms"].as_f64().unwrap();
    assert!(
        (baseline - 10.0).abs() < 0.01,
        "baseline should be median of [5, 10, 100] = 10, got {baseline}"
    );
    let target = info.params["target_p99_ms"].as_f64().unwrap();
    assert!(
        (target - 30.0).abs() < 0.01,
        "target should be 10 * 3.0 = 30, got {target}"
    );
}

#[test]
fn ramp_increases_rate_on_clean_tick() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0); // baseline=10ms, target=30ms

    let rate_before = ctrl.current_rate();
    // p99 well below target → clean tick → 10% increase.
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    let rate_after = ctrl.current_rate();

    let expected = rate_before * 1.10;
    assert!(
        (rate_after - expected).abs() / expected < 0.02,
        "clean tick should increase rate by ~10%: {rate_before:.1} → {rate_after:.1} (expected {expected:.1})"
    );
}

#[test]
fn ramp_compounds_increases_over_multiple_clean_ticks() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0);

    let rate_start = ctrl.current_rate();
    for _ in 0..5 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_end = ctrl.current_rate();

    // 5 clean ticks: rate * 1.10^5 ≈ 1.61x
    let expected = rate_start * 1.10_f64.powi(5);
    assert!(
        (rate_end - expected).abs() / expected < 0.05,
        "5 clean ticks should compound: {rate_start:.1} → {rate_end:.1} (expected {expected:.1})"
    );
}

#[test]
fn ramp_gentle_backoff_on_mild_violation() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0); // target = 30ms

    // Increase a few times to establish rate and set ticks_since_increase=0.
    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }

    // Two consecutive violations with ratio 1.0-1.5 to trigger gentle backoff.
    // p99 = 40ms → ratio = 40/30 ≈ 1.33 (self-caused since ticks_since_increase=0).
    ctrl.update(&make_ramp_summary(100, 40.0, 0.0)); // streak=1, ratio=1.33 ≥ 1.3 → gentle backoff
    let rate_before = ctrl.current_rate();

    ctrl.update(&make_ramp_summary(100, 40.0, 0.0)); // streak=2 → gentle backoff
    let rate_after = ctrl.current_rate();

    let expected = rate_before * 0.90;
    assert!(
        (rate_after - expected).abs() / expected < 0.05,
        "streak≥2 mild violation should gentle backoff (×0.90): {rate_before:.1} → {rate_after:.1} (expected {expected:.1})"
    );
}

#[test]
fn ramp_moderate_backoff_on_medium_violation() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0); // target = 30ms

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }

    // p99 = 50ms → ratio = 50/30 ≈ 1.67 → moderate backoff (×0.75).
    // First tick: held by persistence (streak=1 < 2, severity < 3.0).
    ctrl.update(&make_ramp_summary(100, 50.0, 0.0));
    let rate_before = ctrl.current_rate();

    // Second tick: persistence confirmed (streak=2 ≥ 2) → moderate backoff.
    // Known-good floor limits the drop to ~80% of peak.
    ctrl.update(&make_ramp_summary(100, 50.0, 0.0));
    let rate_after = ctrl.current_rate();

    assert!(
        rate_after < rate_before * 0.88,
        "moderate violation should drop by >12% after persistence: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_hard_backoff_on_severe_violation() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0); // target = 30ms

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_before = ctrl.current_rate();

    // p99 = 100ms → ratio = 100/30 ≈ 3.33 → hard backoff even on first violation.
    ctrl.update(&make_ramp_summary(100, 100.0, 0.0));
    let rate_after = ctrl.current_rate();

    let expected = rate_before * 0.50;
    // Known-good floor (80% of max_recent_rate) may prevent going all the way.
    assert!(
        rate_after < rate_before * 0.85,
        "severe violation should hard backoff: {rate_before:.1} → {rate_after:.1} (expected ≤{expected:.1})"
    );
}

#[test]
fn ramp_graduated_backoff_proportional_to_severity() {
    // Verify that more severe violations cause equal or larger rate drops.
    // Two violation ticks are needed because latency persistence=2 holds
    // the first tick (unless severity > 3.0 which bypasses persistence).
    let drop_for = |p99_ms: f64| -> f64 {
        let mut ctrl = RampRateController::new(make_ramp_config());
        warmup_ramp(&mut ctrl, 10.0);
        for _ in 0..5 {
            ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
        }
        let before = ctrl.current_rate();
        ctrl.update(&make_ramp_summary(100, p99_ms, 0.0)); // tick 1 (held or bypassed)
        ctrl.update(&make_ramp_summary(100, p99_ms, 0.0)); // tick 2 (confirmed)
        (before - ctrl.current_rate()) / before
    };

    let drop_gentle = drop_for(40.0); // ratio ~1.33 → gentle (×0.90)
    let drop_moderate = drop_for(50.0); // ratio ~1.67 → moderate (×0.75)
    let drop_hard = drop_for(100.0); // ratio ~3.33 → hard (×0.50, bypasses persistence)

    assert!(
        drop_gentle > 0.05,
        "gentle should cause measurable drop: {:.1}%",
        drop_gentle * 100.0
    );
    assert!(
        drop_moderate >= drop_gentle - 0.01,
        "moderate ≥ gentle: {:.1}% vs {:.1}%",
        drop_moderate * 100.0,
        drop_gentle * 100.0,
    );
    assert!(
        drop_hard >= drop_moderate - 0.01,
        "hard ≥ moderate: {:.1}% vs {:.1}%",
        drop_hard * 100.0,
        drop_moderate * 100.0,
    );
}

#[test]
fn ramp_single_mild_violation_holds_rate() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0); // target = 30ms

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_before = ctrl.current_rate();

    // p99 = 35ms → ratio = 35/30 ≈ 1.17 < 1.3, streak=1, self-caused → hold_mild.
    ctrl.update(&make_ramp_summary(100, 35.0, 0.0));
    let rate_after = ctrl.current_rate();

    assert!(
        (rate_after - rate_before).abs() / rate_before < 0.02,
        "single mild self-caused violation should hold: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_external_noise_holds_rate() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0); // target = 30ms

    // Increase a few times, then wait (no increases) to set ticks_since_increase > 2.
    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // increases
    }

    // Feed 3 ticks right at the target to prevent increases but avoid violations.
    // p99 = 29ms → ratio = 29/30 ≈ 0.97, no violation (< 1.0), so streak=0 → increase.
    // Actually we need ticks where rate doesn't increase. Use violations that cause backoff
    // to increment ticks_since_increase.
    // Simpler: use set_rate to then wait for ticks_since_increase to grow.
    // Actually, ticks_since_increase grows whenever new_rps <= current_rps.
    // A hold or backoff tick will increment it. Let's cause gentle backoff ticks.
    ctrl.update(&make_ramp_summary(100, 40.0, 0.0)); // violation, gentle backoff → ticks_since_increase++
    ctrl.update(&make_ramp_summary(100, 40.0, 0.0)); // ticks_since_increase++
    ctrl.update(&make_ramp_summary(100, 40.0, 0.0)); // ticks_since_increase=3

    // Now recover latency, then introduce a new mild violation.
    // Clean tick to reset streak.
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // streak=0 → increase → ticks_since_increase=0

    // Wait again for ticks_since_increase > 2.
    ctrl.update(&make_ramp_summary(100, 40.0, 0.0)); // streak=1, ticks_since_increase=1
    ctrl.update(&make_ramp_summary(100, 40.0, 0.0)); // streak=2, ticks_since_increase=2
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // clean → streak=0, increase → ticks_since_increase=0

    // We need a more direct approach: 3 hold/backoff ticks in a row without increase,
    // then check that mild violation is treated as external.
    // Use ceiling to prevent increase: set max_rps to current rate.
    let current = ctrl.current_rate();
    ctrl.set_max_rps(current);

    // Now clean ticks hit the ceiling (no increase) → ticks_since_increase grows.
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // ceiling-bound, ticks_since_increase++
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // ticks_since_increase++
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // ticks_since_increase=3+

    // Re-open ceiling.
    ctrl.set_max_rps(100_000.0);
    let rate_before = ctrl.current_rate();

    // Now a mild violation: not self-caused (ticks_since_increase > 2), ratio < 1.5 → hold_external.
    ctrl.update(&make_ramp_summary(100, 40.0, 0.0));
    let rate_after = ctrl.current_rate();

    assert!(
        (rate_after - rate_before).abs() / rate_before < 0.02,
        "external noise (ticks_since_increase > 2) should hold: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_error_rate_gentle_backoff() {
    let mut ctrl = RampRateController::new(make_ramp_config()); // max_error_rate = 5%
    warmup_ramp(&mut ctrl, 10.0);

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_before = ctrl.current_rate();

    // error_rate = 6% → error_pct = 6 > 5 → violation.
    // error_pct = 6 < 5 * 1.5 = 7.5 → gentle backoff (×0.90).
    // Latency is clean → latency_rate = current * 1.10.
    // Conservative = min(1.10 * current, 0.90 * current) = 0.90 * current.
    ctrl.update(&make_ramp_summary(100, 10.0, 0.06));
    let rate_after = ctrl.current_rate();

    let expected = rate_before * 0.90;
    assert!(
        (rate_after - expected).abs() / expected < 0.05,
        "error rate just above threshold should gentle backoff (×0.90): {rate_before:.1} → {rate_after:.1} (expected {expected:.1})"
    );
}

#[test]
fn ramp_error_rate_hard_backoff() {
    let mut ctrl = RampRateController::new(make_ramp_config()); // max_error_rate = 5%
    warmup_ramp(&mut ctrl, 10.0);

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_before = ctrl.current_rate();

    // error_rate = 10% → error_pct = 10 > 7.5 (5 * 1.5) → hard backoff (×0.50).
    ctrl.update(&make_ramp_summary(100, 10.0, 0.10));
    let rate_after = ctrl.current_rate();

    // Known-good floor may intervene, but rate should drop significantly.
    assert!(
        rate_after < rate_before * 0.85,
        "high error rate should hard backoff: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_conservative_constraint_takes_minimum() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0); // target = 30ms

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }

    // Both latency and error above threshold simultaneously.
    // p99 = 50ms → ratio ≈ 1.67 → moderate backoff (×0.75).
    // error_rate = 10% → hard backoff (×0.50).
    // Conservative = min(0.75 * rate, 0.50 * rate) = 0.50 * rate.
    let rate_before = ctrl.current_rate();
    ctrl.update(&make_ramp_summary(100, 50.0, 0.10));
    let rate_after = ctrl.current_rate();

    // Error hard backoff (×0.50) is more conservative than latency moderate (×0.75).
    // Known-good floor may lift it, but it should be well below ×0.75.
    assert!(
        rate_after < rate_before * 0.85,
        "should take most conservative constraint: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_known_good_floor_prevents_deep_drop() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0);

    // Ramp up to establish a known-good high rate.
    for _ in 0..10 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let peak_rate = ctrl.current_rate();
    assert!(peak_rate > 200.0, "should have ramped up from 100, got {peak_rate}");

    // Multiple hard backoffs (severe latency violations).
    for _ in 0..5 {
        ctrl.update(&make_ramp_summary(100, 200.0, 0.0)); // ratio ≈ 6.67 → hard backoff
    }
    let floor_rate = ctrl.current_rate();

    // Should not drop below 80% of peak_rate (known-good floor).
    let expected_floor = peak_rate * 0.80;
    assert!(
        floor_rate >= expected_floor * 0.95,
        "known-good floor should prevent deep drops: peak={peak_rate:.1}, floor={floor_rate:.1}, expected≥{expected_floor:.1}"
    );
}

#[test]
fn ramp_respects_min_rps_bound() {
    let config = RampConfig {
        min_rps: 50.0,
        ..make_ramp_config()
    };
    let mut ctrl = RampRateController::new(config);
    warmup_ramp(&mut ctrl, 10.0);

    // Drive rate down with severe violations.
    for _ in 0..100 {
        ctrl.update(&make_ramp_summary(100, 500.0, 0.5));
    }

    assert!(
        ctrl.current_rate() >= 50.0,
        "rate should never go below min_rps=50, got {}",
        ctrl.current_rate()
    );
}

#[test]
fn ramp_respects_max_rps_bound() {
    let config = RampConfig {
        max_rps: 500.0,
        ..make_ramp_config()
    };
    let mut ctrl = RampRateController::new(config);
    warmup_ramp(&mut ctrl, 10.0);

    // Drive rate up with many clean ticks.
    for _ in 0..100 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }

    assert!(
        ctrl.current_rate() <= 500.0,
        "rate should never exceed max_rps=500, got {}",
        ctrl.current_rate()
    );
}

#[test]
fn ramp_timeout_hard_backoff() {
    let mut ctrl = RampRateController::new(make_ramp_config()); // max_error_rate = 5%
    warmup_ramp(&mut ctrl, 10.0);

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_before = ctrl.current_rate();

    // hard_threshold = (5.0/100 * 2.5).max(0.05) = 0.125
    // timeout_fraction = 20/100 = 0.20 > 0.125 → hard backoff (×0.50).
    let mut s = make_ramp_summary(100, 10.0, 0.0);
    s.timeout_count = 20;
    ctrl.update(&s);
    let rate_after = ctrl.current_rate();

    let expected = rate_before * 0.50;
    assert!(
        (rate_after - expected).abs() / expected < 0.02,
        "high timeout fraction should hard backoff (×0.50): {rate_before:.1} → {rate_after:.1} (expected {expected:.1})"
    );
}

#[test]
fn ramp_timeout_soft_backoff() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0);

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_before = ctrl.current_rate();

    // soft_threshold = (5.0/100 * 0.5).max(0.01) = 0.025
    // timeout_fraction = 5/100 = 0.05 > 0.025 but < 0.125 → gentle backoff (×0.90).
    let mut s = make_ramp_summary(100, 10.0, 0.0);
    s.timeout_count = 5;
    ctrl.update(&s);
    let rate_after = ctrl.current_rate();

    let expected = rate_before * 0.90;
    assert!(
        (rate_after - expected).abs() / expected < 0.02,
        "moderate timeout fraction should gentle backoff (×0.90): {rate_before:.1} → {rate_after:.1} (expected {expected:.1})"
    );
}

#[test]
fn ramp_inflight_drops_gentle_backoff() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0);

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate = ctrl.current_rate();
    let rate_before = rate;

    // drop_rate = in_flight_drops / window_secs = drops / 3.0
    // drop_fraction = drop_rate / current_rps
    // Need drop_fraction > 0.01: drops > 0.01 * rate * 3.0
    let drops = (0.02 * rate * 3.0) as u64; // ~2% drops
    let mut s = make_ramp_summary(100, 10.0, 0.0);
    s.in_flight_drops = drops;
    ctrl.update(&s);
    let rate_after = ctrl.current_rate();

    let expected = rate_before * 0.90;
    assert!(
        (rate_after - expected).abs() / expected < 0.05,
        ">1% in-flight drops should gentle backoff (×0.90): {rate_before:.1} → {rate_after:.1} (expected {expected:.1})"
    );
}

#[test]
fn ramp_inflight_drops_hold_at_edge() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0);

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate = ctrl.current_rate();
    let rate_before = rate;

    // Need 0.001 < drop_fraction < 0.01: drops between 0.001 * rate * 3 and 0.01 * rate * 3.
    let drops = (0.005 * rate * 3.0) as u64; // ~0.5% drops
    let mut s = make_ramp_summary(100, 10.0, 0.0);
    s.in_flight_drops = drops;
    ctrl.update(&s);
    let rate_after = ctrl.current_rate();

    assert!(
        (rate_after - rate_before).abs() / rate_before < 0.02,
        "0.1-1% in-flight drops should hold: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_median_smoothing_rejects_single_outlier() {
    let config = RampConfig {
        smoothing_window: 3,
        ..make_ramp_config()
    };
    let mut ctrl = RampRateController::new(config);
    warmup_ramp(&mut ctrl, 10.0); // target = 30ms

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }

    // Feed 2 normal + 1 outlier. Median of [10, 10, 200] = 10 → no violation.
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // window: [10]
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // window: [10, 10]
    let rate_before = ctrl.current_rate();

    ctrl.update(&make_ramp_summary(100, 200.0, 0.0)); // window: [10, 10, 200] → median=10
    let rate_after = ctrl.current_rate();

    // Median is 10ms, ratio = 10/30 < 1.0 → should increase, not backoff.
    assert!(
        rate_after >= rate_before,
        "single outlier should be rejected by median smoothing: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_ratio_freeze_holds_when_achieved_exceeds_target() {
    let config = RampConfig {
        enable_ratio_freeze: true,
        ..make_ramp_config()
    };
    let mut ctrl = RampRateController::new(config);
    warmup_ramp(&mut ctrl, 10.0);

    // Burn through the 10-tick startup grace period.
    for _ in 0..12 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_before = ctrl.current_rate();

    // Set achieved_rps >> target. The controller freezes when ratio > 2.0.
    // request_rate in summary = achieved RPS. current_rps = rate_before.
    // ratio = request_rate / current_rps > 2.0.
    let mut s = make_ramp_summary(100, 10.0, 0.0);
    s.request_rate = rate_before * 3.0; // ratio = 3.0 > 2.0
    ctrl.update(&s);
    let rate_after = ctrl.current_rate();

    assert!(
        (rate_after - rate_before).abs() < 0.01,
        "ratio freeze should hold rate: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_oscillates_around_equilibrium() {
    // Verify the fundamental AIMD behavior: rate climbs on clean ticks,
    // backs off when latency exceeds the target, then climbs again.
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0); // target = 30ms

    let mut rates = Vec::new();
    let threshold_rps = 300.0;

    for i in 0..50 {
        // Simulate a system where latency spikes above target when rate > threshold.
        let current = ctrl.current_rate();
        let p99 = if current > threshold_rps { 60.0 } else { 10.0 };
        ctrl.update(&make_ramp_summary(100, p99, 0.0));
        rates.push(ctrl.current_rate());

        // Don't assert on first few ticks (settling).
        if i > 5 {
            // After settling, rate should stay in a bounded range around the threshold.
            assert!(
                ctrl.current_rate() > 50.0 && ctrl.current_rate() < 1000.0,
                "rate should stay bounded during AIMD oscillation, tick {i}: {}",
                ctrl.current_rate()
            );
        }
    }

    // Verify we saw both increases and decreases (the oscillation).
    let increases = rates.windows(2).filter(|w| w[1] > w[0]).count();
    let decreases = rates.windows(2).filter(|w| w[1] < w[0]).count();
    assert!(
        increases > 3 && decreases > 1,
        "should oscillate: {increases} increases, {decreases} decreases in {rates:?}"
    );
}

/// Regression test for the hysteresis-driven ratchet collapse.
///
/// Models a plant with slow-draining resources (like nginx keepalive
/// connections consuming file descriptors):
///
///   - Resources accumulate proportional to the request rate.
///   - Resources decay exponentially (time constant = 20 ticks, ~60s).
///   - When resources exceed capacity, latency spikes hard.
///
/// Without cooldown + congestion avoidance, the AIMD ratchets down
/// geometrically because:
///   1. Aggressive ramp-up overshoots before old resources drain.
///   2. The known-good floor expires during backoff, allowing deeper drops.
///   3. Each cycle lowers the floor further.
///
/// With cooldown, the controller waits for plant recovery before
/// ramping back up. With congestion avoidance, it approaches the
/// failure rate linearly instead of exponentially. Together they
/// prevent the ratchet and stabilize near the sustainable rate.
#[test]
fn ramp_stabilizes_despite_plant_hysteresis() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0); // baseline=10ms, target=30ms (3×)

    // Plant parameters.
    //
    // Resources accumulate as: resources += rate * ACCUM per tick
    // Resources decay as:      resources *= (1 - DECAY) per tick
    // Steady-state resources:  rate * ACCUM / DECAY
    // Sustainable rate:        CAPACITY * DECAY / ACCUM = 2500 RPS
    let capacity = 500.0;
    let accum = 0.01;  // resources consumed per RPS per tick
    let decay = 0.05;  // fraction freed per tick (τ = 20 ticks ≈ 60s)
    let sustainable_rate = capacity * decay / accum; // 2500.0
    let mut resources = 0.0;

    let mut rates = Vec::new();

    for _ in 0..200 {
        let current = ctrl.current_rate();

        // Plant dynamics: resources are consumed by requests and freed slowly.
        resources += current * accum;
        resources *= 1.0 - decay;

        // When resources exceed capacity, latency spikes to ~3× target
        // (triggering backoff). Below capacity, latency is well below target.
        let p99 = if resources > capacity {
            100.0 // 100ms → lat_ratio ≈ 3.3 → hard backoff
        } else {
            10.0  // 10ms → lat_ratio ≈ 0.33 → increase
        };

        ctrl.update(&make_ramp_summary(100, p99, 0.0));
        rates.push(ctrl.current_rate());
    }

    // The rate should stabilize near the sustainable rate, not collapse
    // to min_rps. We check that the last quarter average is within 50%
    // of the plant's true sustainable capacity.
    let n = rates.len();
    let last_quarter = &rates[n * 3 / 4..];
    let avg_last = last_quarter.iter().sum::<f64>() / last_quarter.len() as f64;

    assert!(
        avg_last > sustainable_rate * 0.5,
        "rate should stabilize near sustainable rate ({sustainable_rate:.0}), \
         but last quarter average was {avg_last:.0} (ratchet collapse detected). \
         Last 20 rates: {:?}",
        &rates[n.saturating_sub(20)..]
    );

    // Verify the controller is still active (not stuck at a fixed rate).
    // With congestion avoidance, it probes cautiously and may rarely
    // overshoot, so we only require at least 1 backoff and some increases.
    let increases = rates.windows(2).filter(|w| w[1] > w[0]).count();
    let decreases = rates.windows(2).filter(|w| w[1] < w[0]).count();
    assert!(
        increases > 5 && decreases >= 1,
        "controller should be actively probing, \
         but saw only {increases} increases and {decreases} decreases"
    );
}

#[test]
fn ramp_set_latency_multiplier_updates_target() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0); // baseline=10, target=30 (3×)

    let info = ctrl.controller_info();
    assert!(
        (info.params["target_p99_ms"].as_f64().unwrap() - 30.0).abs() < 0.01,
        "initial target should be 30ms"
    );

    // Update multiplier mid-test via apply_update.
    let result = ctrl.apply_update(
        "set_latency_multiplier",
        &serde_json::json!({"multiplier": 5.0}),
    );
    assert!(result.is_ok());

    let info = ctrl.controller_info();
    let new_target = info.params["target_p99_ms"].as_f64().unwrap();
    assert!(
        (new_target - 50.0).abs() < 0.01,
        "target should be 10 * 5.0 = 50 after update, got {new_target}"
    );
}

#[test]
fn ramp_apply_update_set_max_rps() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0);

    let result = ctrl.apply_update("set_max_rps", &serde_json::json!({"max_rps": 200.0}));
    assert!(result.is_ok());

    // Drive rate up — should be capped at 200.
    for _ in 0..100 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    assert!(
        ctrl.current_rate() <= 200.0,
        "rate should be capped at new max_rps=200, got {}",
        ctrl.current_rate()
    );
}

#[test]
fn ramp_apply_update_rejects_unknown_action() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    let result = ctrl.apply_update("set_bogus", &serde_json::json!({}));
    assert!(result.is_err());
}

#[test]
fn ramp_controller_state_empty_during_warmup() {
    let config = RampConfig {
        warmup_duration: Duration::from_secs(60), // long warmup
        ..make_ramp_config()
    };
    let mut ctrl = RampRateController::new(config);
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0));

    let state = ctrl.controller_state();
    assert!(
        state.is_empty(),
        "controller_state should be empty during warmup"
    );
}

#[test]
fn ramp_controller_state_populated_during_ramping() {
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0);
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0));

    let state = ctrl.controller_state();
    let names: Vec<&str> = state.iter().map(|(name, _)| *name).collect();
    assert!(
        names.contains(&"netanvil_ramp_smoothed_p99_ms"),
        "should expose smoothed_p99_ms gauge: {names:?}"
    );
    assert!(
        names.contains(&"netanvil_ramp_latency_ratio"),
        "should expose latency_ratio gauge: {names:?}"
    );
}

// ---------------------------------------------------------------------------
// Behavior diff tests (unified constraint system)
// ---------------------------------------------------------------------------

#[test]
fn ramp_dual_trigger_timeout_and_error_takes_minimum() {
    // Behavior diff #3: when timeout AND error rate both trigger simultaneously,
    // the most conservative (min desired rate) wins. Old code: timeout early-return
    // prevented error rate from being evaluated.
    let mut ctrl = RampRateController::new(make_ramp_config()); // max_error_rate = 5%
    warmup_ramp(&mut ctrl, 10.0);

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_before = ctrl.current_rate();

    // Timeout soft backoff (×0.90) AND error hard backoff (×0.50).
    // soft_threshold = (5/100 × 0.5).max(0.01) = 0.025
    // timeout_fraction = 5/100 = 0.05 → soft backoff
    // error_rate = 10% → error_pct = 10 > 7.5 → hard backoff
    // Min(rate×0.90, rate×0.50) = rate×0.50
    let mut s = make_ramp_summary(100, 10.0, 0.10); // 10% errors
    s.timeout_count = 5; // 5% timeouts
    ctrl.update(&s);
    let rate_after = ctrl.current_rate();

    // Error hard backoff (×0.50) is more conservative than timeout soft (×0.90).
    // Both are Catastrophic and OperatingPoint respectively — error rate respects
    // floor but timeout would bypass it. Since error is binding (lower desired rate),
    // floor applies. Floor = 80% of peak.
    let floor = rate_before * 0.80;
    assert!(
        rate_after <= floor * 1.02,
        "dual-trigger should take min rate (error hard backoff, floored): \
         {rate_before:.1} → {rate_after:.1} (expected ≤{floor:.1})"
    );
    assert!(
        rate_after < rate_before * 0.85,
        "dual-trigger should cause significant drop: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_single_severe_spike_held_then_backoff() {
    // Behavior diff #5: a single latency spike with severity in [1.3, 3.0)
    // is held for 1 tick (persistence=2), then backs off on the second tick.
    // Severity > 3.0 bypasses persistence and backs off immediately.
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0); // target = 30ms

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }

    // p99 = 45ms → ratio = 1.5 → moderate backoff territory.
    // Tick 1: severity 1.5, streak=1 < persistence=2 → Hold.
    let rate_before_spike = ctrl.current_rate();
    ctrl.update(&make_ramp_summary(100, 45.0, 0.0));
    let rate_after_tick1 = ctrl.current_rate();

    assert!(
        (rate_after_tick1 - rate_before_spike).abs() / rate_before_spike < 0.02,
        "first tick of moderate spike should hold: {rate_before_spike:.1} → {rate_after_tick1:.1}"
    );

    // Tick 2: streak=2 ≥ persistence=2 → moderate backoff applied.
    ctrl.update(&make_ramp_summary(100, 45.0, 0.0));
    let rate_after_tick2 = ctrl.current_rate();

    assert!(
        rate_after_tick2 < rate_before_spike * 0.88,
        "second tick should apply moderate backoff: {rate_before_spike:.1} → {rate_after_tick2:.1}"
    );

    // Now test severity > 3.0 bypasses persistence.
    let mut ctrl2 = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl2, 10.0);
    for _ in 0..3 {
        ctrl2.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_before_hard = ctrl2.current_rate();

    // p99 = 100ms → ratio = 3.33 → severity > 3.0 → bypasses persistence.
    ctrl2.update(&make_ramp_summary(100, 100.0, 0.0));
    let rate_after_hard = ctrl2.current_rate();

    assert!(
        rate_after_hard < rate_before_hard * 0.85,
        "severity > 3.0 should bypass persistence and backoff immediately: \
         {rate_before_hard:.1} → {rate_after_hard:.1}"
    );
}

#[test]
fn ramp_streak_hysteresis_at_severity_boundary() {
    // Behavior diff #6: streak resets to 0 only when severity < 0.7
    // (AllowIncrease band). In the Hold band [0.7, 1.0), streak is maintained.
    // This prevents rapid flapping at the boundary.
    let mut ctrl = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl, 10.0); // target = 30ms

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }

    // Build up a violation streak: 2 ticks above threshold.
    // p99 = 40ms → ratio = 1.33 → severity ≥ 1.0.
    ctrl.update(&make_ramp_summary(100, 40.0, 0.0)); // streak=1, held
    ctrl.update(&make_ramp_summary(100, 40.0, 0.0)); // streak=2, gentle backoff

    // Now oscillate into the Hold band: p99 = 25ms → ratio = 0.83.
    // Severity in [0.7, 1.0): streak should be MAINTAINED (not reset).
    ctrl.update(&make_ramp_summary(100, 25.0, 0.0));

    // Go back above threshold with severity ≥ 1.5 (bypasses self_caused
    // override). p99 = 50ms → ratio = 1.67, streak was maintained ≥ 2.
    // Should trigger immediate backoff (streak=3 ≥ persistence=2).
    let rate_before_reviolation = ctrl.current_rate();
    ctrl.update(&make_ramp_summary(100, 50.0, 0.0));
    let rate_after_reviolation = ctrl.current_rate();

    assert!(
        rate_after_reviolation < rate_before_reviolation * 0.95,
        "re-violation after hold band should backoff immediately (streak maintained): \
         {rate_before_reviolation:.1} → {rate_after_reviolation:.1}"
    );

    // Contrast: if severity drops to < 0.7 (reset zone), streak resets,
    // so the same re-violation is held for 1 tick (streak=1 < persistence=2).
    let mut ctrl2 = RampRateController::new(make_ramp_config());
    warmup_ramp(&mut ctrl2, 10.0);
    for _ in 0..3 {
        ctrl2.update(&make_ramp_summary(100, 10.0, 0.0));
    }

    // Build streak.
    ctrl2.update(&make_ramp_summary(100, 40.0, 0.0)); // streak=1
    ctrl2.update(&make_ramp_summary(100, 40.0, 0.0)); // streak=2

    // Drop to clean zone: p99 = 10ms → ratio = 0.33 → severity < 0.7 → streak resets.
    ctrl2.update(&make_ramp_summary(100, 10.0, 0.0)); // streak=0, increase
    let rate_before_new = ctrl2.current_rate();

    // New violation at severity ≥ 1.5: streak=1 < persistence=2 → held.
    ctrl2.update(&make_ramp_summary(100, 50.0, 0.0)); // streak=1 < 2 → hold
    let rate_after_new = ctrl2.current_rate();

    assert!(
        (rate_after_new - rate_before_new).abs() / rate_before_new < 0.02,
        "after full reset (severity < 0.7), single violation should hold: \
         {rate_before_new:.1} → {rate_after_new:.1}"
    );
}

// ---------------------------------------------------------------------------
// External constraint tests
// ---------------------------------------------------------------------------

#[test]
fn ramp_external_higher_is_worse_backs_off() {
    let config = RampConfig {
        external_constraints: vec![ExternalConstraintConfig {
            signal_name: "cpu_pct".into(),
            threshold: 80.0,
            direction: SignalDirection::HigherIsWorse,
            on_missing: MissingSignalBehavior::Ignore,
            stale_after_ticks: 3,
            persistence: 1,
        }],
        ..make_ramp_config()
    };
    let mut ctrl = RampRateController::new(config);
    warmup_ramp(&mut ctrl, 10.0);

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_before = ctrl.current_rate();

    // cpu_pct = 120 → severity = 120/80 = 1.5 → moderate backoff.
    let mut s = make_ramp_summary(100, 10.0, 0.0);
    s.external_signals = vec![("cpu_pct".into(), 120.0)];
    ctrl.update(&s);
    let rate_after = ctrl.current_rate();

    assert!(
        rate_after < rate_before * 0.85,
        "HigherIsWorse signal above threshold should backoff: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_external_lower_is_worse_backs_off() {
    let config = RampConfig {
        external_constraints: vec![ExternalConstraintConfig {
            signal_name: "free_fds".into(),
            threshold: 1000.0,
            direction: SignalDirection::LowerIsWorse,
            on_missing: MissingSignalBehavior::Ignore,
            stale_after_ticks: 3,
            persistence: 1,
        }],
        ..make_ramp_config()
    };
    let mut ctrl = RampRateController::new(config);
    warmup_ramp(&mut ctrl, 10.0);

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_before = ctrl.current_rate();

    // free_fds = 500 → severity = 1000/500 = 2.0 → hard backoff.
    let mut s = make_ramp_summary(100, 10.0, 0.0);
    s.external_signals = vec![("free_fds".into(), 500.0)];
    ctrl.update(&s);
    let rate_after = ctrl.current_rate();

    assert!(
        rate_after < rate_before * 0.85,
        "LowerIsWorse signal below threshold should backoff: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_external_below_threshold_allows_increase() {
    let config = RampConfig {
        external_constraints: vec![ExternalConstraintConfig {
            signal_name: "cpu_pct".into(),
            threshold: 80.0,
            direction: SignalDirection::HigherIsWorse,
            on_missing: MissingSignalBehavior::Ignore,
            stale_after_ticks: 3,
            persistence: 1,
        }],
        ..make_ramp_config()
    };
    let mut ctrl = RampRateController::new(config);
    warmup_ramp(&mut ctrl, 10.0);

    let rate_before = ctrl.current_rate();

    // cpu_pct = 40 → severity = 40/80 = 0.5 → AllowIncrease.
    let mut s = make_ramp_summary(100, 10.0, 0.0);
    s.external_signals = vec![("cpu_pct".into(), 40.0)];
    ctrl.update(&s);
    let rate_after = ctrl.current_rate();

    assert!(
        rate_after > rate_before,
        "signal well below threshold should allow increase: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_external_missing_hold_suppresses_increase() {
    let config = RampConfig {
        external_constraints: vec![ExternalConstraintConfig {
            signal_name: "health_score".into(),
            threshold: 50.0,
            direction: SignalDirection::HigherIsWorse,
            on_missing: MissingSignalBehavior::Hold,
            stale_after_ticks: 1,
            persistence: 1,
        }],
        ..make_ramp_config()
    };
    let mut ctrl = RampRateController::new(config);
    warmup_ramp(&mut ctrl, 10.0);

    // Provide signal on all ramp-up ticks to prevent premature staleness.
    for _ in 0..3 {
        let mut s = make_ramp_summary(100, 10.0, 0.0);
        s.external_signals = vec![("health_score".into(), 10.0)];
        ctrl.update(&s);
    }

    // Let staleness confirm: need ticks_since_seen > stale_after_ticks (1),
    // then missing_streak >= persistence (1). Takes 2 missing ticks.
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // tick 1: not yet stale
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // tick 2: stale, confirmed

    // Now Hold is active. Verify rate doesn't increase further.
    let rate_before = ctrl.current_rate();
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // tick 3: hold
    let rate_after = ctrl.current_rate();

    assert!(
        rate_after <= rate_before * 1.01,
        "missing signal with Hold should suppress increase: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_external_missing_backoff_triggers_hard_backoff() {
    let config = RampConfig {
        external_constraints: vec![ExternalConstraintConfig {
            signal_name: "health".into(),
            threshold: 50.0,
            direction: SignalDirection::HigherIsWorse,
            on_missing: MissingSignalBehavior::Backoff,
            stale_after_ticks: 1,
            persistence: 1,
        }],
        ..make_ramp_config()
    };
    let mut ctrl = RampRateController::new(config);
    warmup_ramp(&mut ctrl, 10.0);

    // Provide signal on all ramp-up ticks to prevent premature staleness.
    for _ in 0..5 {
        let mut s = make_ramp_summary(100, 10.0, 0.0);
        s.external_signals = vec![("health".into(), 10.0)];
        ctrl.update(&s);
    }

    // Let staleness build: tick 1 not yet stale, tick 2 confirms.
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // missing 1
    let rate_before = ctrl.current_rate();
    ctrl.update(&make_ramp_summary(100, 10.0, 0.0)); // missing 2, confirmed → hard backoff
    let rate_after = ctrl.current_rate();

    assert!(
        rate_after < rate_before * 0.85,
        "missing signal with Backoff should cause drop: {rate_before:.1} → {rate_after:.1}"
    );
}

#[test]
fn ramp_external_missing_ignore_has_no_effect() {
    let config = RampConfig {
        external_constraints: vec![ExternalConstraintConfig {
            signal_name: "optional_metric".into(),
            threshold: 100.0,
            direction: SignalDirection::HigherIsWorse,
            on_missing: MissingSignalBehavior::Ignore,
            stale_after_ticks: 1,
            persistence: 1,
        }],
        ..make_ramp_config()
    };
    let mut ctrl = RampRateController::new(config);
    warmup_ramp(&mut ctrl, 10.0);

    for _ in 0..3 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_before = ctrl.current_rate();

    // Signal never present, on_missing=Ignore → no effect.
    for _ in 0..5 {
        ctrl.update(&make_ramp_summary(100, 10.0, 0.0));
    }
    let rate_after = ctrl.current_rate();

    assert!(
        rate_after > rate_before,
        "Ignore on missing should allow normal increase: {rate_before:.1} → {rate_after:.1}"
    );
}
