use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use netanvil_metrics::AggregateMetrics;
use netanvil_types::{
    MetricsSummary, RateController, RateDecision, SaturationAssessment, SaturationInfo,
    TimerCommand, WorkerCommand,
};

use crate::handle::IoWorkerHandle;
use crate::result::TestResult;
use crate::timer_thread::TimerThreadHandle;

/// Sliding window for smoothing reported RPS / throughput.
///
/// Uses struct-of-arrays layout so aggregation iterates contiguous
/// `&[u64]` slices — one cache line per counter field.
struct SlidingWindow {
    timestamps: VecDeque<Instant>,
    total_requests: VecDeque<u64>,
    total_errors: VecDeque<u64>,
    bytes_sent: VecDeque<u64>,
    bytes_received: VecDeque<u64>,
    window_duration: Duration,
}

impl SlidingWindow {
    fn new(window_duration: Duration) -> Self {
        Self {
            timestamps: VecDeque::new(),
            total_requests: VecDeque::new(),
            total_errors: VecDeque::new(),
            bytes_sent: VecDeque::new(),
            bytes_received: VecDeque::new(),
            window_duration,
        }
    }

    /// Push one tick's worth of counters.
    fn push(&mut self, now: Instant, requests: u64, errors: u64, sent: u64, received: u64) {
        self.timestamps.push_back(now);
        self.total_requests.push_back(requests);
        self.total_errors.push_back(errors);
        self.bytes_sent.push_back(sent);
        self.bytes_received.push_back(received);
        self.evict(now);
    }

    /// Remove entries older than the window.
    fn evict(&mut self, now: Instant) {
        let cutoff = now - self.window_duration;
        while let Some(&ts) = self.timestamps.front() {
            if ts >= cutoff {
                break;
            }
            self.timestamps.pop_front();
            self.total_requests.pop_front();
            self.total_errors.pop_front();
            self.bytes_sent.pop_front();
            self.bytes_received.pop_front();
        }
    }

    /// Smoothed requests-per-second over the window.
    ///
    /// Each entry's request count covers the interval *ending* at its
    /// timestamp.  The measured span runs from `timestamps[0]` to
    /// `timestamps[N-1]`, so entry[0]'s data falls *before* the span.
    /// We skip it to avoid a fencepost overcount of one-tick/span
    /// (≈ control_interval / window_duration).
    fn request_rate(&self) -> f64 {
        if self.timestamps.len() < 2 {
            // With fewer than 2 entries we can't compute a time span;
            // fall back to the raw tick value.
            return self.total_requests.back().copied().unwrap_or(0) as f64
                / self.window_duration.as_secs_f64().max(0.001);
        }
        let span = self
            .timestamps
            .back()
            .unwrap()
            .saturating_duration_since(*self.timestamps.front().unwrap());
        let secs = span.as_secs_f64().max(0.001);
        let total: u64 = self.total_requests.iter().skip(1).sum();
        total as f64 / secs
    }
}

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
    /// Client/server saturation assessment for this tick.
    pub saturation: SaturationInfo,
    /// Total bytes sent across all cores (cumulative).
    pub total_bytes_sent: u64,
    /// Total bytes received across all cores (cumulative).
    pub total_bytes_received: u64,
    /// Protocol-level packet counters (cumulative): (sent, received, lost).
    pub packets_sent: u64,
    pub packets_received: u64,
    pub packets_lost: u64,
    /// Total timeout completions (cumulative across all cores).
    pub total_timeouts: u64,
}

/// Standard Prometheus latency bucket boundaries in nanoseconds.
const PROMETHEUS_BUCKET_BOUNDS_NS: &[u64] = &[
    1_000_000,      // 1ms
    5_000_000,      // 5ms
    10_000_000,     // 10ms
    25_000_000,     // 25ms
    50_000_000,     // 50ms
    100_000_000,    // 100ms
    250_000_000,    // 250ms
    500_000_000,    // 500ms
    1_000_000_000,  // 1s
    2_500_000_000,  // 2.5s
    5_000_000_000,  // 5s
    10_000_000_000, // 10s
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
    on_progress: Option<crate::ProgressCallback>,
    /// Optional external command channel (from API server, distributed leader, etc.).
    /// Drained each tick; commands converted to TimerCommands and forwarded.
    external_command_rx: Option<flume::Receiver<WorkerCommand>>,
    /// Set to true when an external Stop command is received.
    stopped: bool,
    /// Hold state: when Held, controller.update() is skipped.
    hold_state: netanvil_types::HoldState,
    /// Optional external signal source (pull-based), polled each tick.
    /// Returns `(signal_name, signal_value)` pairs to inject into MetricsSummary.
    external_signal_source: Option<crate::SignalSourceFn>,
    /// Optional pushed signal source (push-based), read each tick.
    /// Pushed signals override polled signals with the same key.
    pushed_signal_source: Option<crate::SignalSourceFn>,
    /// Last-seen timer stats values (for computing per-tick deltas).
    last_timer_dispatched: u64,
    last_timer_dropped: u64,
    /// Stop after this many total requests.
    max_requests: Option<u64>,
    /// Stop if cumulative errors exceed this.
    autostop_threshold: Option<u64>,
    /// Stop if cumulative refused connections exceed this.
    refusestop_threshold: Option<u64>,
    /// Warmup period — metrics merged into total_aggregate only after warmup ends.
    warmup_duration: Option<Duration>,
    /// Whether warmup has completed.
    warmup_complete: bool,
    /// Stop when cumulative bytes received reaches this.
    target_bytes: Option<u64>,
    /// Sliding window for smoothing reported RPS (not fed to rate controller).
    sliding_window: SlidingWindow,
    /// Previous tick's saturation assessment (for logging transitions).
    last_assessment: SaturationAssessment,
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
            hold_state: netanvil_types::HoldState::Released,
            external_signal_source: None,
            pushed_signal_source: None,
            last_timer_dispatched: 0,
            last_timer_dropped: 0,
            max_requests: None,
            autostop_threshold: None,
            refusestop_threshold: None,
            warmup_duration: None,
            warmup_complete: false,
            target_bytes: None,
            sliding_window: SlidingWindow::new(Duration::from_secs(2)),
            last_assessment: SaturationAssessment::Healthy,
        }
    }

    /// Configure request count limit.
    pub fn max_requests(&mut self, n: u64) {
        self.max_requests = Some(n);
    }

    /// Configure byte count termination threshold.
    pub fn target_bytes(&mut self, n: u64) {
        self.target_bytes = Some(n);
    }

    /// Configure auto-stop on error count threshold.
    pub fn autostop_threshold(&mut self, n: u64) {
        self.autostop_threshold = Some(n);
    }

    /// Configure auto-stop on refused connection count threshold.
    pub fn refusestop_threshold(&mut self, n: u64) {
        self.refusestop_threshold = Some(n);
    }

    /// Configure warmup duration (metrics excluded until warmup ends).
    pub fn warmup_duration(&mut self, d: Duration) {
        self.warmup_duration = Some(d);
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

    /// Set an external signal source (pull-based), polled each tick.
    /// Returns `(signal_name, value)` pairs injected into MetricsSummary.
    /// Used for server-reported metrics (e.g. proxy load, queue depth).
    pub fn set_external_signal_source(&mut self, f: impl FnMut() -> Vec<(String, f64)> + 'static) {
        self.external_signal_source = Some(Box::new(f));
    }

    /// Configure response signal extraction for PID control.
    /// Aggregated numeric header values are injected into `MetricsSummary::external_signals`.
    pub fn set_response_signal_configs(
        &mut self,
        configs: Vec<netanvil_types::config::ResponseSignalConfig>,
    ) {
        self.tick_aggregate.set_signal_configs(configs.clone());
        self.total_aggregate.set_signal_configs(configs);
    }

    /// Set a pushed signal source, read each tick.
    /// Used for signals pushed via HTTP API (`PUT /signal`).
    /// Pushed signals override polled signals with the same key.
    pub fn set_pushed_signal_source(&mut self, f: impl FnMut() -> Vec<(String, f64)> + 'static) {
        self.pushed_signal_source = Some(Box::new(f));
    }

    /// Run the full coordinator loop. Blocks until the test completes.
    pub fn run(&mut self, ready_tx: Option<flume::Sender<()>>) -> TestResult {
        // Distribute initial rate
        let initial_rps = self.rate_controller.current_rate();
        self.distribute_rate(initial_rps);

        tracing::info!(
            initial_rps,
            duration_secs = self.test_duration.as_secs(),
            control_interval_ms = self.control_interval.as_millis() as u64,
            num_workers = self.io_workers.len(),
            "test started"
        );

        // Signal that the coordinator is ready to process commands.
        // Ignore send error: the receiver may have timed out and dropped.
        if let Some(tx) = ready_tx {
            let _ = tx.send(());
        }

        loop {
            std::thread::sleep(self.control_interval);
            self.tick();

            if self.is_test_complete() {
                break;
            }
        }

        let elapsed = self.start_time.elapsed();
        tracing::info!(
            elapsed_secs = format!("{:.1}", elapsed.as_secs_f64()),
            total_requests = self.total_aggregate.total_requests(),
            total_errors = self.total_aggregate.total_errors(),
            final_rps = format!("{:.1}", self.rate_controller.current_rate()),
            "test completed"
        );

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
                // During warmup, don't accumulate into total_aggregate.
                // This ensures the final results exclude warmup data.
                if self.warmup_complete {
                    self.total_aggregate.merge(&snapshot);
                }
            }
        }

        // Check if warmup just completed
        if !self.warmup_complete {
            if let Some(warmup) = self.warmup_duration {
                if self.start_time.elapsed() >= warmup {
                    self.warmup_complete = true;
                    tracing::info!("warmup complete, metrics collection started");
                }
            } else {
                // No warmup configured
                self.warmup_complete = true;
            }
        }

        // Rate controller computes new target from the recent window
        let mut summary = self.tick_aggregate.to_summary();

        // Inject external signals from pull, push, and response header sources.
        // Response signals are already in summary.external_signals from to_summary().
        // Polled signals are added next; pushed signals override any with the same key.
        if let Some(ref mut source) = self.external_signal_source {
            for (name, value) in source() {
                if let Some(existing) = summary
                    .external_signals
                    .iter_mut()
                    .find(|(k, _)| k == &name)
                {
                    existing.1 = value;
                } else {
                    summary.external_signals.push((name, value));
                }
            }
        }
        if let Some(ref mut source) = self.pushed_signal_source {
            for (name, value) in source() {
                if let Some(existing) = summary
                    .external_signals
                    .iter_mut()
                    .find(|(k, _)| k == &name)
                {
                    existing.1 = value;
                } else {
                    summary.external_signals.push((name, value));
                }
            }
        }
        if !summary.external_signals.is_empty() {
            tracing::info!(signals = ?summary.external_signals, "external signals for rate controller");
        }

        let decision = match self.hold_state {
            netanvil_types::HoldState::Held { rps } => {
                // Skip controller.update() — controller state is frozen.
                RateDecision { target_rps: rps }
            }
            netanvil_types::HoldState::Released => {
                self.rate_controller.update(&summary)
            }
        };

        // Push this tick's counters into the sliding window for
        // smoothed RPS reporting (API / Prometheus / CLI).
        let now = Instant::now();
        self.sliding_window.push(
            now,
            summary.total_requests,
            summary.total_errors,
            summary.bytes_sent,
            summary.bytes_received,
        );
        let smoothed_rps = self.sliding_window.request_rate();

        tracing::info!(
            tick_requests = summary.total_requests,
            total_requests = self.total_aggregate.total_requests(),
            smoothed_rps,
            target_rps = decision.target_rps,
            error_rate = format!("{:.4}", summary.error_rate),
            p99_ms = format!("{:.2}", summary.latency_p99_ns as f64 / 1_000_000.0),
            "coordinator tick"
        );

        // Distribute to workers via timer thread
        self.distribute_rate(decision.target_rps);

        // Compute saturation info from timer stats + tick aggregate.
        // Uses smoothed_rps so empty ticks don't trigger false ClientSaturated.
        let saturation = self.compute_saturation(&summary, decision.target_rps, smoothed_rps);

        // Emit progress update
        if let Some(ref mut callback) = self.on_progress {
            let elapsed = self.start_time.elapsed();
            let buckets = histogram_to_prometheus_buckets(self.total_aggregate.histogram());
            let pkts = self.total_aggregate.packet_counters();
            let update = ProgressUpdate {
                elapsed,
                remaining: self.test_duration.saturating_sub(elapsed),
                current_rps: smoothed_rps,
                target_rps: decision.target_rps,
                total_requests: self.total_aggregate.total_requests(),
                total_errors: self.total_aggregate.total_errors(),
                window: summary,
                latency_buckets: buckets,
                saturation,
                total_bytes_sent: self.total_aggregate.bytes_sent(),
                total_bytes_received: self.total_aggregate.bytes_received(),
                packets_sent: pkts.0,
                packets_received: pkts.1,
                packets_lost: pkts.2,
                total_timeouts: self.total_aggregate.timeout_count(),
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
                // Deprecated: translate to hold semantics.
                tracing::warn!(
                    rps,
                    "UpdateRate is deprecated, use Hold instead"
                );
                self.hold_state = netanvil_types::HoldState::Held { rps };
                Some(TimerCommand::UpdateRate(rps))
            }
            WorkerCommand::UpdateTargets(targets) => {
                Some(TimerCommand::UpdateTargets(targets))
            }
            WorkerCommand::UpdateMetadata(headers) => {
                Some(TimerCommand::UpdateMetadata(headers))
            }
            WorkerCommand::Stop => {
                self.stopped = true;
                Some(TimerCommand::Stop)
            }
            WorkerCommand::Hold(hold_cmd) => {
                match hold_cmd {
                    netanvil_types::HoldCommand::Hold(rps) => {
                        tracing::info!(rps, "rate hold engaged");
                        self.hold_state = netanvil_types::HoldState::Held { rps };
                    }
                    netanvil_types::HoldCommand::Release => {
                        tracing::info!(
                            controller_rps = self.rate_controller.current_rate(),
                            "rate hold released, resuming controller"
                        );
                        self.hold_state = netanvil_types::HoldState::Released;
                    }
                }
                None
            }
            WorkerCommand::ControllerUpdate {
                action,
                params,
                response_tx,
            } => {
                let result = self.rate_controller.apply_update(&action, &params);
                let _ = response_tx.send(result);
                None
            }
            WorkerCommand::ControllerInfo { response_tx } => {
                let info = self.rate_controller.controller_info();
                let view = netanvil_types::ControllerView {
                    info,
                    held: matches!(self.hold_state, netanvil_types::HoldState::Held { .. }),
                    held_rps: match self.hold_state {
                        netanvil_types::HoldState::Held { rps } => Some(rps),
                        netanvil_types::HoldState::Released => None,
                    },
                };
                let _ = response_tx.send(view);
                None
            }
        };
        if let Some(cmd) = timer_cmd {
            let _ = self.timer_handle.command_tx.send(cmd);
        }
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

    /// Compute client/server saturation info from timer stats and tick metrics.
    /// `smoothed_rps` comes from the sliding window so that empty ticks don't
    /// trigger false `ClientSaturated` assessments.
    fn compute_saturation(
        &mut self,
        summary: &MetricsSummary,
        target_rps: f64,
        smoothed_rps: f64,
    ) -> SaturationInfo {
        // Read timer stats and compute per-tick deltas
        let current_dispatched = self.timer_handle.stats.dispatched.load(Ordering::Relaxed);
        let current_dropped = self.timer_handle.stats.dropped.load(Ordering::Relaxed);

        let tick_dispatched = current_dispatched - self.last_timer_dispatched;
        let tick_dropped = current_dropped - self.last_timer_dropped;
        self.last_timer_dispatched = current_dispatched;
        self.last_timer_dropped = current_dropped;

        let total_attempted = tick_dispatched + tick_dropped;
        let backpressure_ratio = if total_attempted > 0 {
            tick_dropped as f64 / total_attempted as f64
        } else {
            0.0
        };

        // Scheduling delay from the tick aggregate
        let delay_sum = self.tick_aggregate.scheduling_delay_sum_ns();
        let delay_max = self.tick_aggregate.scheduling_delay_max_ns();
        let delay_over_1ms = self.tick_aggregate.scheduling_delay_count_over_1ms();

        let scheduling_delay_mean_ms = if summary.total_requests > 0 {
            (delay_sum as f64 / summary.total_requests as f64) / 1_000_000.0
        } else {
            0.0
        };
        let scheduling_delay_max_ms = delay_max as f64 / 1_000_000.0;
        let delayed_request_ratio = if summary.total_requests > 0 {
            delay_over_1ms as f64 / summary.total_requests as f64
        } else {
            0.0
        };

        let rate_achievement = if target_rps > 0.0 {
            (smoothed_rps / target_rps).min(2.0)
        } else {
            1.0
        };

        // Only flag rate underachievement when there's data in the window.
        // An empty window (e.g. startup) means "no data yet", not "can't keep up".
        let rate_underachieving = summary.total_requests > 0 && rate_achievement < 0.90;

        let client_signals = backpressure_ratio > 0.01
            || scheduling_delay_mean_ms > 5.0
            || delayed_request_ratio > 0.10
            || rate_underachieving;

        let server_signals = summary.error_rate > 0.05 || (summary.latency_p99_ns > 5_000_000_000); // p99 > 5s

        let assessment = match (client_signals, server_signals) {
            (false, false) => SaturationAssessment::Healthy,
            (true, false) => SaturationAssessment::ClientSaturated,
            (false, true) => SaturationAssessment::ServerSaturated,
            (true, true) => SaturationAssessment::BothSaturated,
        };

        if assessment != self.last_assessment {
            let latency_p99_ms = summary.latency_p99_ns as f64 / 1_000_000.0;
            if assessment == SaturationAssessment::Healthy {
                tracing::info!(
                    previous = ?self.last_assessment,
                    rate_achievement,
                    "saturation recovered"
                );
            } else {
                tracing::warn!(
                    ?assessment,
                    previous = ?self.last_assessment,
                    backpressure_ratio,
                    scheduling_delay_mean_ms,
                    rate_achievement,
                    error_rate = summary.error_rate,
                    latency_p99_ms,
                    "saturation detected"
                );
            }
            self.last_assessment = assessment;
        }

        SaturationInfo {
            backpressure_drops: tick_dropped,
            backpressure_ratio,
            scheduling_delay_mean_ms,
            scheduling_delay_max_ms,
            delayed_request_ratio,
            rate_achievement,
            cpu_affinity_ratio: self.tick_aggregate.cpu_affinity_ratio(),
            tcp_rtt_mean_ms: self.tick_aggregate.tcp_rtt_mean_us() / 1000.0,
            tcp_rtt_max_ms: self.tick_aggregate.tcp_rtt_max_us() as f64 / 1000.0,
            tcp_retransmit_ratio: self.tick_aggregate.tcp_retransmit_ratio(),
            assessment,
            in_flight_drops: self.tick_aggregate.in_flight_drops(),
            in_flight_count: self.tick_aggregate.in_flight_count(),
            in_flight_capacity: self.tick_aggregate.in_flight_capacity(),
        }
    }

    fn is_test_complete(&self) -> bool {
        if self.stopped {
            return true;
        }
        if self.start_time.elapsed() >= self.test_duration {
            return true;
        }
        if let Some(max) = self.max_requests {
            if self.total_aggregate.total_requests() >= max {
                return true;
            }
        }
        if let Some(threshold) = self.autostop_threshold {
            if self.total_aggregate.total_errors() >= threshold {
                tracing::warn!(
                    errors = self.total_aggregate.total_errors(),
                    threshold,
                    "autostop: error threshold exceeded"
                );
                return true;
            }
        }
        if let Some(threshold) = self.refusestop_threshold {
            if self.total_aggregate.total_errors() >= threshold {
                tracing::warn!(
                    errors = self.total_aggregate.total_errors(),
                    threshold,
                    "refusestop: refused connection threshold exceeded"
                );
                return true;
            }
        }
        if let Some(target) = self.target_bytes {
            if self.total_aggregate.bytes_received() >= target {
                tracing::info!(
                    bytes = self.total_aggregate.bytes_received(),
                    target,
                    "target bytes reached"
                );
                return true;
            }
        }
        false
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

        // Final saturation assessment from total timer stats + aggregate delay
        let total_dispatched = self.timer_handle.stats.dispatched.load(Ordering::Relaxed);
        let total_dropped = self.timer_handle.stats.dropped.load(Ordering::Relaxed);
        let total_attempted = total_dispatched + total_dropped;
        let total_requests = self.total_aggregate.total_requests();

        let saturation = SaturationInfo {
            backpressure_drops: total_dropped,
            backpressure_ratio: if total_attempted > 0 {
                total_dropped as f64 / total_attempted as f64
            } else {
                0.0
            },
            scheduling_delay_mean_ms: if total_requests > 0 {
                (self.total_aggregate.scheduling_delay_sum_ns() as f64 / total_requests as f64)
                    / 1_000_000.0
            } else {
                0.0
            },
            scheduling_delay_max_ms: self.total_aggregate.scheduling_delay_max_ns() as f64
                / 1_000_000.0,
            delayed_request_ratio: if total_requests > 0 {
                self.total_aggregate.scheduling_delay_count_over_1ms() as f64
                    / total_requests as f64
            } else {
                0.0
            },
            cpu_affinity_ratio: self.total_aggregate.cpu_affinity_ratio(),
            tcp_rtt_mean_ms: self.total_aggregate.tcp_rtt_mean_us() / 1000.0,
            tcp_rtt_max_ms: self.total_aggregate.tcp_rtt_max_us() as f64 / 1000.0,
            tcp_retransmit_ratio: self.total_aggregate.tcp_retransmit_ratio(),
            rate_achievement: 1.0, // not meaningful for final result
            assessment: if total_dropped > 0
                || self.total_aggregate.scheduling_delay_count_over_1ms() as f64
                    / total_requests.max(1) as f64
                    > 0.10
            {
                SaturationAssessment::ClientSaturated
            } else {
                SaturationAssessment::Healthy
            },
            in_flight_drops: 0,
            in_flight_count: 0,
            in_flight_capacity: 0,
        };

        let bytes_sent = self.total_aggregate.bytes_sent();
        let bytes_received = self.total_aggregate.bytes_received();
        let secs = elapsed.as_secs_f64();

        TestResult {
            total_requests,
            total_errors: self.total_aggregate.total_errors(),
            duration: elapsed,
            latency_p50: Duration::from_nanos(hist.value_at_quantile(0.50)),
            latency_p90: Duration::from_nanos(hist.value_at_quantile(0.90)),
            latency_p99: Duration::from_nanos(hist.value_at_quantile(0.99)),
            latency_max: Duration::from_nanos(hist.max()),
            request_rate: total_requests as f64 / secs,
            error_rate: if total_requests > 0 {
                self.total_aggregate.total_errors() as f64 / total_requests as f64
            } else {
                0.0
            },
            total_bytes_sent: bytes_sent,
            total_bytes_received: bytes_received,
            throughput_send_mbps: bytes_sent as f64 * 8.0 / secs / 1_000_000.0,
            throughput_recv_mbps: bytes_received as f64 * 8.0 / secs / 1_000_000.0,
            saturation,
        }
    }
}
