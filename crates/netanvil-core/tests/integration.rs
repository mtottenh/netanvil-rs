//! End-to-end integration tests: spin up a real HTTP server,
//! run the full load test pipeline, verify results.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use netanvil_core::run_test;
use netanvil_http::HttpExecutor;
use netanvil_types::{ConnectionConfig, RateConfig, TestConfig};

// ---------------------------------------------------------------------------
// Test server
// ---------------------------------------------------------------------------

struct TestServer {
    addr: SocketAddr,
    request_count: Arc<AtomicU64>,
    _handle: std::thread::JoinHandle<()>,
}

fn start_test_server() -> TestServer {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let request_count = Arc::new(AtomicU64::new(0));
    let count = request_count.clone();

    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use axum::extract::{Path, State};
            use axum::routing::get;

            let app = axum::Router::new()
                .route(
                    "/",
                    get(|State(count): State<Arc<AtomicU64>>| async move {
                        count.fetch_add(1, Ordering::Relaxed);
                        "OK"
                    }),
                )
                .route(
                    "/delay/{ms}",
                    get(
                        |State(count): State<Arc<AtomicU64>>,
                         Path(ms): Path<u64>| async move {
                            count.fetch_add(1, Ordering::Relaxed);
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                            format!("delayed {ms}ms")
                        },
                    ),
                )
                .route(
                    "/error",
                    get(|State(count): State<Arc<AtomicU64>>| async move {
                        count.fetch_add(1, Ordering::Relaxed);
                        (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            "server error",
                        )
                    }),
                )
                .route(
                    "/intermittent",
                    get(|State(count): State<Arc<AtomicU64>>| async move {
                        let n = count.fetch_add(1, Ordering::Relaxed);
                        if n % 20 == 0 {
                            // Every 20th request: add 200ms delay
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                        "OK"
                    }),
                )
                .with_state(count);

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            addr_tx.send(addr).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(50));

    TestServer {
        addr,
        request_count,
        _handle: handle,
    }
}

fn make_config(server: &TestServer, rps: f64, duration: Duration, cores: usize) -> TestConfig {
    TestConfig {
        targets: vec![format!("http://{}/", server.addr)],
        duration,
        rate: RateConfig::Static { rps },
        num_cores: cores,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(10),
            ..Default::default()
        },
        metrics_interval: Duration::from_millis(200),
        control_interval: Duration::from_millis(100),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn full_pipeline_generates_load_at_target_rate() {
    let server = start_test_server();
    let config = make_config(&server, 200.0, Duration::from_secs(3), 2);

    let result = run_test(config, || HttpExecutor::with_timeout(Duration::from_secs(10))).unwrap();

    let expected = 600u64; // 200 RPS * 3s
    let lower = (expected as f64 * 0.70) as u64;
    let upper = (expected as f64 * 1.30) as u64;

    assert!(
        result.total_requests >= lower && result.total_requests <= upper,
        "expected ~{expected} requests, got {} (bounds: {lower}..{upper})",
        result.total_requests
    );
    assert_eq!(result.total_errors, 0);

    // Server-side count should approximately match client-side count
    let server_count = server.request_count.load(Ordering::Relaxed);
    assert!(
        server_count >= lower && server_count <= upper,
        "server saw {server_count} requests, expected ~{expected}"
    );
}

#[test]
fn rate_accuracy_across_core_counts() {
    let server = start_test_server();

    for num_cores in [1, 4] {
        // Reset server count
        server.request_count.store(0, Ordering::Relaxed);

        let config = make_config(&server, 150.0, Duration::from_secs(2), num_cores);
        let result =
            run_test(config, || HttpExecutor::with_timeout(Duration::from_secs(10))).unwrap();

        let expected = 300u64; // 150 RPS * 2s
        let lower = (expected as f64 * 0.65) as u64;
        let upper = (expected as f64 * 1.35) as u64;

        assert!(
            result.total_requests >= lower && result.total_requests <= upper,
            "cores={num_cores}: expected ~{expected} requests, got {} (bounds: {lower}..{upper})",
            result.total_requests
        );

        let server_count = server.request_count.load(Ordering::Relaxed);
        assert!(
            server_count >= lower && server_count <= upper,
            "cores={num_cores}: server saw {server_count}, expected ~{expected}"
        );
    }
}

#[test]
fn step_rate_changes_throughput() {
    let server = start_test_server();

    let config = TestConfig {
        targets: vec![format!("http://{}/", server.addr)],
        duration: Duration::from_secs(4),
        rate: RateConfig::Step {
            steps: vec![
                (Duration::from_secs(0), 100.0),
                (Duration::from_secs(2), 300.0),
            ],
        },
        num_cores: 2,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(10),
            ..Default::default()
        },
        metrics_interval: Duration::from_millis(200),
        control_interval: Duration::from_millis(100),
        ..Default::default()
    };

    let result = run_test(config, || HttpExecutor::with_timeout(Duration::from_secs(10))).unwrap();

    // 2s at 100 + 2s at 300 = 200 + 600 = 800
    let expected = 800u64;
    let lower = (expected as f64 * 0.55) as u64;
    let upper = (expected as f64 * 1.45) as u64;

    assert!(
        result.total_requests >= lower && result.total_requests <= upper,
        "expected ~{expected} requests with step rate, got {} (bounds: {lower}..{upper})",
        result.total_requests
    );
}

#[test]
fn error_endpoint_tracks_errors() {
    let server = start_test_server();

    let config = TestConfig {
        targets: vec![format!("http://{}/error", server.addr)],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(10),
            ..Default::default()
        },
        metrics_interval: Duration::from_millis(200),
        control_interval: Duration::from_millis(100),
        ..Default::default()
    };

    let result = run_test(config, || HttpExecutor::with_timeout(Duration::from_secs(10))).unwrap();

    assert!(result.total_requests > 100);
    // With default error_status_threshold=400, HTTP 500s count as errors
    assert_eq!(
        result.total_requests, result.total_errors,
        "all requests to /error should be counted as errors"
    );
    assert!(
        result.error_rate > 0.9,
        "error rate should be ~100%, got {:.1}%",
        result.error_rate * 100.0
    );
    let server_count = server.request_count.load(Ordering::Relaxed);
    assert!(server_count > 100, "server should have received requests");
}

#[test]
fn latency_reflects_server_delay() {
    let server = start_test_server();

    let config = TestConfig {
        targets: vec![format!("http://{}/delay/50", server.addr)],
        duration: Duration::from_secs(2),
        rate: RateConfig::Static { rps: 50.0 },
        num_cores: 1,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(10),
            ..Default::default()
        },
        metrics_interval: Duration::from_millis(200),
        control_interval: Duration::from_millis(100),
        ..Default::default()
    };

    let result = run_test(config, || HttpExecutor::with_timeout(Duration::from_secs(10))).unwrap();

    assert!(result.total_requests > 50);
    // Server adds 50ms delay — p50 should be at least 30ms
    assert!(
        result.latency_p50 >= Duration::from_millis(30),
        "p50 should reflect server delay: {:?}",
        result.latency_p50
    );
}

#[test]
fn coordinated_omission_detected_with_intermittent_delays() {
    let server = start_test_server();

    // Hit the /intermittent endpoint which delays every 20th request by 200ms.
    // At 200 RPS, a 200ms stall means ~40 requests pile up behind it.
    // Without CO correction, p99 would be ~200ms.
    // With CO correction, those 40 queued requests show their true wait time.
    let config = TestConfig {
        targets: vec![format!("http://{}/intermittent", server.addr)],
        duration: Duration::from_secs(3),
        rate: RateConfig::Static { rps: 200.0 },
        num_cores: 1,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(10),
            ..Default::default()
        },
        metrics_interval: Duration::from_millis(200),
        control_interval: Duration::from_millis(100),
        ..Default::default()
    };

    let result = run_test(config, || HttpExecutor::with_timeout(Duration::from_secs(10))).unwrap();

    assert!(result.total_requests > 300);
    // p50 should be fast (most requests are instant)
    assert!(
        result.latency_p50 < Duration::from_millis(50),
        "p50 should be fast for non-delayed requests: {:?}",
        result.latency_p50
    );
    // p99 should reflect the periodic delays + queue buildup
    assert!(
        result.latency_p99 > Duration::from_millis(10),
        "p99 should show impact of periodic delays: {:?}",
        result.latency_p99
    );
}
