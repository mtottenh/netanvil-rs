use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use netanvil_core::{
    ConstantRateScheduler, HeaderTransformer, NoopTransformer, SimpleGenerator, Worker,
};
use netanvil_metrics::HdrMetricsCollector;
use netanvil_types::{
    ExecutionResult, RequestContext, RequestExecutor, RequestSpec,
    TimingBreakdown, WorkerCommand,
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
    async fn execute(&self, _spec: &RequestSpec, context: &RequestContext) -> ExecutionResult {
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
            response_size: 256,
            error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: build and run a worker for a given config, return metrics
// ---------------------------------------------------------------------------

fn run_worker_test(
    rps: f64,
    duration: Duration,
    run_for: Duration,
) -> (u64, u64) {
    let (cmd_tx, cmd_rx) = flume::unbounded();
    let (metrics_tx, metrics_rx) = flume::unbounded();

    let start = Instant::now();
    let scheduler = ConstantRateScheduler::new(rps, start, Some(duration));
    let generator = SimpleGenerator::get(vec!["http://mock.test/".into()]);
    let transformer = NoopTransformer;
    let executor = MockExecutor::new();
    let collector = HdrMetricsCollector::new(0);

    let worker = Worker::new(
        scheduler,
        generator,
        transformer,
        executor,
        collector,
        cmd_rx,
        metrics_tx,
        0, // core_id
        Duration::from_millis(100), // metrics interval
    );

    // Spawn a thread to stop the worker after run_for if it hasn't finished
    let cmd_tx_clone = cmd_tx.clone();
    std::thread::spawn(move || {
        std::thread::sleep(run_for + Duration::from_millis(500));
        let _ = cmd_tx_clone.send(WorkerCommand::Stop);
    });

    // Run in a compio runtime
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(worker.run());

    // Collect all metrics snapshots
    let mut total_requests = 0u64;
    let mut total_errors = 0u64;
    while let Ok(snapshot) = metrics_rx.try_recv() {
        total_requests += snapshot.total_requests;
        total_errors += snapshot.total_errors;
    }

    (total_requests, total_errors)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn worker_generates_approximately_correct_request_count() {
    let rps = 200.0;
    let duration = Duration::from_secs(2);
    let (total, errors) = run_worker_test(rps, duration, duration);

    let expected = (rps * duration.as_secs_f64()) as u64;

    // Allow 20% tolerance — timer precision, async scheduling overhead
    let lower = (expected as f64 * 0.80) as u64;
    let upper = (expected as f64 * 1.20) as u64;
    assert!(
        total >= lower && total <= upper,
        "expected ~{expected} requests, got {total} (bounds: {lower}..{upper})"
    );
    assert_eq!(errors, 0);
}

#[test]
fn worker_stops_on_command() {
    let (cmd_tx, cmd_rx) = flume::unbounded();
    let (metrics_tx, metrics_rx) = flume::unbounded();

    let start = Instant::now();
    // Schedule for 60 seconds — but we'll stop it after 500ms
    let scheduler = ConstantRateScheduler::new(100.0, start, Some(Duration::from_secs(60)));
    let generator = SimpleGenerator::get(vec!["http://mock.test/".into()]);
    let executor = MockExecutor::new();
    let collector = HdrMetricsCollector::new(0);

    let worker = Worker::new(
        scheduler,
        generator,
        NoopTransformer,
        executor,
        collector,
        cmd_rx,
        metrics_tx,
        0,
        Duration::from_millis(100),
    );

    // Send stop after 500ms
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        let _ = cmd_tx.send(WorkerCommand::Stop);
    });

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    let before = Instant::now();
    rt.block_on(worker.run());
    let elapsed = before.elapsed();

    // Should have stopped in roughly 500ms, not 60 seconds
    assert!(
        elapsed < Duration::from_secs(2),
        "worker should stop promptly on command, took {:?}",
        elapsed
    );

    // Should have generated some requests (500ms at 100 RPS ≈ 50)
    let mut total = 0u64;
    while let Ok(snap) = metrics_rx.try_recv() {
        total += snap.total_requests;
    }
    assert!(total > 10, "expected some requests before stop, got {total}");
    assert!(total < 200, "too many requests for 500ms at 100 RPS: {total}");
}

#[test]
fn worker_responds_to_rate_update() {
    let (cmd_tx, cmd_rx) = flume::unbounded();
    let (metrics_tx, metrics_rx) = flume::unbounded();

    let start = Instant::now();
    let scheduler = ConstantRateScheduler::new(100.0, start, Some(Duration::from_secs(4)));
    let generator = SimpleGenerator::get(vec!["http://mock.test/".into()]);
    let executor = MockExecutor::new();
    let collector = HdrMetricsCollector::new(0);

    let worker = Worker::new(
        scheduler,
        generator,
        NoopTransformer,
        executor,
        collector,
        cmd_rx,
        metrics_tx,
        0,
        Duration::from_millis(200),
    );

    // After 2 seconds, quadruple the rate
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        let _ = cmd_tx.send(WorkerCommand::UpdateRate(400.0));
    });

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(worker.run());

    let mut total = 0u64;
    while let Ok(snap) = metrics_rx.try_recv() {
        total += snap.total_requests;
    }

    // First 2s at 100 RPS = 200, last 2s at 400 RPS = 800, total ≈ 1000
    // Allow generous tolerance for timer imprecision
    assert!(
        total > 600,
        "expected ~1000 total requests with rate change, got {total}"
    );
}

#[test]
fn worker_sends_periodic_metrics_snapshots() {
    let (_cmd_tx, cmd_rx) = flume::unbounded();
    let (metrics_tx, metrics_rx) = flume::unbounded();

    let start = Instant::now();
    let scheduler = ConstantRateScheduler::new(100.0, start, Some(Duration::from_secs(2)));
    let generator = SimpleGenerator::get(vec!["http://mock.test/".into()]);
    let executor = MockExecutor::new();
    let collector = HdrMetricsCollector::new(0);

    let worker = Worker::new(
        scheduler,
        generator,
        NoopTransformer,
        executor,
        collector,
        cmd_rx,
        metrics_tx,
        0,
        Duration::from_millis(200), // report every 200ms
    );

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(worker.run());

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
    // Total across all snapshots should match overall request count
    assert!(
        total_requests > 150,
        "total requests across snapshots should be ~200, got {total_requests}"
    );
}

// ---------------------------------------------------------------------------
// Capturing executor: records the RequestSpec of each call
// ---------------------------------------------------------------------------

struct CapturingExecutor {
    captured: Rc<RefCell<Vec<RequestSpec>>>,
}

impl CapturingExecutor {
    fn new() -> (Self, Rc<RefCell<Vec<RequestSpec>>>) {
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
    async fn execute(&self, spec: &RequestSpec, context: &RequestContext) -> ExecutionResult {
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
            response_size: 64,
            error: None,
        }
    }
}

#[test]
fn worker_responds_to_target_update() {
    let (cmd_tx, cmd_rx) = flume::unbounded();
    let (metrics_tx, _metrics_rx) = flume::unbounded();

    let start = Instant::now();
    let scheduler = ConstantRateScheduler::new(100.0, start, Some(Duration::from_secs(3)));
    let generator = SimpleGenerator::get(vec!["http://original.test/".into()]);
    let (executor, captured) = CapturingExecutor::new();
    let collector = HdrMetricsCollector::new(0);

    let worker = Worker::new(
        scheduler,
        generator,
        NoopTransformer,
        executor,
        collector,
        cmd_rx,
        metrics_tx,
        0,
        Duration::from_millis(500),
    );

    // After 500ms, change targets
    let cmd_tx_clone = cmd_tx.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        let _ = cmd_tx_clone.send(WorkerCommand::UpdateTargets(vec![
            "http://new-target.test/a".into(),
            "http://new-target.test/b".into(),
        ]));
        // Let it run another 500ms then stop
        std::thread::sleep(Duration::from_millis(500));
        let _ = cmd_tx_clone.send(WorkerCommand::Stop);
    });

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(worker.run());

    let specs = captured.borrow();
    assert!(specs.len() > 20, "should have generated requests, got {}", specs.len());

    // Early requests should go to original target
    let first_url = &specs[0].url;
    assert!(
        first_url.contains("original.test"),
        "first request should hit original target, got {first_url}"
    );

    // Later requests should go to new targets
    let last_url = &specs[specs.len() - 1].url;
    assert!(
        last_url.contains("new-target.test"),
        "last request should hit new target, got {last_url}"
    );

    // Verify new targets round-robin
    let new_target_specs: Vec<&RequestSpec> = specs
        .iter()
        .filter(|s| s.url.contains("new-target.test"))
        .collect();
    assert!(new_target_specs.len() > 5, "should have several requests to new targets");
    let has_a = new_target_specs.iter().any(|s| s.url.contains("/a"));
    let has_b = new_target_specs.iter().any(|s| s.url.contains("/b"));
    assert!(has_a && has_b, "should round-robin across both new targets");
}

#[test]
fn worker_responds_to_header_update() {
    let (cmd_tx, cmd_rx) = flume::unbounded();
    let (metrics_tx, _metrics_rx) = flume::unbounded();

    let start = Instant::now();
    let scheduler = ConstantRateScheduler::new(100.0, start, Some(Duration::from_secs(3)));
    let generator = SimpleGenerator::get(vec!["http://test.local/".into()]);
    let transformer = HeaderTransformer::new(vec![("X-Original".into(), "yes".into())]);
    let (executor, captured) = CapturingExecutor::new();
    let collector = HdrMetricsCollector::new(0);

    let worker = Worker::new(
        scheduler,
        generator,
        transformer,
        executor,
        collector,
        cmd_rx,
        metrics_tx,
        0,
        Duration::from_millis(500),
    );

    // After 500ms, change headers
    let cmd_tx_clone = cmd_tx.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        let _ = cmd_tx_clone.send(WorkerCommand::UpdateHeaders(vec![
            ("X-New-Header".into(), "new-value".into()),
            ("Authorization".into(), "Bearer token123".into()),
        ]));
        std::thread::sleep(Duration::from_millis(500));
        let _ = cmd_tx_clone.send(WorkerCommand::Stop);
    });

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(worker.run());

    let specs = captured.borrow();
    assert!(specs.len() > 20, "should have generated requests");

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
