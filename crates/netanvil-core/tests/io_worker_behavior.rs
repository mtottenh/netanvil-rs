//! Behavioral tests for the I/O worker loop.
//!
//! These tests validate the I/O worker in isolation by manually driving
//! the fire channel (no timer thread). They are the timer-thread-architecture
//! equivalents of the old worker_behavior.rs tests.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use netanvil_core::io_worker::{io_worker_loop, IoWorkerConfig};
use netanvil_core::{HeaderTransformer, NoopTransformer, SimpleGenerator};
use netanvil_metrics::HdrMetricsCollector;
use netanvil_types::{
    EventRecorder, ExecutionResult, HttpRequestSpec, NoopEventRecorder, RequestContext,
    RequestExecutor, ScheduledRequest,
    TimingBreakdown,
};

// ---------------------------------------------------------------------------
// Mock executor: records call count, returns instant success
// ---------------------------------------------------------------------------

struct MockExecutor {
    call_count: Cell<u64>,
}

impl MockExecutor {
    fn new() -> Self {
        Self {
            call_count: Cell::new(0),
        }
    }
}

impl RequestExecutor for MockExecutor {
    type Spec = HttpRequestSpec;

    async fn execute(&self, _spec: &HttpRequestSpec, context: &RequestContext) -> ExecutionResult {
        self.call_count.set(self.call_count.get() + 1);
        ExecutionResult {
            request_id: context.request_id,
            intended_time: context.intended_time,
            actual_time: context.actual_time,
            timing: TimingBreakdown {
                total: Duration::from_micros(100),
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
// Capturing executor: records the HttpRequestSpec of each call
// ---------------------------------------------------------------------------

struct CapturingExecutor {
    captured: Rc<RefCell<Vec<HttpRequestSpec>>>,
}

impl CapturingExecutor {
    fn new() -> (Self, Rc<RefCell<Vec<HttpRequestSpec>>>) {
        let captured = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                captured: captured.clone(),
            },
            captured,
        )
    }
}

impl RequestExecutor for CapturingExecutor {
    type Spec = HttpRequestSpec;

    async fn execute(&self, spec: &HttpRequestSpec, context: &RequestContext) -> ExecutionResult {
        self.captured.borrow_mut().push(spec.clone());
        ExecutionResult {
            request_id: context.request_id,
            intended_time: context.intended_time,
            actual_time: context.actual_time,
            timing: TimingBreakdown {
                total: Duration::from_micros(50),
                ..Default::default()
            },
            status: Some(200),
            bytes_sent: 0,
            response_size: 64,
            error: None,
            response_headers: None,
            response_body: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn io_worker_fires_requests_from_channel() {
    // Send N Fire messages from another thread, verify N requests are executed.
    let (fire_tx, fire_rx) = flume::bounded::<ScheduledRequest>(128);
    let (metrics_tx, metrics_rx) = flume::unbounded();

    let n = 50u64;

    // Send fire events from another thread (like the real timer thread does),
    // giving the I/O worker time to process them.
    std::thread::spawn(move || {
        let now = Instant::now();
        for i in 0..n {
            fire_tx
                .send(ScheduledRequest::Fire(now + Duration::from_millis(i)))
                .unwrap();
            // Small sleep to let the worker process between messages
            if i % 10 == 9 {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // Give the worker time to process the last batch before stopping
        std::thread::sleep(Duration::from_millis(100));
        fire_tx.send(ScheduledRequest::Stop).unwrap();
    });

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(async {
        let generator = SimpleGenerator::get(vec!["http://mock.test/".into()]);
        let executor = MockExecutor::new();
        let collector = HdrMetricsCollector::new(0, vec![], false);

        io_worker_loop(
            IoWorkerConfig {
                fire_rx,
                metrics_tx,
                core_id: 0,
                metrics_interval: Duration::from_millis(50),
                graceful_shutdown: true,
            },
            generator,
            Rc::new(NoopTransformer),
            Rc::new(executor),
            Rc::new(collector),
            Rc::new(NoopEventRecorder) as Rc<dyn EventRecorder>,
        )
        .await;
    });

    // Collect all metrics
    let mut total = 0u64;
    while let Ok(snap) = metrics_rx.try_recv() {
        total += snap.total_requests;
    }

    let lower = (n as f64 * 0.90) as u64;
    assert!(
        total >= lower && total <= n,
        "expected ~{n} requests fired, got {total}"
    );
}

#[test]
fn io_worker_stops_on_stop_message() {
    // Verify Stop message causes clean, prompt shutdown.
    let (fire_tx, fire_rx) = flume::bounded::<ScheduledRequest>(128);
    let (metrics_tx, _metrics_rx) = flume::unbounded();

    // Send a few fires then stop
    let now = Instant::now();
    for _ in 0..5 {
        fire_tx.send(ScheduledRequest::Fire(now)).unwrap();
    }
    fire_tx.send(ScheduledRequest::Stop).unwrap();

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    let before = Instant::now();
    rt.block_on(async {
        let generator = SimpleGenerator::get(vec!["http://mock.test/".into()]);
        let executor = MockExecutor::new();
        let collector = HdrMetricsCollector::new(0, vec![], false);

        io_worker_loop(
            IoWorkerConfig {
                fire_rx,
                metrics_tx,
                core_id: 0,
                metrics_interval: Duration::from_millis(100),
                graceful_shutdown: true,
            },
            generator,
            Rc::new(NoopTransformer),
            Rc::new(executor),
            Rc::new(collector),
            Rc::new(NoopEventRecorder) as Rc<dyn EventRecorder>,
        )
        .await;
    });
    let elapsed = before.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "worker should stop promptly on Stop message, took {:?}",
        elapsed
    );
}

#[test]
fn io_worker_exits_on_channel_disconnect() {
    // Dropping the sender should cause the worker to exit cleanly.
    let (fire_tx, fire_rx) = flume::bounded::<ScheduledRequest>(128);
    let (metrics_tx, _metrics_rx) = flume::unbounded();

    // Send a few fires then drop the sender
    let now = Instant::now();
    for _ in 0..3 {
        fire_tx.send(ScheduledRequest::Fire(now)).unwrap();
    }
    drop(fire_tx);

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    let before = Instant::now();
    rt.block_on(async {
        let generator = SimpleGenerator::get(vec!["http://mock.test/".into()]);
        let executor = MockExecutor::new();
        let collector = HdrMetricsCollector::new(0, vec![], false);

        io_worker_loop(
            IoWorkerConfig {
                fire_rx,
                metrics_tx,
                core_id: 0,
                metrics_interval: Duration::from_millis(100),
                graceful_shutdown: true,
            },
            generator,
            Rc::new(NoopTransformer),
            Rc::new(executor),
            Rc::new(collector),
            Rc::new(NoopEventRecorder) as Rc<dyn EventRecorder>,
        )
        .await;
    });
    let elapsed = before.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "worker should exit on channel disconnect, took {:?}",
        elapsed
    );
}

#[test]
fn io_worker_handles_target_update() {
    // Verify UpdateTargets propagates to the generator, changing which URLs are hit.
    let (fire_tx, fire_rx) = flume::bounded::<ScheduledRequest>(256);
    let (metrics_tx, _metrics_rx) = flume::unbounded();

    let now = Instant::now();

    // Fire some requests to the original target
    for _ in 0..20 {
        fire_tx.send(ScheduledRequest::Fire(now)).unwrap();
    }

    // Update targets
    fire_tx
        .send(ScheduledRequest::UpdateTargets(vec![
            "http://new-target.test/a".into(),
            "http://new-target.test/b".into(),
        ]))
        .unwrap();

    // Fire more requests to the new targets
    for _ in 0..20 {
        fire_tx.send(ScheduledRequest::Fire(now)).unwrap();
    }

    fire_tx.send(ScheduledRequest::Stop).unwrap();

    let (executor, captured) = CapturingExecutor::new();

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(async {
        let generator = SimpleGenerator::get(vec!["http://original.test/".into()]);
        let collector = HdrMetricsCollector::new(0, vec![], false);

        io_worker_loop(
            IoWorkerConfig {
                fire_rx,
                metrics_tx,
                core_id: 0,
                metrics_interval: Duration::from_secs(10),
                graceful_shutdown: true,
            },
            generator,
            Rc::new(NoopTransformer),
            Rc::new(executor),
            Rc::new(collector),
            Rc::new(NoopEventRecorder) as Rc<dyn EventRecorder>,
        )
        .await;
    });

    let specs = captured.borrow();
    assert!(
        specs.len() >= 40,
        "should have generated ~40 requests, got {}",
        specs.len()
    );

    // Early requests should go to original target
    assert!(
        specs[0].url.contains("original.test"),
        "first request should hit original target, got {}",
        specs[0].url
    );

    // Later requests should go to new targets
    let last_url = &specs[specs.len() - 1].url;
    assert!(
        last_url.contains("new-target.test"),
        "last request should hit new target, got {last_url}"
    );

    // Verify new targets round-robin
    let new_specs: Vec<&HttpRequestSpec> = specs
        .iter()
        .filter(|s| s.url.contains("new-target.test"))
        .collect();
    assert!(
        new_specs.len() >= 15,
        "should have many requests to new targets, got {}",
        new_specs.len()
    );
    let has_a = new_specs.iter().any(|s| s.url.contains("/a"));
    let has_b = new_specs.iter().any(|s| s.url.contains("/b"));
    assert!(has_a && has_b, "should round-robin across both new targets");
}

#[test]
fn io_worker_handles_header_update() {
    // Verify UpdateMetadata propagates to the transformer.
    let (fire_tx, fire_rx) = flume::bounded::<ScheduledRequest>(256);
    let (metrics_tx, _metrics_rx) = flume::unbounded();

    let now = Instant::now();

    // Fire some requests with original headers
    for _ in 0..20 {
        fire_tx.send(ScheduledRequest::Fire(now)).unwrap();
    }

    // Update headers
    fire_tx
        .send(ScheduledRequest::UpdateMetadata(vec![
            ("X-New-Header".into(), "new-value".into()),
            ("Authorization".into(), "Bearer token123".into()),
        ]))
        .unwrap();

    // Fire more requests with new headers
    for _ in 0..20 {
        fire_tx.send(ScheduledRequest::Fire(now)).unwrap();
    }

    fire_tx.send(ScheduledRequest::Stop).unwrap();

    let (executor, captured) = CapturingExecutor::new();

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(async {
        let generator = SimpleGenerator::get(vec!["http://test.local/".into()]);
        let transformer = HeaderTransformer::new(vec![("X-Original".into(), "yes".into())]);
        let collector = HdrMetricsCollector::new(0, vec![], false);

        io_worker_loop(
            IoWorkerConfig {
                fire_rx,
                metrics_tx,
                core_id: 0,
                metrics_interval: Duration::from_secs(10),
                graceful_shutdown: true,
            },
            generator,
            Rc::new(transformer),
            Rc::new(executor),
            Rc::new(collector),
            Rc::new(NoopEventRecorder) as Rc<dyn EventRecorder>,
        )
        .await;
    });

    let specs = captured.borrow();
    assert!(specs.len() >= 40, "should have generated ~40 requests");

    // Early requests should have original header
    let first_headers: Vec<&str> = specs[0].headers.iter().map(|(k, _)| k.as_str()).collect();
    assert!(
        first_headers.contains(&"X-Original"),
        "first request should have original header, got {:?}",
        specs[0].headers
    );

    // Later requests should have new headers (not the original)
    let last = &specs[specs.len() - 1];
    let last_header_names: Vec<&str> = last.headers.iter().map(|(k, _)| k.as_str()).collect();
    assert!(
        last_header_names.contains(&"X-New-Header"),
        "last request should have new header, got {:?}",
        last.headers
    );
    assert!(
        last_header_names.contains(&"Authorization"),
        "last request should have Authorization header, got {:?}",
        last.headers
    );
    // Original header should be gone (replaced, not appended)
    assert!(
        !last_header_names.contains(&"X-Original"),
        "original header should be replaced, got {:?}",
        last.headers
    );
}

#[test]
fn io_worker_sends_periodic_metrics_snapshots() {
    // Drive the worker for a while and verify metrics snapshots arrive periodically.
    let (fire_tx, fire_rx) = flume::bounded::<ScheduledRequest>(1024);
    let (metrics_tx, metrics_rx) = flume::unbounded();

    // We'll send fire events from another thread at a steady pace
    std::thread::spawn(move || {
        let start = Instant::now();
        let duration = Duration::from_secs(2);
        let interval = Duration::from_millis(5); // 200 RPS

        let mut next = start;
        while start.elapsed() < duration {
            if Instant::now() >= next {
                let _ = fire_tx.send(ScheduledRequest::Fire(next));
                next += interval;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let _ = fire_tx.send(ScheduledRequest::Stop);
    });

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(async {
        let generator = SimpleGenerator::get(vec!["http://mock.test/".into()]);
        let executor = MockExecutor::new();
        let collector = HdrMetricsCollector::new(0, vec![], false);

        io_worker_loop(
            IoWorkerConfig {
                fire_rx,
                metrics_tx,
                core_id: 0,
                metrics_interval: Duration::from_millis(200), // report every 200ms
                graceful_shutdown: true,
            },
            generator,
            Rc::new(NoopTransformer),
            Rc::new(executor),
            Rc::new(collector),
            Rc::new(NoopEventRecorder) as Rc<dyn EventRecorder>,
        )
        .await;
    });

    // 2 seconds / 200ms interval ≈ 10 snapshots, plus the final one
    let mut snapshot_count = 0;
    let mut total_requests = 0u64;
    while let Ok(snap) = metrics_rx.try_recv() {
        snapshot_count += 1;
        total_requests += snap.total_requests;
    }

    assert!(
        snapshot_count >= 5,
        "expected at least 5 snapshots over 2 seconds with 200ms interval, got {snapshot_count}"
    );
    assert!(
        total_requests > 200,
        "total requests across snapshots should be ~400, got {total_requests}"
    );
}
