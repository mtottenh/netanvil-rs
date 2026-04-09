use std::time::Duration;

use netanvil_core::run_test;
use netanvil_types::{
    ConnectionConfig, ExecutionResult, HttpRequestSpec, RateConfig, RequestContext,
    RequestExecutor, TestConfig, TimingBreakdown,
};

// ---------------------------------------------------------------------------
// Mock executor: instant success with configurable latency
// ---------------------------------------------------------------------------

struct MockExecutor {
    latency: Duration,
}

impl MockExecutor {
    fn instant() -> Self {
        Self {
            latency: Duration::from_micros(50),
        }
    }

    fn with_latency(latency: Duration) -> Self {
        Self { latency }
    }
}

impl RequestExecutor for MockExecutor {
    type Spec = HttpRequestSpec;
    type PacketSource = netanvil_types::NoopPacketSource;

    async fn execute(&self, _spec: &HttpRequestSpec, context: &RequestContext) -> ExecutionResult {
        if self.latency > Duration::from_millis(1) {
            compio::time::sleep(self.latency).await;
        }
        ExecutionResult {
            request_id: context.request_id,
            intended_time: context.intended_time,
            sent_time: context.sent_time,
            actual_time: context.actual_time,
            dispatch_time: context.dispatch_time,
            timing: TimingBreakdown {
                total: self.latency,
                ..Default::default()
            },
            status: Some(200),
            bytes_sent: 0,
            response_size: 256,
            error: None,
            response_headers: None,
            response_body: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Error executor: always fails
// ---------------------------------------------------------------------------

struct ErrorExecutor;

impl RequestExecutor for ErrorExecutor {
    type Spec = HttpRequestSpec;
    type PacketSource = netanvil_types::NoopPacketSource;

    async fn execute(&self, _spec: &HttpRequestSpec, context: &RequestContext) -> ExecutionResult {
        ExecutionResult {
            request_id: context.request_id,
            intended_time: context.intended_time,
            sent_time: context.sent_time,
            actual_time: context.actual_time,
            dispatch_time: context.dispatch_time,
            timing: TimingBreakdown {
                total: Duration::from_micros(50),
                ..Default::default()
            },
            status: Some(500),
            bytes_sent: 0,
            response_size: 0,
            error: Some(netanvil_types::ExecutionError::Http(
                "500 Internal Server Error".into(),
            )),
            response_headers: None,
            response_body: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Engine integration tests
// ---------------------------------------------------------------------------

#[test]
fn engine_runs_constant_rate_test_with_correct_request_count() {
    let config = TestConfig {
        targets: vec!["http://mock.test/".into()],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 200.0 },
        num_cores: 2,
        control_interval: Duration::from_secs(1),
        connections: ConnectionConfig::default(),
        ..Default::default()
    };

    let result = run_test(config, |_| MockExecutor::instant()).unwrap();

    let expected = 400u64; // 200 RPS * 2s
    let lower = (expected as f64 * 0.70) as u64;
    let upper = (expected as f64 * 1.30) as u64;

    assert!(
        result.total_requests >= lower && result.total_requests <= upper,
        "expected ~{expected} requests, got {} (bounds: {lower}..{upper})",
        result.total_requests
    );
    assert_eq!(result.total_errors, 0);
    assert!(
        result.request_rate > 100.0,
        "rate too low: {}",
        result.request_rate
    );
}

#[test]
fn engine_records_latency_percentiles() {
    let config = TestConfig {
        targets: vec!["http://mock.test/".into()],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        control_interval: Duration::from_secs(1),
        connections: ConnectionConfig::default(),
        ..Default::default()
    };

    let result = run_test(config, |_| {
        MockExecutor::with_latency(Duration::from_millis(5))
    })
    .unwrap();

    assert!(result.total_requests > 100);
    // Mock latency is 5ms — p50 should be in that ballpark
    // (HDR histogram has some precision loss, and compio sleep adds jitter)
    assert!(
        result.latency_p50 < Duration::from_millis(50),
        "p50 too high: {:?}",
        result.latency_p50
    );
}

#[test]
fn engine_tracks_errors() {
    let config = TestConfig {
        targets: vec!["http://mock.test/".into()],
        duration: Duration::from_secs(3),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        control_interval: Duration::from_secs(1),
        connections: ConnectionConfig::default(),
        ..Default::default()
    };

    let result = run_test(config, |_| ErrorExecutor).unwrap();

    assert!(result.total_requests > 50);
    assert_eq!(result.total_requests, result.total_errors);
    assert!(
        (result.error_rate - 1.0).abs() < 0.01,
        "error rate should be ~1.0, got {}",
        result.error_rate
    );
}

#[test]
fn engine_scales_across_cores() {
    // Run at 200 RPS with 1 core, then 4 cores. Both should hit the target.
    for num_cores in [1, 4] {
        let config = TestConfig {
            targets: vec!["http://mock.test/".into()],
            duration: Duration::from_secs(2),
            rate: RateConfig::Static { rps: 200.0 },
            num_cores,
            control_interval: Duration::from_secs(1),
            connections: ConnectionConfig::default(),
            ..Default::default()
        };

        let result = run_test(config, |_| MockExecutor::instant()).unwrap();

        let expected = 400u64;
        let lower = (expected as f64 * 0.65) as u64;
        let upper = (expected as f64 * 1.35) as u64;

        assert!(
            result.total_requests >= lower && result.total_requests <= upper,
            "cores={num_cores}: expected ~{expected} requests, got {} (bounds: {lower}..{upper})",
            result.total_requests
        );
    }
}

#[test]
fn engine_step_rate_changes_throughput() {
    let config = TestConfig {
        targets: vec!["http://mock.test/".into()],
        duration: Duration::from_secs(4),
        rate: RateConfig::Step {
            steps: vec![
                (Duration::from_secs(0), 100.0),
                (Duration::from_secs(2), 400.0),
            ],
        },
        num_cores: 2,
        control_interval: Duration::from_secs(1),
        connections: ConnectionConfig::default(),
        ..Default::default()
    };

    let result = run_test(config, |_| MockExecutor::instant()).unwrap();

    // 2s at 100 RPS + 2s at 400 RPS = 200 + 800 = 1000
    let expected = 1000u64;
    let lower = (expected as f64 * 0.60) as u64;
    let upper = (expected as f64 * 1.40) as u64;

    assert!(
        result.total_requests >= lower && result.total_requests <= upper,
        "expected ~{expected} requests with step rate, got {} (bounds: {lower}..{upper})",
        result.total_requests
    );
}
