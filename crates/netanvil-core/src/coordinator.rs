use std::time::{Duration, Instant};

use netanvil_metrics::AggregateMetrics;
use netanvil_types::{RateController, RateDecision, WorkerCommand};

use crate::handle::WorkerHandle;
use crate::result::TestResult;

/// Orchestrates a load test by distributing rate targets to workers
/// and collecting metrics.
///
/// Runs on its own thread as a synchronous control loop (~10-100Hz).
/// The coordinator doesn't know worker internals — it communicates
/// exclusively via `WorkerHandle` channels.
pub struct Coordinator {
    rate_controller: Box<dyn RateController>,
    workers: Vec<WorkerHandle>,
    /// Per-tick window aggregate (reset each tick, used by rate controller)
    tick_aggregate: AggregateMetrics,
    /// Running total across the entire test (never reset, used for final results)
    total_aggregate: AggregateMetrics,
    test_duration: Duration,
    control_interval: Duration,
    start_time: Instant,
}

impl Coordinator {
    pub fn new(
        rate_controller: Box<dyn RateController>,
        workers: Vec<WorkerHandle>,
        test_duration: Duration,
        control_interval: Duration,
    ) -> Self {
        Self {
            rate_controller,
            workers,
            tick_aggregate: AggregateMetrics::new(),
            total_aggregate: AggregateMetrics::new(),
            test_duration,
            control_interval,
            start_time: Instant::now(),
        }
    }

    /// Run the full coordinator loop. Blocks until the test completes.
    pub fn run(&mut self) -> TestResult {
        // Distribute initial rate
        let initial_rps = self.rate_controller.current_rate();
        self.distribute_rate(initial_rps);

        loop {
            std::thread::sleep(self.control_interval);
            self.tick();

            if self.is_test_complete() {
                break;
            }
        }

        self.stop_workers();
        self.collect_final_metrics()
    }

    /// Single control loop iteration.
    ///
    /// Public so a future DistributedCoordinator can call this and also
    /// forward metrics/decisions over the network.
    pub fn tick(&mut self) -> RateDecision {
        // Collect metrics from all workers into the tick window
        self.tick_aggregate.reset();
        for worker in &self.workers {
            while let Ok(snapshot) = worker.metrics_rx.try_recv() {
                self.tick_aggregate.merge(&snapshot);
                self.total_aggregate.merge(&snapshot);
            }
        }

        // Rate controller computes new target from the recent window
        let summary = self.tick_aggregate.to_summary();
        let decision = self.rate_controller.update(&summary);

        // Distribute to workers
        self.distribute_rate(decision.target_rps);

        decision
    }

    /// Get current aggregate metrics (for distributed layer to forward).
    pub fn aggregate_metrics(&self) -> &AggregateMetrics {
        &self.total_aggregate
    }

    fn distribute_rate(&self, total_rps: f64) {
        if self.workers.is_empty() {
            return;
        }
        let per_core = total_rps / self.workers.len() as f64;
        for w in &self.workers {
            let _ = w.command_tx.send(WorkerCommand::UpdateRate(per_core));
        }
    }

    fn stop_workers(&self) {
        for w in &self.workers {
            let _ = w.command_tx.send(WorkerCommand::Stop);
        }
    }

    fn is_test_complete(&self) -> bool {
        self.start_time.elapsed() >= self.test_duration
    }

    fn collect_final_metrics(&mut self) -> TestResult {
        // Give workers a moment to send final snapshots, then join
        std::thread::sleep(Duration::from_millis(200));

        // Drain any remaining snapshots into the total
        for worker in &self.workers {
            while let Ok(snapshot) = worker.metrics_rx.try_recv() {
                self.total_aggregate.merge(&snapshot);
            }
        }

        // Join worker threads
        for worker in &mut self.workers {
            if let Some(handle) = worker.thread.take() {
                let _ = handle.join();
            }
        }

        // Drain once more after join (workers send a final snapshot on exit)
        for worker in &self.workers {
            while let Ok(snapshot) = worker.metrics_rx.try_recv() {
                self.total_aggregate.merge(&snapshot);
            }
        }

        let elapsed = self.start_time.elapsed();
        let hist = self.total_aggregate.histogram();

        TestResult {
            total_requests: self.total_aggregate.total_requests(),
            total_errors: self.total_aggregate.total_errors(),
            duration: elapsed,
            latency_p50: Duration::from_nanos(hist.value_at_quantile(0.50)),
            latency_p90: Duration::from_nanos(hist.value_at_quantile(0.90)),
            latency_p99: Duration::from_nanos(hist.value_at_quantile(0.99)),
            latency_max: Duration::from_nanos(hist.max()),
            request_rate: self.total_aggregate.total_requests() as f64 / elapsed.as_secs_f64(),
            error_rate: if self.total_aggregate.total_requests() > 0 {
                self.total_aggregate.total_errors() as f64
                    / self.total_aggregate.total_requests() as f64
            } else {
                0.0
            },
        }
    }
}
