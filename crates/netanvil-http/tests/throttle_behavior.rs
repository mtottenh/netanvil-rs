use std::io::Cursor;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use compio::io::AsyncRead;
use netanvil_http::throttle::{compute_socket_opts, MaybeThrottled, ThrottledStream};
use netanvil_http::HttpExecutor;
use netanvil_types::{HttpRequestSpec, RequestContext, RequestExecutor};

// ---------------------------------------------------------------------------
// Unit tests: ThrottledStream with in-memory Cursor
// ---------------------------------------------------------------------------

#[test]
fn throttled_read_takes_expected_time() {
    // 80_000 bps = 10_000 bytes/sec. Reading 5_000 bytes should take ~500ms.
    // Token bucket starts full (burst = max(10000*0.05, 1500) = 1500 bytes),
    // so the first 1500 bytes are "free" — the remaining 3500 bytes are
    // throttled at 10_000 B/s → ~350ms. Total ≈ 350ms + some overhead.
    //
    // We use generous bounds to avoid CI flakiness.
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(async {
        let data = vec![0xABu8; 5_000];
        let cursor = Cursor::new(data);
        let mut stream = ThrottledStream::new(cursor, 80_000);

        let start = Instant::now();

        // Read in chunks to exercise the token bucket across multiple reads
        let mut total_read = 0usize;
        while total_read < 5_000 {
            let buf = vec![0u8; 1024];
            let compio::BufResult(result, _buf) = stream.read(buf).await;
            let n = result.unwrap();
            if n == 0 {
                break;
            }
            total_read += n;
        }

        let elapsed = start.elapsed();
        assert_eq!(total_read, 5_000, "should read all 5000 bytes");

        // At 10,000 bytes/sec with 1500 byte burst: 3500 bytes of debt → ~350ms.
        // Allow generous range: 200ms..2000ms to handle slow CI.
        assert!(
            elapsed >= Duration::from_millis(200),
            "throttled read should take at least 200ms, took {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_millis(2000),
            "throttled read should not take more than 2s, took {:?}",
            elapsed
        );
    });
}

#[test]
fn unthrottled_read_is_fast() {
    // Same data but via MaybeThrottled::Direct — should be near-instant.
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(async {
        let data = vec![0xABu8; 10_000];
        let cursor = Cursor::new(data);
        let mut stream = MaybeThrottled::Direct(cursor);

        let start = Instant::now();
        let buf = vec![0u8; 10_000];
        let compio::BufResult(result, _) = stream.read(buf).await;
        let n = result.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(n, 10_000);
        assert!(
            elapsed < Duration::from_millis(50),
            "unthrottled read should be near-instant, took {:?}",
            elapsed
        );
    });
}

#[test]
fn token_bucket_burst_allows_initial_fast_read() {
    // At 80_000 bps (10_000 B/s), burst capacity is max(500, 1500) = 1500 bytes.
    // A read of 1000 bytes (< burst) should complete very quickly.
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(async {
        let data = vec![0xABu8; 1_000];
        let cursor = Cursor::new(data);
        let mut stream = ThrottledStream::new(cursor, 80_000);

        let start = Instant::now();
        let buf = vec![0u8; 1_000];
        let compio::BufResult(result, _) = stream.read(buf).await;
        let n = result.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(n, 1_000);
        // First read within burst should be fast — token debt is small.
        // The stream sleeps for debt: 1000 - 1500 tokens = -500 (no debt at all).
        // So this should be very fast.
        assert!(
            elapsed < Duration::from_millis(100),
            "burst read should be fast, took {:?}",
            elapsed
        );
    });
}

#[test]
fn multiple_reads_accumulate_throttle_delay() {
    // At 40_000 bps = 5_000 bytes/sec, burst = max(250, 1500) = 1500 bytes.
    // Reading 3x1000 = 3000 bytes total. First 1500 are burst, remaining
    // 1500 at 5_000 B/s → 300ms.
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(async {
        let data = vec![0xABu8; 3_000];
        let cursor = Cursor::new(data);
        let mut stream = ThrottledStream::new(cursor, 40_000);

        let start = Instant::now();
        for _ in 0..3 {
            let buf = vec![0u8; 1_000];
            let compio::BufResult(result, _) = stream.read(buf).await;
            assert_eq!(result.unwrap(), 1_000);
        }
        let elapsed = start.elapsed();

        // Expect at least ~200ms of throttle delay for the bytes beyond burst.
        assert!(
            elapsed >= Duration::from_millis(150),
            "multiple reads should accumulate delay, took {:?}",
            elapsed
        );
    });
}

// ---------------------------------------------------------------------------
// Socket option computation
// ---------------------------------------------------------------------------

#[test]
fn compute_socket_opts_56k_modem() {
    let (rcvbuf, clamp) = compute_socket_opts(56_000);
    // 56kbps = 7000 B/s → rcvbuf = max(700, 4096) = 4096
    assert_eq!(rcvbuf, 4096, "56k modem should use minimum rcvbuf");
    // clamp = max(350, 1024) = 1024
    assert_eq!(clamp, 1024, "56k modem should use minimum window clamp");
}

#[test]
fn compute_socket_opts_10mbps() {
    let (rcvbuf, clamp) = compute_socket_opts(10_000_000);
    // 10Mbps = 1,250,000 B/s → rcvbuf = max(125_000, 4096) = 65536 (capped)
    assert_eq!(rcvbuf, 65536, "10Mbps should hit rcvbuf cap");
    // clamp = max(62_500, 1024) = 62_500
    assert_eq!(clamp, 62_500);
}

#[test]
fn compute_socket_opts_1mbps() {
    let (rcvbuf, clamp) = compute_socket_opts(1_000_000);
    // 1Mbps = 125_000 B/s → rcvbuf = 12_500
    assert_eq!(rcvbuf, 12_500);
    // clamp = 6250
    assert_eq!(clamp, 6_250);
}

// ---------------------------------------------------------------------------
// End-to-end: HttpExecutor with bandwidth limiting
// ---------------------------------------------------------------------------

fn start_test_server() -> (SocketAddr, std::thread::JoinHandle<()>) {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();

    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use axum::routing::get;

            let app = axum::Router::new()
                .route("/", get(|| async { "OK" }))
                .route(
                    "/payload-small",
                    get(|| async {
                        // 8 KB — fits in SyncStream's internal buffer, so the
                        // throttle delay is in TTFB (during header parsing).
                        "X".repeat(8 * 1024)
                    }),
                )
                .route(
                    "/payload-large",
                    get(|| async {
                        // 64 KB — requires multiple SyncStream buffer fills,
                        // so throttle delay accumulates during body reading too.
                        "X".repeat(64 * 1024)
                    }),
                );

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            addr_tx.send(addr).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();
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

#[test]
fn bandwidth_limited_request_succeeds() {
    // Verify that a bandwidth-limited request completes successfully
    // and returns the correct response.
    let (addr, _server) = start_test_server();
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let executor = HttpExecutor::with_bandwidth_limit(1_000_000, Duration::from_secs(10));
        let spec = HttpRequestSpec {
            method: http::Method::GET,
            url: format!("http://{}/payload-small", addr),
            headers: vec![],
            body: None,
        };

        let result = executor.execute(&spec, &make_context()).await;

        assert_eq!(result.status, Some(200), "should get 200 OK");
        assert!(result.error.is_none(), "no error: {:?}", result.error);
        assert_eq!(result.response_size, 8 * 1024, "should get 8KB response");
    });
}

#[test]
fn bandwidth_limited_transfer_is_slower_than_unlimited() {
    // Fetch a 64KB response with and without bandwidth limiting.
    // At 80 kbps (10 KB/s), 64KB should take ~6.5s in total.
    // Without limiting, it should be <100ms on localhost.
    //
    // We measure total request time (not just content_transfer) because
    // small responses can be buffered by SyncStream's 8KB internal buffer
    // during header parsing, shifting throttle delay into TTFB. With 64KB,
    // multiple buffer fills are needed during body reading, ensuring the
    // throttle is exercised across the full transfer.
    let (addr, _server) = start_test_server();
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let spec = HttpRequestSpec {
            method: http::Method::GET,
            url: format!("http://{}/payload-large", addr),
            headers: vec![],
            body: None,
        };

        // Unlimited baseline
        let unlimited = HttpExecutor::with_timeout(Duration::from_secs(10));
        let r1 = unlimited.execute(&spec, &make_context()).await;
        assert_eq!(r1.status, Some(200));
        assert_eq!(r1.response_size, 64 * 1024);
        let unlimited_total = r1.timing.total;

        // Limited to 80 kbps (10 KB/s)
        let limited = HttpExecutor::with_bandwidth_limit(80_000, Duration::from_secs(30));
        let r2 = limited.execute(&spec, &make_context()).await;
        assert_eq!(r2.status, Some(200));
        assert_eq!(r2.response_size, 64 * 1024);
        let limited_total = r2.timing.total;

        // The throttled total should be significantly slower.
        // Unlimited on localhost: typically <50ms.
        // Throttled: 64KB at 10KB/s = ~6.5s (minus burst of ~1500 bytes).
        // Use 5x as a conservative threshold.
        assert!(
            limited_total > unlimited_total * 5,
            "throttled total ({:?}) should be much slower than unlimited ({:?})",
            limited_total,
            unlimited_total,
        );

        // The throttled total should take at least 2 seconds.
        // Math: (65536 - 1500 burst) / 10000 B/s ≈ 6.4s.
        // We use a conservative 2s floor to accommodate timing variance.
        assert!(
            limited_total >= Duration::from_secs(2),
            "throttled transfer of 64KB at 80kbps should take >= 2s, took {:?}",
            limited_total,
        );
    });
}
