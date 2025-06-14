use std::time::Duration;

use netanvil_core::{PidRateController, StaticRateController, StepRateController};
use netanvil_types::{MetricsSummary, RateController, TargetMetric};

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
            error_rate: 0.9, // 90% errors
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
        external_signals: Vec::new(),
    }
}

#[test]
fn pid_reduces_rate_when_latency_exceeds_target() {
    let mut ctrl = PidRateController::new(
        TargetMetric::LatencyP99,
        100.0, // target 100ms p99
        500.0, // initial 500 RPS
        10.0,  // min
        10000.0, // max
        0.5, 0.01, 0.1,
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
        0.5, 0.01, 0.1,
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
        2.0, 0.1, 0.5, // aggressive gains
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
        2.0, 0.1, 0.5,
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
        0.5, 0.01, 0.1,
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
        0.1, 0.001, 0.05, // moderate gains
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
        5.0,   // target 5% error rate
        1000.0,
        10.0,
        10000.0,
        0.5, 0.01, 0.1,
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
            "http://a.com", "http://b.com", "http://c.com",
            "http://a.com", "http://b.com", "http://c.com",
            "http://a.com", "http://b.com", "http://c.com",
        ]
    );
}

#[test]
fn header_transformer_appends_headers() {
    use netanvil_core::{HeaderTransformer, NoopTransformer};
    use netanvil_types::{RequestContext, RequestSpec, RequestTransformer};
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

    let spec = RequestSpec {
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

fn make_summary_with_signal(
    requests: u64,
    signal_name: &str,
    signal_value: f64,
) -> MetricsSummary {
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
        0.5, 0.01, 0.1,
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
        0.5, 0.01, 0.1,
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
        TargetMetric::External {
            name: "cpu".into(),
        },
        70.0, // target 70% CPU
        500.0,
        10.0,
        10000.0,
        0.1, 0.001, 0.05,
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
        0.5, 0.01, 0.1,
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
