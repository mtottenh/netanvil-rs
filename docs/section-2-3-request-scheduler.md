# 2.2 High-Precision Request Scheduler

## 2.2.1 Overview and Responsibilities

The Request Scheduler is a critical component in the load testing pipeline, positioned between the Client Session Manager and Request Pipeline. Its singular responsibility is to schedule and dispatch requests with the highest possible timing precision, ensuring accurate load generation patterns.

### Core Responsibilities

1. **High-Precision Timing**: Dispatch tickets at exactly their scheduled times using advanced timing techniques
2. **Coordinated Omission Prevention**: Maintain schedule integrity regardless of system performance
3. **Unbiased Sampling**: Apply Bernoulli sampling for statistically valid request selection
4. **Backpressure Detection**: Measure and report when the system cannot keep up with the schedule
5. **Minimal Overhead**: Operate with minimal latency and jitter on the critical path

### Architectural Positioning

```
┌────────────────┐        ┌───────────────────┐        ┌──────────────────┐        ┌────────────────┐
│ Rate           │        │ Client Session     │        │ Request          │        │ Request        │
│ Controller     │───────>│ Manager           │───────>│ Scheduler        │───────>│ Pipeline       │
└────────────────┘signals └───────────────────┘tickets └──────────────────┘dispatch└────────────────┘
```

The Request Scheduler accepts tickets with scheduled timestamps from the Client Session Manager, and dispatches them at precisely the right time to the Request Pipeline, ensuring accurate load patterns regardless of the performance of other components.

## 2.2.2 Core Interfaces

### Request Scheduler Interface

```rust
/// Interface for high-precision request scheduling
pub trait RequestScheduler<M>: Send + Sync
where
    M: MetricsProvider,
{
    /// Run the scheduler until the test is complete
    async fn run(
        &self,
        config: Arc<LoadTestConfig>,
        ticket_rx: async_channel::Receiver<SchedulerTicket>,
        dispatch_tx: async_channel::Sender<SchedulerTicket>,
    );

    /// Set the sampling rate (0.0-1.0)
    fn set_sampling_rate(&self, rate: f64);

    /// Get the current sampling rate
    fn get_sampling_rate(&self) -> f64;

    /// Signal the scheduler to terminate early
    fn terminate(&self);

    /// Get scheduler type
    fn get_scheduler_type(&self) -> SchedulerType;

    /// Get metrics provider
    fn metrics(&self) -> &M;
}

/// Types of request schedulers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerType {
    /// High performance scheduler with adaptive timing
    HighPrecision,
    /// Adaptive scheduler that balances precision with CPU usage
    Adaptive,
    /// Simple scheduler for less demanding scenarios
    Standard,
    /// Custom scheduler type
    Custom(String),
}
```

### Scheduler Ticket

The scheduler receives tickets from the Client Session Manager and applies scheduling and sampling decisions:

```rust
/// A ticket representing a scheduled request
#[derive(Debug, Clone)]
pub struct SchedulerTicket {
    /// Unique identifier for this request
    pub id: u64,

    /// When this request SHOULD be sent according to the schedule
    pub scheduled_time: Instant,

    /// Whether this is a warmup request (not counted in stats)
    pub is_warmup: bool,

    /// Whether this request should be sampled for detailed analysis
    /// Decision made by scheduler using Bernoulli sampling
    pub should_sample: bool,

    /// Client session ID that generated this ticket
    pub client_session_id: Option<String>,

    /// When this ticket was actually dispatched (filled by scheduler)
    pub dispatch_time: Option<Instant>,

    /// Any scheduling delay (scheduled vs actual) in nanoseconds
    pub scheduling_delay_ns: Option<u64>,
}
```

### Scheduler Metrics

```rust
/// Lock-free metrics collection for scheduler
pub struct SchedulerMetrics {
    /// Component ID for this scheduler
    id: String,

    /// Total number of requests scheduled
    scheduled_count: AtomicU64,

    /// Number of requests scheduled during warmup phase
    warmup_count: AtomicU64,

    /// Number of requests that experienced scheduling delay over threshold
    delayed_count: AtomicU64,

    /// Sum of scheduling delays (ns) for calculating averages
    delay_sum_ns: AtomicU64,

    /// Maximum observed scheduling delay (ns)
    max_delay_ns: AtomicU64,

    /// Number of requests marked for sampling
    sampled_count: AtomicU64,

    /// Current sampling rate (0.0-1.0 stored as percentage * 1000)
    sampling_rate: AtomicU32,

    /// Timer jitter statistics
    timer_jitter_count: AtomicU64,
    timer_jitter_sum_ns: AtomicU64,
    timer_jitter_max_ns: AtomicU64,
    timer_threshold_exceeded_count: AtomicU64,

    /// Last metrics update timestamp
    last_update: AtomicU64,
}
```

## 2.2.3 High-Precision Timer Implementation

The core of the scheduler's timing capabilities:

```rust
/// High-precision timer with jitter awareness
pub struct PrecisionTimer {
    /// Minimum tick resolution
    tick_resolution: Duration,

    /// Jitter threshold before compensation
    jitter_threshold: Duration,

    /// Thread priority set flag
    priority_set: AtomicBool,

    /// Start time
    start_time: Instant,

    /// Metrics for recording jitter
    metrics: Arc<SchedulerMetrics>,
}

impl PrecisionTimer {
    /// Create a new precision timer
    pub fn new(
        tick_resolution: Duration,
        jitter_threshold: Duration,
        metrics: Arc<SchedulerMetrics>
    ) -> Self {
        Self {
            tick_resolution,
            jitter_threshold,
            priority_set: AtomicBool::new(false),
            start_time: Instant::now(),
            metrics,
        }
    }

    /// Sleep until the specified deadline with jitter compensation
    pub async fn sleep_until(&self, deadline: Instant) {
        let now = Instant::now();

        if now >= deadline {
            // Already past deadline
            let jitter_ns = now.duration_since(deadline).as_nanos() as u64;
            let threshold_exceeded = jitter_ns > self.jitter_threshold.as_nanos() as u64;
            self.metrics.record_timer_jitter(jitter_ns, threshold_exceeded);
            return;
        }

        let duration = deadline.duration_since(now);

        // Set thread priority if not already set
        if !self.priority_set.load(Ordering::Relaxed) {
            self.try_set_thread_priority();
            self.priority_set.store(true, Ordering::Relaxed);
        }

        // Use adaptive sleep strategy based on remaining time
        if duration > self.tick_resolution * 2 {
            // For longer durations, sleep most of the time
            let sleep_duration = duration - self.tick_resolution;
            tokio::time::sleep(sleep_duration).await;
        }

        // Adaptive sleep or spin based on precision requirements
        self.adaptive_wait_until(deadline);

        // Record any jitter that occurred
        let end_time = Instant::now();
        if end_time > deadline {
            let jitter_ns = end_time.duration_since(deadline).as_nanos() as u64;
            let threshold_exceeded = jitter_ns > self.jitter_threshold.as_nanos() as u64;
            self.metrics.record_timer_jitter(jitter_ns, threshold_exceeded);
        }
    }

    /// Adaptive wait strategy combining sleep and spin
    fn adaptive_wait_until(&self, deadline: Instant) {
        const SPIN_THRESHOLD_NS: u64 = 50_000; // 50μs threshold for spinning
        const YIELD_THRESHOLD_NS: u64 = 5_000; // 5μs threshold for yielding

        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }

            let remaining_ns = deadline.duration_since(now).as_nanos() as u64;

            if remaining_ns > SPIN_THRESHOLD_NS {
                // Sleep a bit with exponential backoff
                let sleep_ns = remaining_ns / 2;
                std::thread::sleep(Duration::from_nanos(sleep_ns));
            } else if remaining_ns > YIELD_THRESHOLD_NS {
                // Yield to other threads but stay on CPU
                std::thread::yield_now();
            } else {
                // Busy-wait for the final stretch
                std::hint::spin_loop();
            }
        }
    }

    /// Try to set the current thread to high priority
    fn try_set_thread_priority(&self) {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::thread::JoinHandleExt;
            use std::thread;

            // Best effort to set thread priority, ignore errors
            let _ = unsafe {
                let thread_id = libc::pthread_self();
                let policy = libc::SCHED_FIFO;
                let mut param: libc::sched_param = std::mem::zeroed();

                // Get the priority range
                let max_priority = libc::sched_get_priority_max(policy);
                if max_priority != -1 {
                    param.sched_priority = max_priority;
                    libc::pthread_setschedparam(thread_id, policy, &param);
                }
            };
        }

        #[cfg(target_os = "windows")]
        {
            use winapi::um::processthreadsapi::GetCurrentThread;
            use winapi::um::processthreadsapi::SetThreadPriority;
            use winapi::um::winbase::THREAD_PRIORITY_HIGHEST;

            unsafe {
                let handle = GetCurrentThread();
                SetThreadPriority(handle, THREAD_PRIORITY_HIGHEST);
            }
        }
    }
}
```

## 2.2.4 High-Precision Scheduler Implementation

```rust
/// High-precision request scheduler implementation
pub struct HighPrecisionScheduler<M>
where
    M: MetricsProvider,
{
    /// Scheduler ID
    id: String,

    /// Scheduler configuration
    config: Arc<SchedulerConfig>,

    /// Precision timer for accurate scheduling
    timer: Arc<PrecisionTimer>,

    /// Current sampling rate (0.0-1.0)
    sampling_rate: Arc<AtomicF64>,

    /// RNG for sampling decisions
    rng: Arc<Mutex<SmallRng>>,

    /// Whether scheduler is active
    active: Arc<AtomicBool>,

    /// Metrics provider
    metrics: M,
}

impl<M> HighPrecisionScheduler<M>
where
    M: MetricsProvider,
{
    /// Create a new high-precision scheduler
    pub fn new(
        id: String,
        config: SchedulerConfig,
        metrics: M,
    ) -> Self {
        let scheduler_metrics = match metrics.get_component_metrics() {
            Some(m) => m,
            None => Arc::new(SchedulerMetrics::new(id.clone())),
        };

        Self {
            id,
            config: Arc::new(config),
            timer: Arc::new(PrecisionTimer::new(
                Duration::from_micros(config.tick_resolution_us),
                Duration::from_nanos(config.jitter_threshold_ns),
                scheduler_metrics,
            )),
            sampling_rate: Arc::new(AtomicF64::new(config.sampling_rate)),
            rng: Arc::new(Mutex::new(SmallRng::from_entropy())),
            active: Arc::new(AtomicBool::new(false)),
            metrics,
        }
    }

    /// Make a sampling decision using Bernoulli sampling
    fn make_sampling_decision(&self) -> bool {
        let rate = self.sampling_rate.load(Ordering::Relaxed);
        let mut rng = self.rng.lock().unwrap();

        // Simple Bernoulli trial with probability equal to the target sampling rate
        rng.gen::<f64>() < rate
    }
}

impl<M> RequestScheduler<M> for HighPrecisionScheduler<M>
where
    M: MetricsProvider,
{
    async fn run(
        &self,
        config: Arc<LoadTestConfig>,
        ticket_rx: async_channel::Receiver<SchedulerTicket>,
        dispatch_tx: async_channel::Sender<SchedulerTicket>,
    ) {
        // Set active flag
        self.active.store(true, Ordering::SeqCst);

        // Track warmup phase
        let start_time = Instant::now();
        let warmup_end = start_time + Duration::from_secs(config.warmup_duration_secs);
        let mut warmup_complete = false;

        // Create queue for upcoming tickets
        let mut upcoming_tickets = BinaryHeap::new();

        // Track last dispatch time for detecting backpressure
        let mut last_dispatch = Instant::now();

        // Main scheduling loop
        while self.active.load(Ordering::SeqCst) {
            // Check if warmup phase is complete
            if !warmup_complete && Instant::now() >= warmup_end {
                warmup_complete = true;
            }

            // Receive tickets from upstream (non-blocking)
            while let Ok(ticket) = ticket_rx.try_recv() {
                // Add to priority queue, ordered by scheduled time
                upcoming_tickets.push(ScheduledTicket {
                    ticket,
                    priority: std::cmp::Reverse(ticket.scheduled_time),
                });
            }

            // Process tickets that are ready to be dispatched
            let now = Instant::now();

            // Check next ticket
            if let Some(next_ticket) = upcoming_tickets.peek() {
                if next_ticket.ticket.scheduled_time <= now {
                    // Ready to dispatch
                    if let Some(mut scheduled_ticket) = upcoming_tickets.pop() {
                        // Make sampling decision
                        let should_sample = if !warmup_complete {
                            false // Don't sample during warmup
                        } else {
                            self.make_sampling_decision()
                        };

                        // Record metrics
                        if scheduled_ticket.ticket.is_warmup {
                            self.metrics.record_warmup_request();
                        } else {
                            self.metrics.record_scheduled_request();
                        }

                        if should_sample {
                            self.metrics.record_sampled_request();
                        }

                        // Update ticket with dispatch info
                        scheduled_ticket.ticket.should_sample = should_sample;
                        scheduled_ticket.ticket.dispatch_time = Some(now);

                        // Calculate scheduling delay
                        let delay_ns = now.duration_since(scheduled_ticket.ticket.scheduled_time)
                            .as_nanos() as u64;
                        scheduled_ticket.ticket.scheduling_delay_ns = Some(delay_ns);

                        // Record delay if significant
                        if delay_ns > self.config.delay_threshold_ns {
                            self.metrics.record_delay(delay_ns);
                        }

                        // Send to pipeline
                        if let Err(_) = dispatch_tx.try_send(scheduled_ticket.ticket) {
                            // Channel closed or full - detect backpressure
                            let backpressure_time = now.duration_since(last_dispatch);
                            if backpressure_time > Duration::from_millis(self.config.backpressure_threshold_ms) {
                                self.metrics.record_backpressure(backpressure_time.as_millis() as u64);
                            }
                        } else {
                            // Update last dispatch time
                            last_dispatch = now;
                        }
                    }
                    continue; // Check for more tickets ready to dispatch
                }

                // Next ticket isn't ready yet, sleep until it is
                let sleep_until = next_ticket.ticket.scheduled_time;
                self.timer.sleep_until(sleep_until).await;
            } else {
                // No tickets in queue, wait for more from upstream
                match ticket_rx.recv().await {
                    Ok(ticket) => {
                        // Add received ticket to queue
                        upcoming_tickets.push(ScheduledTicket {
                            ticket,
                            priority: std::cmp::Reverse(ticket.scheduled_time),
                        });
                    },
                    Err(_) => {
                        // Channel closed, terminate
                        break;
                    }
                }
            }
        }
    }

    fn set_sampling_rate(&self, rate: f64) {
        let clamped_rate = rate.clamp(0.0, 1.0);
        self.sampling_rate.store(clamped_rate, Ordering::Relaxed);
        self.metrics.update_sampling_rate(clamped_rate);
    }

    fn get_sampling_rate(&self) -> f64 {
        self.sampling_rate.load(Ordering::Relaxed)
    }

    fn terminate(&self) {
        self.active.store(false, Ordering::SeqCst);
    }

    fn get_scheduler_type(&self) -> SchedulerType {
        SchedulerType::HighPrecision
    }

    fn metrics(&self) -> &M {
        &self.metrics
    }
}

/// Priority queue wrapper for scheduled tickets
#[derive(Debug)]
struct ScheduledTicket {
    /// The ticket to be scheduled
    ticket: SchedulerTicket,
    /// Priority for the binary heap (Reverse for min-heap behavior)
    priority: std::cmp::Reverse<Instant>,
}

impl PartialEq for ScheduledTicket {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
    }
}

impl Eq for ScheduledTicket {}

impl PartialOrd for ScheduledTicket {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.priority.partial_cmp(&other.priority)
    }
}

impl Ord for ScheduledTicket {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.priority.cmp(&other.priority)
    }
}
```

## 2.2.5 Scheduler Configuration

```rust
/// Configuration for request scheduler
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Timer tick resolution in microseconds
    pub tick_resolution_us: u64,

    /// Jitter threshold in nanoseconds
    pub jitter_threshold_ns: u64,

    /// Sampling rate (0.0-1.0)
    pub sampling_rate: f64,

    /// Delay threshold for reporting in nanoseconds
    pub delay_threshold_ns: u64,

    /// Backpressure detection threshold in milliseconds
    pub backpressure_threshold_ms: u64,

    /// Core ID to pin the scheduler to (if specified)
    pub core_id: Option<usize>,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            tick_resolution_us: 100,    // 100µs resolution
            jitter_threshold_ns: 1_000, // 1µs jitter threshold
            sampling_rate: 0.01,        // 1% sampling rate
            delay_threshold_ns: 1_000_000, // 1ms delay threshold
            backpressure_threshold_ms: 50, // 50ms backpressure threshold
            core_id: None,
        }
    }
}
```

## 2.2.6 Running on a Dedicated Core

One of the key features is the ability to run the scheduler on a dedicated CPU core:

```rust
/// Run a scheduler on a dedicated CPU core
pub fn run_scheduler_on_dedicated_core<M: MetricsProvider>(
    scheduler: Arc<dyn RequestScheduler<M>>,
    config: Arc<LoadTestConfig>,
    ticket_rx: async_channel::Receiver<SchedulerTicket>,
    dispatch_tx: async_channel::Sender<SchedulerTicket>,
    core_id: usize,
) -> std::thread::JoinHandle<()> {
    // Create thread for dedicated core
    std::thread::spawn(move || {
        // Set core affinity for this thread
        #[cfg(target_os = "linux")]
        {
            let cores = core_affinity::get_core_ids().unwrap();
            if core_id < cores.len() {
                core_affinity::set_for_current(cores[core_id]);
            }
        }

        #[cfg(target_os = "windows")]
        {
            // On Windows, use SetThreadAffinityMask
            use winapi::um::winbase::SetThreadAffinityMask;
            use winapi::um::processthreadsapi::GetCurrentThread;

            unsafe {
                let mask = 1u64 << core_id;
                SetThreadAffinityMask(GetCurrentThread(), mask);
            }
        }

        // Set thread priority to maximum (platform-specific)
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::thread::JoinHandleExt;
            let native_handle = std::thread::current().id();
            let pid = nix::unistd::Pid::from_raw(native_handle.as_u64() as i32);
            let param = nix::sched::SchedParam::new(
                nix::sched::sched_get_priority_max(nix::sched::SCHED_FIFO).unwrap()
            );
            let _ = nix::sched::sched_setscheduler(pid, nix::sched::SCHED_FIFO, &param);
        }

        #[cfg(target_os = "windows")]
        {
            use winapi::um::processthreadsapi::{GetCurrentThread, SetThreadPriority};
            use winapi::um::winbase::THREAD_PRIORITY_HIGHEST;
            unsafe {
                let handle = GetCurrentThread();
                SetThreadPriority(handle, THREAD_PRIORITY_HIGHEST);
            }
        }

        // Create tokio runtime for this thread only
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        // Run scheduler on this runtime
        rt.block_on(async {
            scheduler.run(
                config,
                ticket_rx,
                dispatch_tx,
            ).await;
        });
    })
}
```

## 2.2.7 Factory and Builder Pattern Integration

```rust
/// Factory for creating schedulers
pub struct SchedulerFactory;

impl SchedulerFactory {
    /// Create a scheduler based on configuration
    pub fn create<M: MetricsProvider>(
        scheduler_type: SchedulerType,
        id: String,
        config: SchedulerConfig,
        metrics: M,
    ) -> Arc<dyn RequestScheduler<M>> {
        match scheduler_type {
            SchedulerType::HighPrecision => {
                Arc::new(HighPrecisionScheduler::new(id, config, metrics))
            },
            SchedulerType::Adaptive => {
                Arc::new(AdaptiveScheduler::new(id, config, metrics))
            },
            SchedulerType::Standard => {
                Arc::new(StandardScheduler::new(id, config, metrics))
            },
            SchedulerType::Custom(_) => {
                // Default to high precision
                Arc::new(HighPrecisionScheduler::new(id, config, metrics))
            },
        }
    }
}

impl LoadTestBuilder {
    /// Use a high-precision scheduler
    pub fn with_high_precision_scheduler(mut self) -> Self {
        self.scheduler_type = SchedulerType::HighPrecision;
        self
    }

    /// Use an adaptive scheduler
    pub fn with_adaptive_scheduler(mut self) -> Self {
        self.scheduler_type = SchedulerType::Adaptive;
        self
    }

    /// Set scheduler timing precision
    pub fn with_scheduler_timing_precision(
        mut self,
        tick_resolution_us: u64,
        jitter_threshold_ns: u64,
    ) -> Self {
        self.scheduler_config.tick_resolution_us = tick_resolution_us;
        self.scheduler_config.jitter_threshold_ns = jitter_threshold_ns;
        self
    }

    /// Set sampling rate
    pub fn with_sampling_rate(mut self, rate: f64) -> Self {
        self.scheduler_config.sampling_rate = rate;
        self
    }

    /// Pin scheduler to a specific core
    pub fn with_scheduler_core_pinning(mut self, core_id: usize) -> Self {
        self.scheduler_config.core_id = Some(core_id);
        self
    }

    /// Build the scheduler component
    fn build_scheduler<M: MetricsProvider>(&self, metrics: M) -> Arc<dyn RequestScheduler<M>> {
        // Generate a unique ID for this scheduler
        let id = format!("scheduler-{}", Uuid::new_v4());

        // Create scheduler based on type
        SchedulerFactory::create(
            self.scheduler_type,
            id,
            self.scheduler_config.clone(),
            metrics,
        )
    }
}
```

## 2.2.8 Coordinated Omission Prevention

A critical aspect of the scheduler is preventing coordinated omission - where delays in test execution artificially reduce the load on the system:

```rust
impl<M> HighPrecisionScheduler<M>
where
    M: MetricsProvider,
{
    /// Record timing data to detect coordinated omission
    fn record_timing_data(&self, ticket: &SchedulerTicket) {
        if let (Some(dispatch_time), Some(delay_ns)) = (ticket.dispatch_time, ticket.scheduling_delay_ns) {
            // Record both the scheduled and actual times
            // This shows any divergence that could indicate coordinated omission

            // Record the delay itself
            if delay_ns > self.config.delay_threshold_ns {
                self.metrics.record_delay(delay_ns);

                // Log significant delays for analysis
                if delay_ns > self.config.delay_threshold_ns * 10 {
                    // This would be logged to a specialized coordinated omission detection system
                    // For now we just record it in metrics
                    self.metrics.record_significant_delay(delay_ns);
                }
            }

            // The key principle: we always maintain the original schedule
            // Even if dispatching is delayed, we don't reschedule subsequent tickets
            // This ensures the test accurately measures service degradation
        }
    }
}
```

## 2.2.9 Statistical Validity of Bernoulli Sampling

Maintaining statistical validity through unbiased sampling is important for accurate test results:

```rust
impl<M> HighPrecisionScheduler<M>
where
    M: MetricsProvider,
{
    /// Validate sampling statistics periodically
    fn validate_sampling_statistics(&self) {
        // Get current metrics
        let total_requests = self.metrics.get_scheduled_count();
        let sampled_requests = self.metrics.get_sampled_count();
        let target_rate = self.sampling_rate.load(Ordering::Relaxed);

        if total_requests > 1000 {
            // We have enough data for statistical validation
            let actual_rate = sampled_requests as f64 / total_requests as f64;

            // Calculate expected standard deviation for Bernoulli sampling
            // For a Bernoulli process, std_dev = sqrt(p * (1-p) / n)
            let expected_std_dev = f64::sqrt(target_rate * (1.0 - target_rate) / total_requests as f64);

            // Check if actual rate is within 3 standard deviations of target
            // (99.7% of values should fall within this range)
            let deviation = f64::abs(actual_rate - target_rate);

            if deviation > 3.0 * expected_std_dev {
                // Sampling may be biased - log warning
                log::warn!(
                    "Sampling rate deviation exceeds 3σ: target={:.4}, actual={:.4}, std_dev={:.6}",
                    target_rate,
                    actual_rate,
                    expected_std_dev
                );
            }
        }
    }
}
```

## 2.2.10 Performance Optimization Techniques

The scheduler employs several techniques to minimize jitter and maximize timing precision:

1. **Adaptive Sleep Strategy**:
   - For long waits: Standard sleep calls
   - For medium waits: Sleep with progressive backoff
   - For short waits: Busy-wait with spin loops

2. **Thread Priority Elevation**:
   - Raises scheduler thread to highest priority
   - Minimizes preemption by other processes

3. **CPU Core Pinning**:
   - Pins scheduler to dedicated core
   - Reduces context switching and cache pollution

4. **Minimal Allocation**:
   - Uses pre-allocated structures
   - Avoids garbage collection and memory pressure

5. **Lock-Free Metrics**:
   - Records metrics without blocking
   - Uses atomic operations for counters

6. **Scheduling Feedback**:
   - Measures actual vs. intended timing
   - Adjusts strategy based on observed jitter

These techniques together enable microsecond-level precision for request scheduling, ensuring accurate load testing results.

## 2.2.11 Conclusion

The High-Precision Request Scheduler fulfills its single responsibility of dispatching requests at precisely the right time, addressing several critical aspects of accurate load testing:

1. It prevents coordinated omission by maintaining the original schedule regardless of system performance
2. It provides statistical validity through unbiased Bernoulli sampling
3. It minimizes overhead and jitter through advanced timing techniques
4. It can operate on a dedicated core for maximum precision
5. It integrates seamlessly with upstream and downstream components

By focusing solely on this responsibility, the scheduler achieves maximum performance and precision while working harmoniously within the overall load testing architecture.
