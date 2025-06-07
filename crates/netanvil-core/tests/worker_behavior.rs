use std::cell::Cell;
use std::time::{Duration, Instant};

use netanvil_core::{
    ConstantRateScheduler, NoopTransformer, SimpleGenerator, Worker,
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
    let collector = HdrMetricsCollector::new();

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
    let collector = HdrMetricsCollector::new();

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
    let collector = HdrMetricsCollector::new();

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
    let collector = HdrMetricsCollector::new();

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
