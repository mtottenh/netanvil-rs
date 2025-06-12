//! End-to-end integration tests: spin up a real HTTP server,
//! run the full load test pipeline, verify results.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use netanvil_core::run_test;
use netanvil_http::HttpExecutor;
use netanvil_types::{ConnectionConfig, RateConfig, SchedulerConfig, TestConfig};

// ---------------------------------------------------------------------------
// Test server
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ServerState {
    request_count: Arc<AtomicU64>,
    /// Collects (method, path, selected_headers) from requests to /echo-meta
    captured_meta: Arc<Mutex<Vec<(String, String, Vec<(String, String)>)>>>,
}

struct TestServer {
    addr: SocketAddr,
    state: ServerState,
    _handle: std::thread::JoinHandle<()>,
}

impl TestServer {
    fn request_count(&self) -> u64 {
        self.state.request_count.load(Ordering::Relaxed)
    }

    fn reset_count(&self) {
        self.state.request_count.store(0, Ordering::Relaxed);
    }

    fn captured_meta(&self) -> Vec<(String, String, Vec<(String, String)>)> {
        self.state.captured_meta.lock().unwrap().clone()
    }
}

fn start_test_server() -> TestServer {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let state = ServerState {
        request_count: Arc::new(AtomicU64::new(0)),
        captured_meta: Arc::new(Mutex::new(Vec::new())),
    };
    let server_state = state.clone();

    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use axum::extract::{Path, State};
            use axum::http::Request;
            use axum::routing::{any, get};

            let app = axum::Router::new()
                .route(
                    "/",
                    get(|State(s): State<ServerState>| async move {
                        s.request_count.fetch_add(1, Ordering::Relaxed);
                        "OK"
                    }),
                )
                .route(
                    "/delay/{ms}",
                    get(
                        |State(s): State<ServerState>, Path(ms): Path<u64>| async move {
                            s.request_count.fetch_add(1, Ordering::Relaxed);
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                            format!("delayed {ms}ms")
                        },
                    ),
                )
                .route(
                    "/error",
                    get(|State(s): State<ServerState>| async move {
                        s.request_count.fetch_add(1, Ordering::Relaxed);
                        (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            "server error",
                        )
                    }),
                )
                .route(
                    "/not-found",
                    get(|State(s): State<ServerState>| async move {
                        s.request_count.fetch_add(1, Ordering::Relaxed);
                        (axum::http::StatusCode::NOT_FOUND, "not found")
                    }),
                )
                .route(
                    "/intermittent",
                    get(|State(s): State<ServerState>| async move {
                        let n = s.request_count.fetch_add(1, Ordering::Relaxed);
                        if n % 20 == 0 {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                        "OK"
                    }),
                )
                // Accepts any method, captures method + selected headers
                .route(
                    "/echo-meta",
                    any(
                        |State(s): State<ServerState>,
                         req: Request<axum::body::Body>| async move {
                            s.request_count.fetch_add(1, Ordering::Relaxed);
                            let method = req.method().to_string();
                            let path = req.uri().path().to_string();
                            let headers: Vec<(String, String)> = req
                                .headers()
                                .iter()
                                .filter(|(k, _)| {
                                    let k = k.as_str();
                                    k.starts_with("x-") || k == "authorization"
                                })
                                .map(|(k, v)| {
                                    (k.to_string(), v.to_str().unwrap_or("").to_string())
                                })
                                .collect();
                            s.captured_meta
                                .lock()
                                .unwrap()
                                .push((method, path, headers));
                            "OK"
                        },
                    ),
                )
                .with_state(server_state);

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
        state,
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
    let server_count = server.request_count();
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
        server.reset_count();

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

        let server_count = server.request_count();
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
    let server_count = server.request_count();
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

// ===========================================================================
// Config-driven extensibility tests
// ===========================================================================

#[test]
fn poisson_scheduler_generates_load_at_target_rate() {
    let server = start_test_server();

    let config = TestConfig {
        targets: vec![format!("http://{}/", server.addr)],
        duration: Duration::from_secs(3),
        rate: RateConfig::Static { rps: 200.0 },
        scheduler: SchedulerConfig::Poisson { seed: Some(42) },
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

    let expected = 600u64; // 200 RPS * 3s
    let lower = (expected as f64 * 0.65) as u64;
    let upper = (expected as f64 * 1.35) as u64;

    assert!(
        result.total_requests >= lower && result.total_requests <= upper,
        "poisson: expected ~{expected} requests, got {} (bounds: {lower}..{upper})",
        result.total_requests
    );
    assert_eq!(result.total_errors, 0);
}

#[test]
fn custom_headers_reach_the_server() {
    let server = start_test_server();

    let config = TestConfig {
        targets: vec![format!("http://{}/echo-meta", server.addr)],
        duration: Duration::from_secs(1),
        rate: RateConfig::Static { rps: 50.0 },
        headers: vec![
            ("X-Test-Id".into(), "netanvil-123".into()),
            ("Authorization".into(), "Bearer secret-token".into()),
        ],
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

    assert!(result.total_requests > 20);

    // Verify headers were received server-side
    let captured = server.captured_meta();
    assert!(!captured.is_empty(), "server should have captured requests");

    // Check first captured request has our custom headers
    let (_, _, headers) = &captured[0];
    let header_names: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    assert!(
        header_names.contains(&"x-test-id"),
        "server should see x-test-id header, got: {:?}",
        headers
    );
    assert!(
        header_names.contains(&"authorization"),
        "server should see authorization header, got: {:?}",
        headers
    );

    // Verify header values
    let test_id = headers.iter().find(|(k, _)| k == "x-test-id").unwrap();
    assert_eq!(test_id.1, "netanvil-123");
}

#[test]
fn http_method_is_configurable() {
    let server = start_test_server();

    let config = TestConfig {
        targets: vec![format!("http://{}/echo-meta", server.addr)],
        method: "POST".into(),
        duration: Duration::from_secs(1),
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

    assert!(result.total_requests > 20);

    let captured = server.captured_meta();
    assert!(!captured.is_empty());

    // All requests should be POST
    for (method, _, _) in &captured {
        assert_eq!(method, "POST", "expected POST, got {method}");
    }
}

#[test]
fn error_threshold_zero_ignores_http_errors() {
    let server = start_test_server();

    let config = TestConfig {
        targets: vec![format!("http://{}/error", server.addr)],
        duration: Duration::from_secs(1),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        error_status_threshold: 0, // only transport errors count
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
    // With threshold=0, HTTP 500 responses are NOT counted as errors
    assert_eq!(
        result.total_errors, 0,
        "with threshold=0, HTTP errors should not be counted"
    );
}

#[test]
fn error_threshold_500_only_counts_server_errors() {
    let server = start_test_server();

    // Hit the 404 endpoint — with threshold=500, this should NOT be an error
    let config = TestConfig {
        targets: vec![format!("http://{}/not-found", server.addr)],
        duration: Duration::from_secs(1),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        error_status_threshold: 500, // only 5xx count
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
    // 404 < 500, so no errors with this threshold
    assert_eq!(
        result.total_errors, 0,
        "404 should not be an error with threshold=500, but got {} errors",
        result.total_errors
    );
}

#[test]
fn error_threshold_default_counts_4xx_and_5xx() {
    let server = start_test_server();

    // Hit the 404 endpoint — with default threshold=400, this IS an error
    let config = TestConfig {
        targets: vec![format!("http://{}/not-found", server.addr)],
        duration: Duration::from_secs(1),
        rate: RateConfig::Static { rps: 100.0 },
        num_cores: 1,
        // error_status_threshold defaults to 400
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
    assert_eq!(
        result.total_requests, result.total_errors,
        "all 404 requests should be errors with default threshold=400"
    );
}
