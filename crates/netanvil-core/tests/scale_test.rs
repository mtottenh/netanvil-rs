//! Scale test: characterizes maximum sustainable RPS per core.
//!
//! Runs progressively higher rates against a minimal-latency server and
//! measures: achieved rate, p99 latency, error rate, and the achieved/target
//! ratio. When the ratio drops below 0.8, we've hit the ceiling.
//!
//! With the N:M timer thread architecture, scheduling overhead is eliminated
//! from the I/O worker — the timer thread handles all timing via synchronous
//! precision sleep (~1μs). This should yield higher ceilings and better
//! rate accuracy than the old per-worker scheduling model.
//!
//! Run with: cargo test -p netanvil-core --test scale_test --release -- --nocapture
//! (release mode is important for realistic numbers)

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use netanvil_core::run_test;
use netanvil_http::HttpExecutor;
use netanvil_types::{ConnectionConfig, RateConfig, TestConfig};

fn start_fast_server() -> (SocketAddr, Arc<AtomicU64>) {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let count = Arc::new(AtomicU64::new(0));
    let server_count = count.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4) // multiple threads so server isn't the bottleneck
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            use axum::extract::State;
            use axum::routing::get;

            let app = axum::Router::new()
                .route(
                    "/",
                    get(|State(c): State<Arc<AtomicU64>>| async move {
                        c.fetch_add(1, Ordering::Relaxed);
                        "OK"
                    }),
                )
                .with_state(server_count);

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            addr_tx.send(addr).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    (addr, count)
}

struct ScaleResult {
    target_rps: f64,
    achieved_rps: f64,
    ratio: f64,
    total_requests: u64,
    total_errors: u64,
    p99_ms: f64,
}

fn run_at_rate(
    addr: SocketAddr,
    target_rps: f64,
    duration_secs: u64,
    num_cores: usize,
) -> ScaleResult {
    let config = TestConfig {
        targets: vec![format!("http://{}/", addr)],
        duration: Duration::from_secs(duration_secs),
        rate: RateConfig::Static { rps: target_rps },
        num_cores,
        connections: ConnectionConfig {
            max_connections_per_core: 500,
            request_timeout: Duration::from_secs(10),
            ..Default::default()
        },
        control_interval: Duration::from_secs(1),
        ..Default::default()
    };

    let result = run_test(config, || {
        HttpExecutor::with_timeout(Duration::from_secs(10))
    })
    .unwrap();

    let achieved_rps = result.total_requests as f64 / result.duration.as_secs_f64();
    ScaleResult {
        target_rps,
        achieved_rps,
        ratio: achieved_rps / target_rps,
        total_requests: result.total_requests,
        total_errors: result.total_errors,
        p99_ms: result.latency_p99.as_secs_f64() * 1000.0,
    }
}

#[test]
fn scale_characterization() {
    let (addr, _server_count) = start_fast_server();

    // Warm up connections
    let _ = run_at_rate(addr, 100.0, 1, 1);

    eprintln!();
    eprintln!("  === Single-core scale characterization (timer thread architecture) ===");
    eprintln!();

    let rates = [500.0, 1000.0, 2000.0, 5000.0, 10000.0];
    let mut results = Vec::new();

    eprintln!(
        "  {:<12} {:>12} {:>8} {:>10} {:>8} {:>8}",
        "Target RPS", "Achieved RPS", "Ratio", "Requests", "Errors", "p99 ms"
    );
    eprintln!("  {}", "-".repeat(70));

    for &target in &rates {
        let r = run_at_rate(addr, target, 5, 1);
        eprintln!(
            "  {:<12.0} {:>12.1} {:>8.2} {:>10} {:>8} {:>8.1}",
            r.target_rps, r.achieved_rps, r.ratio, r.total_requests, r.total_errors, r.p99_ms
        );
        results.push(r);
    }
    eprintln!();

    // Basic sanity: at 500 RPS (well within capability), we should hit >80%
    assert!(
        results[0].ratio > 0.80,
        "should achieve >80% of 500 RPS target, got ratio {:.2}",
        results[0].ratio
    );

    // At 1000 RPS, should still be viable
    assert!(
        results[1].ratio > 0.70,
        "should achieve >70% of 1000 RPS target, got ratio {:.2}",
        results[1].ratio
    );

    // Find the ceiling: highest rate where ratio > 0.7
    let ceiling = results
        .iter()
        .filter(|r| r.ratio > 0.7)
        .last()
        .map(|r| r.target_rps)
        .unwrap_or(0.0);
    eprintln!("  Estimated single-core ceiling: ~{ceiling:.0} RPS (ratio > 0.7)");
    eprintln!();
}

#[test]
fn scale_multi_core() {
    let (addr, _server_count) = start_fast_server();

    // Warm up
    let _ = run_at_rate(addr, 100.0, 1, 1);

    eprintln!();
    eprintln!("  === Multi-core scaling (timer thread architecture) ===");
    eprintln!();

    let target_rps = 2000.0;
    let core_counts = [1, 2, 4];

    eprintln!(
        "  {:<8} {:>12} {:>12} {:>8} {:>10} {:>8}",
        "Cores", "Target RPS", "Achieved RPS", "Ratio", "Requests", "p99 ms"
    );
    eprintln!("  {}", "-".repeat(68));

    let mut results = Vec::new();
    for &cores in &core_counts {
        let r = run_at_rate(addr, target_rps, 3, cores);
        eprintln!(
            "  {:<8} {:>12.0} {:>12.1} {:>8.2} {:>10} {:>8.1}",
            cores, r.target_rps, r.achieved_rps, r.ratio, r.total_requests, r.p99_ms
        );
        results.push((cores, r));
    }
    eprintln!();

    // With more cores, the achieved rate should be at least as good
    // (the timer thread handles scheduling, so I/O workers are uncontended)
    let single_ratio = results[0].1.ratio;
    for &(cores, ref r) in &results[1..] {
        assert!(
            r.ratio >= single_ratio * 0.80,
            "multi-core ({cores}) should not degrade significantly vs single-core: \
             single={single_ratio:.2}, multi={:.2}",
            r.ratio
        );
    }
}
