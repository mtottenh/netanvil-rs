//! Integration tests for the HTTP control API.
//!
//! These tests start a real load test with the API server, then make
//! HTTP requests to verify control and metrics endpoints.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use netanvil_api::{ControlServer, SharedState};
use netanvil_core::run_test_with_api;
use netanvil_http::HttpExecutor;
use netanvil_types::{ConnectionConfig, RateConfig, TestConfig};

// ---------------------------------------------------------------------------
// Test HTTP server (target for load test)
// ---------------------------------------------------------------------------

fn start_target_server() -> SocketAddr {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use axum::routing::get;
            let app = axum::Router::new().route("/", get(|| async { "OK" }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    addr
}

// ---------------------------------------------------------------------------
// Simple HTTP client helpers (no extra deps)
// ---------------------------------------------------------------------------

fn http_get(addr: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    parse_http_response(&response)
}

fn http_put(addr: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "PUT {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    parse_http_response(&response)
}

fn http_post(addr: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    parse_http_response(&response)
}

fn parse_http_response(raw: &str) -> (u16, String) {
    let parts: Vec<&str> = raw.splitn(2, "\r\n\r\n").collect();
    let status_line = raw.lines().next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = parts.get(1).unwrap_or(&"").to_string();
    (status_code, body)
}

// ---------------------------------------------------------------------------
// Helper: run a test with API in the background, call a verification function
// ---------------------------------------------------------------------------

fn with_api_test(
    target_addr: SocketAddr,
    api_port: u16,
    duration: Duration,
    rps: f64,
    verify: impl FnOnce(&str) + Send + 'static,
) {
    let shared_state = SharedState::new();
    let (ext_cmd_tx, ext_cmd_rx) = flume::unbounded();

    let server = ControlServer::new(api_port, shared_state.clone(), ext_cmd_tx).unwrap();
    let _server_handle = server.spawn();

    // Give the server a moment to start
    std::thread::sleep(Duration::from_millis(100));

    let api_addr = format!("127.0.0.1:{api_port}");

    // Run verification in a background thread (the test body runs while the load test is active)
    let verify_handle = std::thread::spawn(move || {
        // Wait for the test to generate some data
        std::thread::sleep(Duration::from_millis(500));
        verify(&api_addr);
    });

    let config = TestConfig {
        targets: vec![format!("http://{}/", target_addr)],
        duration,
        rate: RateConfig::Static { rps },
        num_cores: 1,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        metrics_interval: Duration::from_millis(200),
        control_interval: Duration::from_millis(100),
        ..Default::default()
    };

    let progress_state = shared_state.clone();
    let _ = run_test_with_api(
        config,
        || HttpExecutor::with_timeout(Duration::from_secs(5)),
        move |update| {
            progress_state.update_from_progress(update);
        },
        ext_cmd_rx,
    );

    verify_handle.join().unwrap();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn get_status_returns_running() {
    let target = start_target_server();
    with_api_test(target, 19001, Duration::from_secs(3), 50.0, |api_addr| {
        let (status, body) = http_get(api_addr, "/status");
        assert_eq!(status, 200);

        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["state"], "running");
        assert!(json["elapsed_secs"].as_f64().unwrap() > 0.0);
        assert!(json["total_requests"].as_u64().unwrap() > 0);
    });
}

#[test]
fn get_metrics_returns_latency_data() {
    let target = start_target_server();
    with_api_test(target, 19002, Duration::from_secs(3), 50.0, |api_addr| {
        let (status, body) = http_get(api_addr, "/metrics");
        assert_eq!(status, 200);

        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(json["current_rps"].as_f64().is_some());
        assert!(json["total_requests"].as_u64().is_some());
        assert!(json["latency_p99_ms"].as_f64().is_some());
    });
}

#[test]
fn put_rate_changes_target_rps() {
    let target = start_target_server();
    with_api_test(target, 19003, Duration::from_secs(4), 50.0, |api_addr| {
        // Update rate
        let (status, body) = http_put(api_addr, "/rate", r#"{"rps": 200.0}"#);
        assert_eq!(status, 200);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["ok"], true);

        // Wait for the rate change to propagate through the coordinator
        std::thread::sleep(Duration::from_millis(500));

        // The rate controller's baseline should now be 200 (set_rate updates it)
        let (_, body) = http_get(api_addr, "/status");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let new_target = json["target_rps"].as_f64().unwrap();
        assert!(
            new_target > 100.0,
            "target RPS should reflect the API rate change, got {new_target}"
        );
    });
}

#[test]
fn post_stop_terminates_test_early() {
    let target = start_target_server();
    let shared_state = SharedState::new();
    let (ext_cmd_tx, ext_cmd_rx) = flume::unbounded();

    let api_port = 19004;
    let server = ControlServer::new(api_port, shared_state.clone(), ext_cmd_tx).unwrap();
    let _server_handle = server.spawn();
    std::thread::sleep(Duration::from_millis(100));

    let config = TestConfig {
        targets: vec![format!("http://{}/", target)],
        duration: Duration::from_secs(60), // long test
        rate: RateConfig::Static { rps: 50.0 },
        num_cores: 1,
        connections: ConnectionConfig {
            request_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        metrics_interval: Duration::from_millis(200),
        control_interval: Duration::from_millis(100),
        ..Default::default()
    };

    // Send stop after 1 second
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        let _ = http_post(&format!("127.0.0.1:{api_port}"), "/stop");
    });

    let progress_state = shared_state.clone();
    let start = std::time::Instant::now();
    let result = run_test_with_api(
        config,
        || HttpExecutor::with_timeout(Duration::from_secs(5)),
        move |update| {
            progress_state.update_from_progress(update);
        },
        ext_cmd_rx,
    )
    .unwrap();

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "test should stop early via API, took {:?}",
        elapsed
    );
    assert!(result.total_requests > 0);
}

#[test]
fn put_targets_accepted() {
    let target = start_target_server();
    with_api_test(target, 19005, Duration::from_secs(2), 50.0, |api_addr| {
        let (status, body) = http_put(
            api_addr,
            "/targets",
            r#"{"targets": ["http://new-target/"]}"#,
        );
        assert_eq!(status, 200);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["ok"], true);
    });
}

#[test]
fn put_headers_accepted() {
    let target = start_target_server();
    with_api_test(target, 19006, Duration::from_secs(2), 50.0, |api_addr| {
        let (status, body) = http_put(
            api_addr,
            "/headers",
            r#"{"headers": [["X-Custom", "value"]]}"#,
        );
        assert_eq!(status, 200);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["ok"], true);
    });
}

#[test]
fn not_found_for_unknown_path() {
    let target = start_target_server();
    with_api_test(target, 19007, Duration::from_secs(2), 50.0, |api_addr| {
        let (status, body) = http_get(api_addr, "/nonexistent");
        assert_eq!(status, 404);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["ok"], false);
    });
}

#[test]
fn put_rate_rejects_invalid_json() {
    let target = start_target_server();
    with_api_test(target, 19008, Duration::from_secs(2), 50.0, |api_addr| {
        let (status, body) = http_put(api_addr, "/rate", "not json");
        assert_eq!(status, 400);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["ok"], false);
        assert!(json["error"].as_str().unwrap().contains("invalid JSON"));
    });
}
