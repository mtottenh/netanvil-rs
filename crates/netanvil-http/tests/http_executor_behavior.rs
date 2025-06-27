use std::net::SocketAddr;
use std::time::{Duration, Instant};

use netanvil_http::HttpExecutor;
use netanvil_types::{RequestContext, RequestExecutor, RequestSpec};

// ---------------------------------------------------------------------------
// Test server: axum on a background tokio thread
// ---------------------------------------------------------------------------

fn start_test_server() -> (SocketAddr, std::thread::JoinHandle<()>) {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();

    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use axum::extract::Path;
            use axum::routing::{get, post};

            let app = axum::Router::new()
                .route("/", get(|| async { "OK" }))
                .route("/echo", post(|body: String| async move { body }))
                .route(
                    "/status/{code}",
                    get(|Path(code): Path<u16>| async move {
                        (
                            axum::http::StatusCode::from_u16(code)
                                .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
                            "error",
                        )
                    }),
                )
                .route(
                    "/slow",
                    get(|| async {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        "SLOW"
                    }),
                )
                .route(
                    "/delay/{ms}",
                    get(|Path(ms): Path<u64>| async move {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        format!("delayed {ms}ms")
                    }),
                );

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            addr_tx.send(addr).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // Give the server a moment to start accepting connections
    std::thread::sleep(Duration::from_millis(50));
    (addr, handle)
}

fn make_context() -> RequestContext {
    let now = Instant::now();
    RequestContext {
        request_id: 1,
        intended_time: now,
        actual_time: now,
        core_id: 0,
        is_sampled: false,
        session_id: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn get_request_returns_200_with_body() {
    let (addr, _server) = start_test_server();
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let executor = HttpExecutor::new();
        let spec = RequestSpec {
            method: http::Method::GET,
            url: format!("http://{}/", addr),
            headers: vec![],
            body: None,
        };

        let result = executor.execute(&spec, &make_context()).await;

        assert_eq!(result.status, Some(200));
        assert!(
            result.error.is_none(),
            "unexpected error: {:?}",
            result.error
        );
        assert!(result.response_size > 0, "body should not be empty");
        assert!(
            result.timing.total > Duration::ZERO,
            "total timing should be > 0"
        );
        assert!(
            result.timing.time_to_first_byte > Duration::ZERO,
            "TTFB should be > 0"
        );
    });
}

#[test]
fn post_request_sends_body() {
    let (addr, _server) = start_test_server();
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let executor = HttpExecutor::new();
        let payload = b"hello world".to_vec();
        let spec = RequestSpec {
            method: http::Method::POST,
            url: format!("http://{}/echo", addr),
            headers: vec![("Content-Type".into(), "text/plain".into())],
            body: Some(payload.clone()),
        };

        let result = executor.execute(&spec, &make_context()).await;

        assert_eq!(result.status, Some(200));
        assert!(result.error.is_none());
        // Echo endpoint returns the body — size should match
        assert_eq!(result.response_size, payload.len() as u64);
    });
}

#[test]
fn server_error_is_captured_in_status() {
    let (addr, _server) = start_test_server();
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let executor = HttpExecutor::new();
        let spec = RequestSpec {
            method: http::Method::GET,
            url: format!("http://{}/status/500", addr),
            headers: vec![],
            body: None,
        };

        let result = executor.execute(&spec, &make_context()).await;

        assert_eq!(result.status, Some(500));
        // HTTP 500 is not a transport error — it's a valid response
        assert!(result.error.is_none());
    });
}

#[test]
fn timeout_produces_timeout_error() {
    let (addr, _server) = start_test_server();
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        // 200ms timeout, server delays 5s
        let executor = HttpExecutor::with_timeout(Duration::from_millis(200));
        let spec = RequestSpec {
            method: http::Method::GET,
            url: format!("http://{}/slow", addr),
            headers: vec![],
            body: None,
        };

        let start = Instant::now();
        let result = executor.execute(&spec, &make_context()).await;
        let elapsed = start.elapsed();

        assert!(result.error.is_some(), "should have timed out");
        assert!(
            matches!(result.error, Some(netanvil_types::ExecutionError::Timeout)),
            "error should be Timeout, got {:?}",
            result.error
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "should not wait for the full 5s delay: {:?}",
            elapsed
        );
    });
}

#[test]
fn connection_refused_produces_connect_error() {
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let executor = HttpExecutor::with_timeout(Duration::from_secs(2));
        let spec = RequestSpec {
            method: http::Method::GET,
            // Port 1 is almost certainly not listening
            url: "http://127.0.0.1:1/".into(),
            headers: vec![],
            body: None,
        };

        let result = executor.execute(&spec, &make_context()).await;

        assert!(result.error.is_some(), "should have failed to connect");
        assert!(result.status.is_none());
    });
}

#[test]
fn custom_headers_are_sent() {
    let (addr, _server) = start_test_server();
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let executor = HttpExecutor::new();
        let spec = RequestSpec {
            method: http::Method::GET,
            url: format!("http://{}/", addr),
            headers: vec![
                ("X-Custom-Header".into(), "test-value".into()),
                ("Accept".into(), "application/json".into()),
            ],
            body: None,
        };

        let result = executor.execute(&spec, &make_context()).await;

        // We can't easily verify the server received the headers without
        // a more complex echo endpoint, but we verify the request succeeded
        assert_eq!(result.status, Some(200));
        assert!(result.error.is_none());
    });
}

#[test]
fn timing_breakdown_is_plausible_for_delayed_response() {
    let (addr, _server) = start_test_server();
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let executor = HttpExecutor::new();
        let spec = RequestSpec {
            method: http::Method::GET,
            url: format!("http://{}/delay/100", addr),
            headers: vec![],
            body: None,
        };

        let result = executor.execute(&spec, &make_context()).await;

        assert_eq!(result.status, Some(200));
        // Server delays 100ms, so TTFB should be >= 80ms (with tolerance)
        assert!(
            result.timing.time_to_first_byte >= Duration::from_millis(80),
            "TTFB should reflect server delay: {:?}",
            result.timing.time_to_first_byte
        );
        // Total should be >= TTFB
        assert!(result.timing.total >= result.timing.time_to_first_byte);
    });
}

#[test]
fn multiple_sequential_requests_reuse_connection() {
    let (addr, _server) = start_test_server();
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let executor = HttpExecutor::new();
        let spec = RequestSpec {
            method: http::Method::GET,
            url: format!("http://{}/", addr),
            headers: vec![],
            body: None,
        };

        // First request — includes connection setup
        let r1 = executor.execute(&spec, &make_context()).await;
        assert_eq!(r1.status, Some(200));

        // Subsequent requests should be faster (connection reuse)
        let mut ttfbs = Vec::new();
        for _ in 0..5 {
            let r = executor.execute(&spec, &make_context()).await;
            assert_eq!(r.status, Some(200));
            ttfbs.push(r.timing.time_to_first_byte);
        }

        // At least some of the subsequent TTFBs should be low (< 5ms for local)
        let fast_count = ttfbs
            .iter()
            .filter(|t| **t < Duration::from_millis(5))
            .count();
        assert!(
            fast_count >= 3,
            "expected most requests to be fast with connection reuse, TTFBs: {:?}",
            ttfbs
        );
    });
}
