use std::time::{Duration, Instant};

use netanvil_metrics::AggregateMetrics;
use netanvil_types::{MetricsSummary, RateController, RateDecision, TimerCommand, WorkerCommand};

use crate::handle::IoWorkerHandle;
use crate::result::TestResult;
use crate::timer_thread::TimerThreadHandle;

/// Snapshot of live progress during a test, emitted each tick.
#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    pub elapsed: Duration,
    pub remaining: Duration,
    pub current_rps: f64,
    pub target_rps: f64,
    pub total_requests: u64,
    pub total_errors: u64,
    pub window: MetricsSummary,
    /// Cumulative histogram buckets from the total aggregate (not just this tick).
    /// Each entry is (upper_bound_seconds, cumulative_count).
    /// Suitable for Prometheus histogram exposition.
    pub latency_buckets: Vec<(f64, u64)>,
}

/// Standard Prometheus latency bucket boundaries in nanoseconds.
const PROMETHEUS_BUCKET_BOUNDS_NS: &[u64] = &[
    1_000_000,       // 1ms
    5_000_000,       // 5ms
    10_000_000,      // 10ms
    25_000_000,      // 25ms
    50_000_000,      // 50ms
    100_000_000,     // 100ms
    250_000_000,     // 250ms
    500_000_000,     // 500ms
    1_000_000_000,   // 1s
    2_500_000_000,   // 2.5s
    5_000_000_000,   // 5s
    10_000_000_000,  // 10s
];

fn histogram_to_prometheus_buckets(hist: &hdrhistogram::Histogram<u64>) -> Vec<(f64, u64)> {
    let mut buckets = Vec::with_capacity(PROMETHEUS_BUCKET_BOUNDS_NS.len() + 1);
    for &bound_ns in PROMETHEUS_BUCKET_BOUNDS_NS {
        let count = hist.count_between(0, bound_ns);
        buckets.push((bound_ns as f64 / 1_000_000_000.0, count));
    }
    // +Inf bucket = total count
    buckets.push((f64::INFINITY, hist.len()));
    buckets
}

/// Orchestrates a load test by distributing rate targets to workers
/// and collecting metrics.
///
/// Runs on its own thread as a synchronous control loop (~10-100Hz).
/// Routes commands through the `TimerThreadHandle` and collects metrics
/// from `IoWorkerHandle`s.
pub struct Coordinator {
    rate_controller: Box<dyn RateController>,
    /// I/O worker handles (metrics collection only — commands go via timer thread).
    io_workers: Vec<IoWorkerHandle>,
    /// Timer thread handle (command routing).
    timer_handle: TimerThreadHandle,
    /// Per-tick window aggregate (reset each tick, used by rate controller)
    tick_aggregate: AggregateMetrics,
    /// Running total across the entire test (never reset, used for final results)
    total_aggregate: AggregateMetrics,
    test_duration: Duration,
    control_interval: Duration,
    start_time: Instant,
    /// Optional callback invoked each tick with live progress.
    on_progress: Option<Box<dyn FnMut(&ProgressUpdate)>>,
    /// Optional external command channel (from API server, distributed leader, etc.).
    /// Drained each tick; commands converted to TimerCommands and forwarded.
    external_command_rx: Option<flume::Receiver<WorkerCommand>>,
    /// Set to true when an external Stop command is received.
    stopped: bool,
    /// Optional external signal source, polled each tick.
    /// Returns `(signal_name, signal_value)` pairs to inject into MetricsSummary.
    external_signal_source: Option<Box<dyn FnMut() -> Vec<(String, f64)>>>,
}

impl Coordinator {
    pub fn new(
        rate_controller: Box<dyn RateController>,
        io_workers: Vec<IoWorkerHandle>,
        timer_handle: TimerThreadHandle,
        test_duration: Duration,
        control_interval: Duration,
    ) -> Self {
        Self {
            rate_controller,
            io_workers,
            timer_handle,
            tick_aggregate: AggregateMetrics::new(),
            total_aggregate: AggregateMetrics::new(),
            test_duration,
            control_interval,
            start_time: Instant::now(),
            on_progress: None,
            external_command_rx: None,
            stopped: false,
            external_signal_source: None,
        }
    }

    /// Set a callback that receives live progress updates each tick.
    pub fn on_progress(&mut self, f: impl FnMut(&ProgressUpdate) + 'static) {
        self.on_progress = Some(Box::new(f));
    }

    /// Set an external command channel. Commands received here are forwarded
    /// to workers each tick. Used by the HTTP control API and distributed leader.
    pub fn set_external_commands(&mut self, rx: flume::Receiver<WorkerCommand>) {
        self.external_command_rx = Some(rx);
    }

    /// Set an external signal source, polled each tick before the rate controller.
    /// Returns `(signal_name, value)` pairs injected into MetricsSummary.
    /// Used for server-reported metrics (e.g. proxy load, queue depth).
    pub fn set_external_signal_source(
        &mut self,
        f: impl FnMut() -> Vec<(String, f64)> + 'static,
    ) {
        self.external_signal_source = Some(Box::new(f));
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
        // Drain external commands (from API server, distributed leader, etc.)
        // Collect into a Vec first to avoid borrow conflict with &mut self.
        let commands: Vec<_> = self
            .external_command_rx
            .as_ref()
            .map(|rx| {
                let mut cmds = Vec::new();
                while let Ok(cmd) = rx.try_recv() {
                    cmds.push(cmd);
                }
                cmds
            })
            .unwrap_or_default();
        for cmd in commands {
            self.handle_external_command(cmd);
        }

        // Collect metrics from all I/O workers
        self.tick_aggregate.reset();
        for worker in &self.io_workers {
            while let Ok(snapshot) = worker.metrics_rx.try_recv() {
                self.tick_aggregate.merge(&snapshot);
                self.total_aggregate.merge(&snapshot);
            }
        }

        // Rate controller computes new target from the recent window
        let mut summary = self.tick_aggregate.to_summary();

        // Inject external signals (e.g. server-reported load metrics)
        if let Some(ref mut source) = self.external_signal_source {
            let signals = source();
            if !signals.is_empty() {
                tracing::debug!(?signals, "injected external signals into metrics summary");
            }
            summary.external_signals = signals;
        }

        let decision = self.rate_controller.update(&summary);

        tracing::debug!(
            tick_requests = summary.total_requests,
            total_requests = self.total_aggregate.total_requests(),
            target_rps = decision.target_rps,
            "coordinator tick"
        );

        // Distribute to workers via timer thread
        self.distribute_rate(decision.target_rps);

        // Emit progress update
        if let Some(ref mut callback) = self.on_progress {
            let elapsed = self.start_time.elapsed();
            let buckets = histogram_to_prometheus_buckets(self.total_aggregate.histogram());
            let update = ProgressUpdate {
                elapsed,
                remaining: self.test_duration.saturating_sub(elapsed),
                current_rps: summary.request_rate,
                target_rps: decision.target_rps,
                total_requests: self.total_aggregate.total_requests(),
                total_errors: self.total_aggregate.total_errors(),
                window: summary,
                latency_buckets: buckets,
            };
            callback(&update);
        }

        decision
    }

    /// Get current aggregate metrics (for distributed layer to forward).
    pub fn aggregate_metrics(&self) -> &AggregateMetrics {
        &self.total_aggregate
    }

    fn handle_external_command(&mut self, cmd: WorkerCommand) {
        let timer_cmd = match cmd {
            WorkerCommand::UpdateRate(rps) => {
                self.rate_controller.set_rate(rps);
                TimerCommand::UpdateRate(rps)
            }
            WorkerCommand::UpdateTargets(targets) => TimerCommand::UpdateTargets(targets),
            WorkerCommand::UpdateHeaders(headers) => TimerCommand::UpdateHeaders(headers),
            WorkerCommand::Stop => {
                self.stopped = true;
                TimerCommand::Stop
            }
        };
        let _ = self.timer_handle.command_tx.send(timer_cmd);
    }

    /// Send total rate to timer thread — it distributes to schedulers internally.
    fn distribute_rate(&self, total_rps: f64) {
        let _ = self
            .timer_handle
            .command_tx
            .send(TimerCommand::UpdateRate(total_rps));
    }

    fn stop_workers(&self) {
        let _ = self.timer_handle.command_tx.send(TimerCommand::Stop);
    }

    fn is_test_complete(&self) -> bool {
        self.stopped || self.start_time.elapsed() >= self.test_duration
    }

    fn collect_final_metrics(&mut self) -> TestResult {
        // Give workers a moment to send final snapshots, then join
        std::thread::sleep(Duration::from_millis(200));

        // Drain any remaining snapshots
        for worker in &self.io_workers {
            while let Ok(snapshot) = worker.metrics_rx.try_recv() {
                self.total_aggregate.merge(&snapshot);
            }
        }

        // Join timer thread first (it sends Stop to workers)
        if let Some(handle) = self.timer_handle.thread.take() {
            let _ = handle.join();
        }

        // Join worker threads
        for worker in &mut self.io_workers {
            if let Some(handle) = worker.thread.take() {
                let _ = handle.join();
            }
        }

        // Drain once more after join (workers send a final snapshot on exit)
        for worker in &self.io_workers {
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
