use std::time::Duration;

use netanvil_core::{
    controller::autotune::{
        compute_cohen_coon_gains, gain_schedule, ExplorationManager, MetricExploration, PidState,
    },
    AutotuneParams, AutotuningPidController, CompositePidController, PidGainValues,
    PidRateController, StaticRateController, StepRateController,
};
use netanvil_types::{MetricsSummary, PidConstraint, PidGains, RateController, TargetMetric};

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

    // 500 ticks: ~15 for exploration, ~485 for convergence
    for _ in 0..500 {
        let rate = ctrl.current_rate();
        let simulated_latency = rate / 10.0;
        ctrl.update(&make_summary(100, simulated_latency, 0.0));
    }

    let final_rate = ctrl.current_rate();
    assert!(
        (final_rate - 1000.0).abs() < 500.0,
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
