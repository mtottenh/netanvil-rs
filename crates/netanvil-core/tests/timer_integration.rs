//! End-to-end integration tests for the timer thread + I/O worker architecture.
//!
//! These tests wire up the full architecture (timer thread dispatching to
//! I/O workers on compio runtimes) without using run_test() — they construct
//! the components directly. This validates the architecture independent of
//! the engine wiring.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use netanvil_core::io_worker::{io_worker_loop, IoWorkerConfig};
use netanvil_core::timer_thread::{self, FIRE_CHANNEL_CAPACITY};
use netanvil_core::{ConstantRateScheduler, NoopTransformer, SimpleGenerator};
use netanvil_metrics::HdrMetricsCollector;
use netanvil_types::{
    EventRecorder, ExecutionResult, HttpRequestSpec, MetricsSnapshot, NoopEventRecorder,
    RequestContext, RequestExecutor, RequestScheduler, ScheduledRequest, TimerCommand,
    TimingBreakdown,
};

// ---------------------------------------------------------------------------
// Mock executor for integration tests
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
    type PacketSource = netanvil_types::NoopPacketSource;

    async fn execute(&self, _spec: &HttpRequestSpec, context: &RequestContext) -> ExecutionResult {
        self.call_count.set(self.call_count.get() + 1);
        ExecutionResult {
            request_id: context.request_id,
            intended_time: context.intended_time,
            sent_time: context.sent_time,
            actual_time: context.actual_time,
            dispatch_time: context.dispatch_time,
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

/// Spin up the full timer thread + N I/O worker architecture.
///
/// Returns: (cmd_tx, metrics_rxs, timer_join, worker_joins)
fn spawn_full_architecture(
    num_workers: usize,
    rps_per_worker: f64,
    duration: Duration,
    metrics_interval: Duration,
) -> (
    flume::Sender<TimerCommand>,
    Vec<flume::Receiver<MetricsSnapshot>>,
    std::thread::JoinHandle<()>,
    Vec<std::thread::JoinHandle<()>>,
) {
    let start = Instant::now();

    // Create schedulers
    let mut schedulers: Vec<Box<dyn RequestScheduler>> = Vec::new();
    for _ in 0..num_workers {
        schedulers.push(Box::new(ConstantRateScheduler::new(
            rps_per_worker,
            start,
            Some(duration),
        )));
    }

    // Create fire channels
    let mut fire_txs = Vec::new();
    let mut fire_rxs = Vec::new();
    for _ in 0..num_workers {
        let (tx, rx) = flume::bounded::<ScheduledRequest>(FIRE_CHANNEL_CAPACITY);
        fire_txs.push(tx);
        fire_rxs.push(rx);
    }

    // Timer command channel
    let (cmd_tx, cmd_rx) = flume::unbounded();

    // Spawn I/O workers
    let mut worker_joins = Vec::new();
    let mut metrics_rxs = Vec::new();

    for core_id in 0..num_workers {
        let fire_rx = fire_rxs.remove(0);
        let (metrics_tx, metrics_rx) = flume::unbounded();
        metrics_rxs.push(metrics_rx);

        let thread = std::thread::Builder::new()
            .name(format!("test-io-{core_id}"))
            .spawn(move || {
                let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
                rt.block_on(async {
                    let generator = SimpleGenerator::get(vec!["http://mock.test/".into()]);
                    let executor = MockExecutor::new();
                    let collector = HdrMetricsCollector::new(0, vec![], false);

                    io_worker_loop(
                        IoWorkerConfig {
                            fire_rx,
                            metrics_tx,
                            core_id,
                            metrics_interval,
                            graceful_shutdown: true,
                        },
                        generator,
                        Rc::new(NoopTransformer),
                        Rc::new(executor),
                        Rc::new(collector),
                        Rc::new(NoopEventRecorder) as Rc<dyn EventRecorder>,
                        Rc::new(netanvil_core::in_flight::InFlightLimit::new(0)),
                    )
                    .await;
                });
            })
            .unwrap();

        worker_joins.push(thread);
    }

    // Spawn timer thread
    let timer_join = std::thread::Builder::new()
        .name("test-timer".into())
        .spawn(move || {
            timer_thread::timer_loop(
                schedulers,
                fire_txs,
                cmd_rx,
                timer_thread::TimerStats::new(),
            );
        })
        .unwrap();

    (cmd_tx, metrics_rxs, timer_join, worker_joins)
}

fn collect_total_requests(metrics_rxs: &[flume::Receiver<MetricsSnapshot>]) -> u64 {
    let mut total = 0u64;
    for rx in metrics_rxs {
        while let Ok(snap) = rx.try_recv() {
            total += snap.total_requests;
        }
    }
    total
}

#[test]
fn timer_plus_workers_achieve_target_rate() {
    // 2 workers at 250 RPS each (500 total) for 2 seconds → ~1000 requests.
    let (cmd_tx, metrics_rxs, timer_join, worker_joins) =
        spawn_full_architecture(2, 250.0, Duration::from_secs(2), Duration::from_millis(200));

    // Wait for timer thread (schedulers exhaust after 2 seconds)
    timer_join.join().unwrap();

    // Wait for workers to finish processing
    for j in worker_joins {
        j.join().unwrap();
    }

    let total = collect_total_requests(&metrics_rxs);

    // Allow 25% tolerance
    let expected = 1000u64;
    let lower = (expected as f64 * 0.75) as u64;
    let upper = (expected as f64 * 1.25) as u64;
    assert!(
        total >= lower && total <= upper,
        "expected ~{expected} total requests across 2 workers, got {total} (bounds: {lower}..{upper})"
    );

    drop(cmd_tx);
}

#[test]
fn timer_plus_workers_rate_change_mid_test() {
    // Start at 100 RPS/worker (200 total), after 1s change to 400 RPS/worker (800 total).
    // Duration 2s. Expected: ~200 (first 1s) + ~800 (last 1s) = ~1000
    let (cmd_tx, metrics_rxs, timer_join, worker_joins) =
        spawn_full_architecture(2, 100.0, Duration::from_secs(2), Duration::from_millis(200));

    // After 1 second, quadruple the rate (total 800 → per worker 400)
    let cmd_tx_clone = cmd_tx.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        let _ = cmd_tx_clone.send(TimerCommand::UpdateRate(800.0));
    });

    timer_join.join().unwrap();
    for j in worker_joins {
        j.join().unwrap();
    }

    let total = collect_total_requests(&metrics_rxs);

    // ~200 + ~800 = ~1000, allow generous tolerance
    assert!(
        total > 500,
        "expected ~1000 with rate change from 200→800 total, got {total}"
    );
    assert!(
        total < 1500,
        "too many requests with rate change, got {total}"
    );

    drop(cmd_tx);
}

#[test]
fn timer_plus_workers_stop_command() {
    // Long duration (60s) but stop after 500ms — verify clean shutdown.
    let (cmd_tx, metrics_rxs, timer_join, worker_joins) = spawn_full_architecture(
        2,
        100.0,
        Duration::from_secs(60),
        Duration::from_millis(100),
    );

    // Stop after 500ms
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        let _ = cmd_tx.send(TimerCommand::Stop);
    });

    let before = Instant::now();
    timer_join.join().unwrap();
    for j in worker_joins {
        j.join().unwrap();
    }
    let elapsed = before.elapsed();

    assert!(
        elapsed < Duration::from_secs(3),
        "should stop promptly after Stop command, took {:?}",
        elapsed
    );

    let total = collect_total_requests(&metrics_rxs);

    // Should have some requests (~50 per worker in 500ms at 100 RPS)
    assert!(
        total > 30,
        "expected some requests before stop, got {total}"
    );
    assert!(
        total < 500,
        "too many requests for 500ms at 200 total RPS: {total}"
    );
}

#[test]
fn timer_plus_workers_distribute_load_evenly() {
    // 4 workers at 100 RPS each for 1 second — each should get ~100 requests.
    let (cmd_tx, metrics_rxs, timer_join, worker_joins) =
        spawn_full_architecture(4, 100.0, Duration::from_secs(1), Duration::from_millis(200));

    timer_join.join().unwrap();
    for j in worker_joins {
        j.join().unwrap();
    }

    let mut per_worker = Vec::new();
    for rx in &metrics_rxs {
        let mut count = 0u64;
        while let Ok(snap) = rx.try_recv() {
            count += snap.total_requests;
        }
        per_worker.push(count);
    }

    let total: u64 = per_worker.iter().sum();
    assert!(
        total > 300 && total < 500,
        "total should be ~400, got {total}"
    );

    // Each worker should have a reasonable share
    for (i, &count) in per_worker.iter().enumerate() {
        let expected = total as f64 / 4.0;
        let lower = (expected * 0.50) as u64;
        let upper = (expected * 1.50) as u64;
        assert!(
            count >= lower && count <= upper,
            "worker {i} got {count}, expected ~{expected:.0} (bounds: {lower}..{upper}). All: {per_worker:?}"
        );
    }

    drop(cmd_tx);
}

#[test]
fn timer_plus_workers_metrics_snapshots_flow() {
    // Verify that periodic metrics snapshots arrive from workers throughout the test.
    let (cmd_tx, metrics_rxs, timer_join, worker_joins) =
        spawn_full_architecture(2, 200.0, Duration::from_secs(2), Duration::from_millis(200));

    // Collect metrics while the test runs
    let mut snapshot_counts = vec![0u64; 2];

    // Wait for completion
    timer_join.join().unwrap();
    for j in worker_joins {
        j.join().unwrap();
    }

    for (i, rx) in metrics_rxs.iter().enumerate() {
        while let Ok(_snap) = rx.try_recv() {
            snapshot_counts[i] += 1;
        }
    }

    // Each worker should have produced multiple snapshots over 2 seconds
    for (i, &count) in snapshot_counts.iter().enumerate() {
        assert!(
            count >= 3,
            "worker {i} should have produced at least 3 snapshots over 2s, got {count}"
        );
    }

    drop(cmd_tx);
}

#[test]
fn timer_plus_workers_coordinated_omission_tracking() {
    // Verify that the intended_time in Fire messages is correctly propagated
    // to the RequestContext, giving accurate coordinated omission data.
    //
    // We check this by verifying that intended_time ≤ actual_time for all
    // requests (the timer dispatches the intended time, the worker records
    // the actual dispatch time).
    let (fire_tx, fire_rx) = flume::bounded::<ScheduledRequest>(128);
    let (metrics_tx, _metrics_rx) = flume::unbounded();

    let intended_time = Instant::now() - Duration::from_millis(100); // in the past

    // Send from another thread so the worker can process between messages
    std::thread::spawn(move || {
        fire_tx
            .send(ScheduledRequest::Fire {
                intended_time,
                sent_time: intended_time,
            })
            .unwrap();
        // Give the worker time to process and spawn the task
        std::thread::sleep(Duration::from_millis(200));
        fire_tx.send(ScheduledRequest::Stop).unwrap();
    });

    // Use a capturing executor to verify the context
    struct ContextCapture {
        contexts: std::cell::RefCell<Vec<(Instant, Instant)>>,
    }

    impl RequestExecutor for ContextCapture {
        type Spec = HttpRequestSpec;
        type PacketSource = netanvil_types::NoopPacketSource;

        async fn execute(
            &self,
            _spec: &HttpRequestSpec,
            context: &RequestContext,
        ) -> ExecutionResult {
            self.contexts
                .borrow_mut()
                .push((context.intended_time, context.actual_time));
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
                status: Some(200),
                bytes_sent: 0,
                response_size: 64,
                error: None,
                response_headers: None,
                response_body: None,
            }
        }
    }

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    rt.block_on(async {
        let generator = SimpleGenerator::get(vec!["http://mock.test/".into()]);
        let executor = ContextCapture {
            contexts: std::cell::RefCell::new(Vec::new()),
        };
        let exec_rc = Rc::new(executor);
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
            exec_rc.clone(),
            Rc::new(collector),
            Rc::new(NoopEventRecorder) as Rc<dyn EventRecorder>,
            Rc::new(netanvil_core::in_flight::InFlightLimit::new(0)),
        )
        .await;

        let contexts = exec_rc.contexts.borrow();
        assert_eq!(contexts.len(), 1, "should have executed 1 request");

        let (intended, actual) = contexts[0];
        assert_eq!(
            intended, intended_time,
            "intended_time should be passed through from Fire message"
        );
        assert!(
            actual > intended,
            "actual_time ({actual:?}) should be after intended_time ({intended:?})"
        );
        // The delay should be at least 100ms since we set intended_time 100ms in the past
        let delay = actual - intended;
        assert!(
            delay >= Duration::from_millis(90),
            "CO delay should be ~100ms, got {delay:?}"
        );
    });
}
