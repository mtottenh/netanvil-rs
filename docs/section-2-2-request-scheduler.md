# 2. Core Trait Definitions

## 2.2 Request Scheduler with Bernoulli Sampling

The scheduler component determines when requests should be issued to maintain specified arrival patterns and prevent coordinated omission. Additionally, it now makes unbiased sampling decisions using Bernoulli sampling (uniform random sampling) before request execution, ensuring statistical validity regardless of system performance.

### 2.2.1 Scheduler Metrics with Sampling Statistics

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
    
    /// Histogram for inter-arrival times (μs)
    /// Protected by RwLock but updated selectively to reduce contention
    inter_arrival_histogram: Arc<RwLock<Histogram<u64>>>,
    
    /// Histogram for scheduling delays (ns)
    /// Protected by RwLock but updated selectively to reduce contention
    delay_histogram: Arc<RwLock<Histogram<u64>>>,
    
    /// Timer jitter statistics
    timer_jitter_count: AtomicU64,
    timer_jitter_sum_ns: AtomicU64,
    timer_jitter_max_ns: AtomicU64,
    timer_threshold_exceeded_count: AtomicU64,
    
    /// Last metrics update timestamp
    last_update: AtomicU64,
}

impl SchedulerMetrics {
    /// Create new scheduler metrics
    pub fn new(id: String) -> Self {
        Self {
            id,
            scheduled_count: AtomicU64::new(0),
            warmup_count: AtomicU64::new(0),
            delayed_count: AtomicU64::new(0),
            delay_sum_ns: AtomicU64::new(0),
            max_delay_ns: AtomicU64::new(0),
            sampled_count: AtomicU64::new(0),
            sampling_rate: AtomicU32::new(0), // Default 0%
            inter_arrival_histogram: Arc::new(RwLock::new(Histogram::<u64>::new(3).unwrap())),
            delay_histogram: Arc::new(RwLock::new(Histogram::<u64>::new(3).unwrap())),
            timer_jitter_count: AtomicU64::new(0),
            timer_jitter_sum_ns: AtomicU64::new(0),
            timer_jitter_max_ns: AtomicU64::new(0),
            timer_threshold_exceeded_count: AtomicU64::new(0),
            last_update: AtomicU64::new(0),
        }
    }
    
    /// Record a scheduled request
    pub fn record_request(&self, is_warmup: bool, is_sampled: bool) {
        self.scheduled_count.fetch_add(1, Ordering::Relaxed);
        
        if is_warmup {
            self.warmup_count.fetch_add(1, Ordering::Relaxed);
        }
        
        if is_sampled {
            self.sampled_count.fetch_add(1, Ordering::Relaxed);
        }
        
        self.update_timestamp();
    }
    
    /// Set the sampling rate
    pub fn set_sampling_rate(&self, rate: f64) {
        // Store as integer percentage * 1000 for atomic storage (0-100000)
        let rate_int = (rate * 100000.0) as u32;
        self.sampling_rate.store(rate_int, Ordering::Relaxed);
    }
    
    /// Get the current sampling rate
    pub fn get_sampling_rate(&self) -> f64 {
        let rate_int = self.sampling_rate.load(Ordering::Relaxed);
        rate_int as f64 / 100000.0
    }
    
    /// Record an inter-arrival interval
    pub fn record_interval(&self, interval_ns: u64) {
        let interval_us = interval_ns / 1000;
        
        // Only update histogram occasionally to reduce lock contention
        // Use modulo on atomic counter for deterministic sampling
        if self.scheduled_count.load(Ordering::Relaxed) % 100 == 0 {
            if let Ok(mut hist) = self.inter_arrival_histogram.try_write() {
                let _ = hist.record(interval_us);
            }
        }
        
        self.update_timestamp();
    }
    
    /// Record a scheduling delay
    pub fn record_delay(&self, delay_ns: u64) {
        if delay_ns > 0 {
            self.delayed_count.fetch_add(1, Ordering::Relaxed);
            self.delay_sum_ns.fetch_add(delay_ns, Ordering::Relaxed);
            
            // Update max delay using compare-exchange loop
            let mut current_max = self.max_delay_ns.load(Ordering::Relaxed);
            while delay_ns > current_max {
                match self.max_delay_ns.compare_exchange(
                    current_max, delay_ns, Ordering::Relaxed, Ordering::Relaxed
                ) {
                    Ok(_) => break,
                    Err(new_current) => current_max = new_current,
                }
            }
            
            // Only update histogram occasionally to reduce lock contention
            if self.delayed_count.load(Ordering::Relaxed) % 50 == 0 {
                if let Ok(mut hist) = self.delay_histogram.try_write() {
                    let _ = hist.record(delay_ns);
                }
            }
        }
        
        self.update_timestamp();
    }
    
    /// Record timer jitter
    pub fn record_timer_jitter(&self, jitter_ns: u64, threshold_exceeded: bool) {
        self.timer_jitter_count.fetch_add(1, Ordering::Relaxed);
        self.timer_jitter_sum_ns.fetch_add(jitter_ns, Ordering::Relaxed);
        
        // Update max jitter using compare-exchange loop
        let mut current_max = self.timer_jitter_max_ns.load(Ordering::Relaxed);
        while jitter_ns > current_max {
            match self.timer_jitter_max_ns.compare_exchange(
                current_max, jitter_ns, Ordering::Relaxed, Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(new_current) => current_max = new_current,
            }
        }
        
        if threshold_exceeded {
            self.timer_threshold_exceeded_count.fetch_add(1, Ordering::Relaxed);
        }
        
        self.update_timestamp();
    }
    
    /// Update the last timestamp
    fn update_timestamp(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
            
        self.last_update.store(now, Ordering::Relaxed);
    }
    
    /// Get a snapshot of metrics for registry
    pub fn get_snapshot(&self) -> HashMap<String, MetricValue> {
        let mut metrics = HashMap::new();
        
        // Add atomic counter values
        metrics.insert("scheduled_count".to_string(), 
                      MetricValue::Counter(self.scheduled_count.load(Ordering::Relaxed)));
                      
        metrics.insert("warmup_count".to_string(),
                      MetricValue::Counter(self.warmup_count.load(Ordering::Relaxed)));
                      
        metrics.insert("delayed_count".to_string(),
                      MetricValue::Counter(self.delayed_count.load(Ordering::Relaxed)));
        
        // Sampling metrics
        metrics.insert("sampled_count".to_string(),
                      MetricValue::Counter(self.sampled_count.load(Ordering::Relaxed)));
                      
        metrics.insert("sampling_rate".to_string(),
                      MetricValue::Float(self.get_sampling_rate()));
                      
        let total = self.scheduled_count.load(Ordering::Relaxed);
        let sampled = self.sampled_count.load(Ordering::Relaxed);
        let actual_rate = if total > 0 { sampled as f64 / total as f64 } else { 0.0 };
        
        metrics.insert("actual_sampling_rate".to_string(),
                      MetricValue::Float(actual_rate));
        
        // Calculate derived metrics
        let delayed = self.delayed_count.load(Ordering::Relaxed);
        metrics.insert("avg_delay_ns".to_string(),
                      MetricValue::Duration(if delayed > 0 {
                          self.delay_sum_ns.load(Ordering::Relaxed) / delayed
                      } else {
                          0
                      }));
                      
        metrics.insert("max_delay_ns".to_string(),
                      MetricValue::Duration(self.max_delay_ns.load(Ordering::Relaxed)));
        
        // Timer jitter metrics
        let jitter_count = self.timer_jitter_count.load(Ordering::Relaxed);
        metrics.insert("timer_jitter_count".to_string(),
                      MetricValue::Counter(jitter_count));
                      
        metrics.insert("timer_jitter_avg_ns".to_string(),
                      MetricValue::Duration(if jitter_count > 0 {
                          self.timer_jitter_sum_ns.load(Ordering::Relaxed) / jitter_count
                      } else {
                          0
                      }));
                      
        metrics.insert("timer_jitter_max_ns".to_string(),
                      MetricValue::Duration(self.timer_jitter_max_ns.load(Ordering::Relaxed)));
                      
        metrics.insert("timer_threshold_exceeded_count".to_string(),
                      MetricValue::Counter(self.timer_threshold_exceeded_count.load(Ordering::Relaxed)));
        
        // Include histogram percentiles (read-only access)
        if let Ok(hist) = self.inter_arrival_histogram.try_read() {
            metrics.insert("interval_p50_us".to_string(),
                         MetricValue::Duration(hist.value_at_percentile(50.0)));
                         
            metrics.insert("interval_p99_us".to_string(),
                         MetricValue::Duration(hist.value_at_percentile(99.0)));
        }
        
        if let Ok(hist) = self.delay_histogram.try_read() {
            metrics.insert("delay_p50_ns".to_string(),
                         MetricValue::Duration(hist.value_at_percentile(50.0)));
                         
            metrics.insert("delay_p99_ns".to_string(),
                         MetricValue::Duration(hist.value_at_percentile(99.0)));
        }
        
        metrics
    }
}
```

### 2.2.2 Request Scheduler Interface with Sampling

```rust
/// Interface for scheduling requests
pub trait RequestScheduler: MetricsProvider + Send + Sync {
    /// Run the scheduler until the test is complete
    async fn run(
        &self,
        config: Arc<LoadTestConfig>,
        rate_controller: Arc<dyn RateController>,
        ticket_tx: async_channel::Sender<SchedulerTicket>,
        profiler: Option<Arc<dyn TransactionProfiler>>,
    );
    
    /// Set the sampling rate (0.0-1.0)
    fn set_sampling_rate(&self, rate: f64);
    
    /// Get the current sampling rate
    fn get_sampling_rate(&self) -> f64;
    
    /// Signal the scheduler to terminate early
    fn terminate(&self);
    
    /// Get the scheduler type
    fn get_scheduler_type(&self) -> SchedulerType;
}

/// A ticket representing a scheduled request time with sampling decision
#[derive(Debug, Clone)]
pub struct SchedulerTicket {
    /// Unique identifier for this request
    pub id: u64,
    /// When this request SHOULD be sent according to the schedule
    pub scheduled_time: Instant,
    /// Whether this is a warmup request (not counted in stats)
    pub is_warmup: bool,
    /// Whether this request should be sampled for detailed analysis
    /// Decision made at scheduling time for unbiased sampling
    pub should_sample: bool,
}

/// Types of request schedulers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerType {
    /// Generates requests at constant intervals
    ConstantRate,
    /// Generates requests with exponentially distributed inter-arrival times (Poisson process)
    Poisson,
    /// Custom scheduler type
    Custom(u32),
}

/// Sampling configuration
#[derive(Debug, Clone)]
pub struct SamplingConfig {
    /// Target sampling rate (0.0-1.0)
    pub target_rate: f64,
}

impl Default for SamplingConfig {
    fn default() -> Self {
        Self {
            target_rate: 0.01, // 1% default sampling rate
        }
    }
}
```

### 2.2.3 High-Precision Timer with Metrics

```rust
/// High-precision timer with jitter awareness and metrics
pub struct PrecisionTimer {
    /// Minimum tick resolution
    tick_resolution: Duration,
    /// Jitter threshold before compensation
    jitter_threshold: Duration,
    /// Jitter compensation factor
    compensation_factor: f64,
    /// Start time
    start_time: Instant,
    /// Reference to metrics for recording jitter
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
            compensation_factor: 0.9,
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
        
        // For very short durations, just busy wait
        if duration < self.tick_resolution {
            self.busy_wait_until(deadline);
            return;
        }
        
        // Sleep for most of the duration using tokio sleep
        let early_wake = duration - self.tick_resolution;
        tokio::time::sleep(early_wake).await;
        
        // Fine-grained busy waiting for remaining time
        self.busy_wait_until(deadline);
    }
    
    /// Sleep for the specified duration with jitter compensation
    pub async fn sleep(&self, duration: Duration) {
        let deadline = Instant::now() + duration;
        self.sleep_until(deadline).await;
    }
    
    /// Busy wait until the specified deadline
    fn busy_wait_until(&self, deadline: Instant) {
        // Fine-grained busy waiting
        while Instant::now() < deadline {
            // Yield to scheduler occasionally on longer waits
            if deadline.duration_since(Instant::now()) > self.tick_resolution {
                std::hint::spin_loop();
            }
        }
        
        // Record jitter statistics
        let now = Instant::now();
        let jitter_ns = now.duration_since(deadline).as_nanos() as u64;
        let threshold_exceeded = jitter_ns > self.jitter_threshold.as_nanos() as u64;
        self.metrics.record_timer_jitter(jitter_ns, threshold_exceeded);
    }
}
```

### 2.2.4 Poisson Process Scheduler With Bernoulli Sampling

```rust
/// Scheduler that generates requests following a Poisson process
pub struct PoissonScheduler {
    /// Scheduler unique ID
    id: String,
    
    /// Scheduler state
    state: Arc<RwLock<SchedulerState>>,
    
    /// RNG for exponential distribution and sampling decisions
    rng: Arc<Mutex<SmallRng>>,
    
    /// Sampling configuration
    sampling_config: Arc<RwLock<SamplingConfig>>,
    
    /// Precision timer for accurate scheduling
    timer: Arc<PrecisionTimer>,
    
    /// Metrics collection
    metrics: Arc<SchedulerMetrics>,
    
    /// Whether scheduler is active
    active: Arc<AtomicBool>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

/// Scheduler state
#[derive(Debug)]
struct SchedulerState {
    /// Next request ID
    next_id: u64,
    /// Warm-up complete
    warmup_complete: bool,
    /// Last request time
    last_request_time: Instant,
}

impl PoissonScheduler {
    /// Create a new Poisson process scheduler
    pub fn new(
        id: String,
        tick_resolution_us: u64, 
        jitter_threshold_ns: u64,
        sampling_config: SamplingConfig,
    ) -> Self {
        let metrics = Arc::new(SchedulerMetrics::new(id.clone()));
        metrics.set_sampling_rate(sampling_config.target_rate);
        
        Self {
            id,
            state: Arc::new(RwLock::new(SchedulerState {
                next_id: 0,
                warmup_complete: false,
                last_request_time: Instant::now(),
            })),
            rng: Arc::new(Mutex::new(SmallRng::from_entropy())),
            sampling_config: Arc::new(RwLock::new(sampling_config)),
            timer: Arc::new(PrecisionTimer::new(
                Duration::from_micros(tick_resolution_us),
                Duration::from_nanos(jitter_threshold_ns),
                metrics.clone(),
            )),
            metrics,
            active: Arc::new(AtomicBool::new(false)),
            registry_handle: Mutex::new(None),
        }
    }
    
    /// Generate an exponentially distributed interval (nanoseconds)
    fn generate_exponential_interval(&self, rate_per_ns: f64) -> f64 {
        let mut rng = self.rng.lock().unwrap();
        let u: f64 = rng.gen();
        -f64::ln(1.0 - u) / rate_per_ns
    }
    
    /// Make an unbiased Bernoulli sampling decision
    /// This implements uniform random sampling with a fixed probability
    fn make_sampling_decision(&self) -> bool {
        let sampling_config = self.sampling_config.read().unwrap();
        let mut rng = self.rng.lock().unwrap();
        
        // Simple Bernoulli trial with probability equal to the target sampling rate
        rng.gen::<f64>() < sampling_config.target_rate
    }
    
    /// Register with metrics registry
    pub fn register_with_registry(&self, registry: Arc<dyn MetricsRegistry>) -> RegistrationHandle {
        let provider = Arc::new(self.clone()) as Arc<dyn MetricsProvider>;
        let handle = registry.register_provider(provider);
        
        let mut registry_handle = self.registry_handle.lock().unwrap();
        *registry_handle = Some(handle.clone());
        
        handle
    }
}

impl RequestScheduler for PoissonScheduler {
    async fn run(
        &self,
        config: Arc<LoadTestConfig>,
        rate_controller: Arc<dyn RateController>,
        ticket_tx: async_channel::Sender<SchedulerTicket>,
        profiler: Option<Arc<dyn TransactionProfiler>>,
    ) {
        let start_time = Instant::now();
        self.active.store(true, Ordering::SeqCst);

        // Main scheduling loop
        let mut warmup_complete = false;
        let warmup_end = start_time + Duration::from_secs(config.warmup_duration_secs);
        
        while start_time.elapsed().as_secs() < config.test_duration_secs && self.active.load(Ordering::SeqCst) {
            // Check if warmup phase is complete
            if !warmup_complete && Instant::now() >= warmup_end {
                warmup_complete = true;
                {
                    let mut state = self.state.write().unwrap();
                    state.warmup_complete = true;
                }
            }
            
            // Get current target RPS from rate controller
            let current_rps = rate_controller.get_current_rps();
            
            if current_rps == 0 {
                // No requests to generate, wait a bit before checking again
                self.timer.sleep(Duration::from_millis(100)).await;
                continue;
            }

            // Calculate next interval using exponential distribution
            let rate_per_ns = (current_rps as f64) / 1_000_000_000.0;
            let next_interval_ns = self.generate_exponential_interval(rate_per_ns);
            let next_interval = Duration::from_nanos(next_interval_ns as u64);

            // Update last request time
            let last_time = {
                let state = self.state.read().unwrap();
                state.last_request_time
            };
            
            // Schedule time for next request
            let scheduled_time = last_time + next_interval;
            
            // Sleep precisely until next request time
            self.timer.sleep_until(scheduled_time).await;
            
            // Generate scheduler ticket with sampling decision
            let (id, is_warmup, should_sample) = {
                let mut state = self.state.write().unwrap();
                let id = state.next_id;
                state.next_id += 1;
                state.last_request_time = scheduled_time;
                
                // Make sampling decision before request execution
                // This ensures unbiased sampling regardless of system performance
                let should_sample = if !state.warmup_complete {
                    false  // Don't sample during warmup
                } else {
                    self.make_sampling_decision()
                };
                
                (id, !state.warmup_complete, should_sample)
            };
            
            // Create scheduler ticket
            let ticket = SchedulerTicket {
                id,
                scheduled_time,
                is_warmup,
                should_sample,
            };

            // Record metrics
            self.metrics.record_request(is_warmup, should_sample);
            self.metrics.record_interval(next_interval_ns as u64);
            
            // Send ticket to downstream components
            if let Err(async_channel::SendError(_)) = ticket_tx.send(ticket.clone()).await {
                // Channel closed, scheduler should terminate
                break;
            }
            
            // Check if we experienced backpressure
            let now = Instant::now();
            if now > scheduled_time {
                let delay = now.duration_since(scheduled_time).as_nanos() as u64;
                self.metrics.record_delay(delay);
            }
        }
    }
    
    fn set_sampling_rate(&self, rate: f64) {
        let mut sampling_config = self.sampling_config.write().unwrap();
        sampling_config.target_rate = rate;
        self.metrics.set_sampling_rate(rate);
    }
    
    fn get_sampling_rate(&self) -> f64 {
        let sampling_config = self.sampling_config.read().unwrap();
        sampling_config.target_rate
    }
    
    fn get_scheduler_type(&self) -> SchedulerType {
        SchedulerType::Poisson
    }
    
    fn terminate(&self) {
        self.active.store(false, Ordering::SeqCst);
    }
}

impl MetricsProvider for PoissonScheduler {
    fn get_metrics(&self) -> ComponentMetrics {
        ComponentMetrics {
            component_type: "RequestScheduler".to_string(),
            component_id: self.id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            metrics: self.metrics.get_snapshot(),
            status: Some(format!("Poisson Scheduler ({}) - Sampling Rate: {:.4}", 
                                self.id, self.get_sampling_rate())),
        }
    }
    
    fn get_component_type(&self) -> &str {
        "RequestScheduler"
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}
```

### 2.2.5 Constant Rate Scheduler with Bernoulli Sampling

```rust
/// Scheduler that generates requests at constant intervals
pub struct ConstantRateScheduler {
    /// Scheduler unique ID
    id: String,
    
    /// Scheduler state
    state: Arc<RwLock<SchedulerState>>,
    
    /// RNG for sampling decisions
    rng: Arc<Mutex<SmallRng>>,
    
    /// Sampling configuration
    sampling_config: Arc<RwLock<SamplingConfig>>,
    
    /// Precision timer for accurate scheduling
    timer: Arc<PrecisionTimer>,
    
    /// Metrics collection
    metrics: Arc<SchedulerMetrics>,
    
    /// Whether scheduler is active
    active: Arc<AtomicBool>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl ConstantRateScheduler {
    /// Create a new constant rate scheduler
    pub fn new(
        id: String,
        tick_resolution_us: u64, 
        jitter_threshold_ns: u64,
        sampling_config: SamplingConfig,
    ) -> Self {
        let metrics = Arc::new(SchedulerMetrics::new(id.clone()));
        metrics.set_sampling_rate(sampling_config.target_rate);
        
        Self {
            id,
            state: Arc::new(RwLock::new(SchedulerState {
                next_id: 0,
                warmup_complete: false,
                last_request_time: Instant::now(),
            })),
            rng: Arc::new(Mutex::new(SmallRng::from_entropy())),
            sampling_config: Arc::new(RwLock::new(sampling_config)),
            timer: Arc::new(PrecisionTimer::new(
                Duration::from_micros(tick_resolution_us),
                Duration::from_nanos(jitter_threshold_ns),
                metrics.clone(),
            )),
            metrics,
            active: Arc::new(AtomicBool::new(false)),
            registry_handle: Mutex::new(None),
        }
    }
    
    /// Make an unbiased Bernoulli sampling decision
    /// This implements uniform random sampling with a fixed probability
    fn make_sampling_decision(&self) -> bool {
        let sampling_config = self.sampling_config.read().unwrap();
        let mut rng = self.rng.lock().unwrap();
        
        // Simple Bernoulli trial with probability equal to the target sampling rate
        rng.gen::<f64>() < sampling_config.target_rate
    }
    
    /// Register with metrics registry
    pub fn register_with_registry(&self, registry: Arc<dyn MetricsRegistry>) -> RegistrationHandle {
        let provider = Arc::new(self.clone()) as Arc<dyn MetricsProvider>;
        let handle = registry.register_provider(provider);
        
        let mut registry_handle = self.registry_handle.lock().unwrap();
        *registry_handle = Some(handle.clone());
        
        handle
    }
}

impl RequestScheduler for ConstantRateScheduler {
    async fn run(
        &self,
        config: Arc<LoadTestConfig>,
        rate_controller: Arc<dyn RateController>,
        ticket_tx: async_channel::Sender<SchedulerTicket>,
        profiler: Option<Arc<dyn TransactionProfiler>>,
    ) {
        let start_time = Instant::now();
        self.active.store(true, Ordering::SeqCst);

        // Initialize scheduler state
        {
            let mut state = self.state.write().unwrap();
            state.last_request_time = start_time;
        }

        // Main scheduling loop
        let mut warmup_complete = false;
        let warmup_end = start_time + Duration::from_secs(config.warmup_duration_secs);
        
        while start_time.elapsed().as_secs() < config.test_duration_secs && self.active.load(Ordering::SeqCst) {
            // Check if warmup phase is complete
            if !warmup_complete && Instant::now() >= warmup_end {
                warmup_complete = true;
                {
                    let mut state = self.state.write().unwrap();
                    state.warmup_complete = true;
                }
            }
            
            // Get current target RPS from rate controller
            let current_rps = rate_controller.get_current_rps();
            
            if current_rps == 0 {
                // No requests to generate, wait a bit before checking again
                self.timer.sleep(Duration::from_millis(100)).await;
                continue;
            }

            // Calculate fixed interval based on RPS
            let interval_ns = 1_000_000_000 / current_rps;
            let interval = Duration::from_nanos(interval_ns);

            // Schedule at least `current_rps` requests in the next second,
            // evenly distributed
            let intervals_per_second = current_rps;
            
            for _ in 0..intervals_per_second {
                // Update last request time
                let last_time = {
                    let state = self.state.read().unwrap();
                    state.last_request_time
                };
                
                // Schedule time for next request
                let scheduled_time = last_time + interval;
                
                // Sleep precisely until next request time
                self.timer.sleep_until(scheduled_time).await;
                
                // Generate scheduler ticket with sampling decision
                let (id, is_warmup, should_sample) = {
                    let mut state = self.state.write().unwrap();
                    let id = state.next_id;
                    state.next_id += 1;
                    state.last_request_time = scheduled_time;
                    
                    // Make sampling decision before request execution
                    // This ensures unbiased sampling regardless of system performance
                    let should_sample = if !state.warmup_complete {
                        false  // Don't sample during warmup
                    } else {
                        self.make_sampling_decision()
                    };
                    
                    (id, !state.warmup_complete, should_sample)
                };
                
                // Create scheduler ticket
                let ticket = SchedulerTicket {
                    id,
                    scheduled_time,
                    is_warmup,
                    should_sample,
                };

                // Record metrics
                self.metrics.record_request(is_warmup, should_sample);
                self.metrics.record_interval(interval_ns);
                
                // Send ticket to downstream components
                if let Err(async_channel::SendError(_)) = ticket_tx.send(ticket.clone()).await {
                    // Channel closed, scheduler should terminate
                    break;
                }
                
                // Check if we experienced backpressure
                let now = Instant::now();
                if now > scheduled_time {
                    let delay = now.duration_since(scheduled_time).as_nanos() as u64;
                    self.metrics.record_delay(delay);
                }
                
                // Check if test should terminate
                if !self.active.load(Ordering::SeqCst) || 
                   start_time.elapsed().as_secs() >= config.test_duration_secs {
                    break;
                }
            }
        }
    }
    
    fn set_sampling_rate(&self, rate: f64) {
        let mut sampling_config = self.sampling_config.write().unwrap();
        sampling_config.target_rate = rate;
        self.metrics.set_sampling_rate(rate);
    }
    
    fn get_sampling_rate(&self) -> f64 {
        let sampling_config = self.sampling_config.read().unwrap();
        sampling_config.target_rate
    }
    
    fn get_scheduler_type(&self) -> SchedulerType {
        SchedulerType::ConstantRate
    }
    
    fn terminate(&self) {
        self.active.store(false, Ordering::SeqCst);
    }
}

impl MetricsProvider for ConstantRateScheduler {
    fn get_metrics(&self) -> ComponentMetrics {
        ComponentMetrics {
            component_type: "RequestScheduler".to_string(),
            component_id: self.id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            metrics: self.metrics.get_snapshot(),
            status: Some(format!("ConstantRate Scheduler ({}) - Sampling Rate: {:.4}", 
                               self.id, self.get_sampling_rate())),
        }
    }
    
    fn get_component_type(&self) -> &str {
        "RequestScheduler"
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}
```

### 2.2.6 Coordinated Omission Prevention

The critical aspects of the scheduler design for preventing coordinated omission are:

1. **Schedule Integrity**: The scheduler maintains the schedule based on intended times rather than actual execution times:

```rust
// Schedule time for next request based on previous scheduled time plus interval
let scheduled_time = last_time + next_interval;

// Sleep until scheduled time
self.timer.sleep_until(scheduled_time).await;

// CRITICAL: Update the last schedule time to the INTENDED time, not actual send time
{
    let mut state = self.state.write().unwrap();
    state.last_request_time = scheduled_time;  // Not Instant::now()!
}
```

2. **Unbiased Sampling**: Sampling decisions are made at scheduling time, before request execution:

```rust
// Make sampling decision BEFORE request execution using Bernoulli sampling
let should_sample = self.make_sampling_decision();

// Create ticket with the scheduled time and sampling decision
let ticket = SchedulerTicket {
    id,
    scheduled_time, // The intended time, preserved for latency calculation
    is_warmup,
    should_sample, // Sampling decision preserved in the ticket
};
```

This approach ensures proper measurement of service degradation and statistical validity, as both the scheduler cadence and sampling decisions are independent of system performance.

### 2.2.7 Builder Pattern for Scheduler and Sampling Configuration

The framework's builder pattern supports configuring scheduler properties with sampling integration:

```rust
impl LoadTestBuilder {
    /// Use a constant rate scheduler
    pub fn with_constant_rate_scheduler(mut self) -> Self {
        self.scheduler_type = SchedulerType::ConstantRate;
        self
    }
    
    /// Use a Poisson process scheduler
    pub fn with_poisson_scheduler(mut self) -> Self {
        self.scheduler_type = SchedulerType::Poisson;
        self
    }
    
    /// Set scheduler timing precision
    pub fn with_scheduler_timing_precision(
        mut self,
        tick_resolution_us: u64,
        jitter_threshold_ns: u64,
    ) -> Self {
        self.scheduler_tick_resolution_us = tick_resolution_us;
        self.scheduler_jitter_threshold_ns = jitter_threshold_ns;
        self
    }
    
    /// Set sampling rate
    pub fn with_sampling_rate(mut self, rate: f64) -> Self {
        self.sampling_config.target_rate = rate;
        self
    }
    
    /// Pin scheduler to a specific core
    pub fn with_scheduler_core_pinning(mut self, core_id: usize) -> Self {
        self.scheduler_core_id = Some(core_id);
        self
    }
    
    /// Build the scheduler component
    fn build_scheduler(&self) -> Arc<dyn RequestScheduler> {
        // Generate a unique ID for this scheduler
        let id = format!("scheduler-{}", Uuid::new_v4());
        
        // Create scheduler based on type
        let scheduler = SchedulerFactory::create_scheduler(
            self.scheduler_type,
            id,
            self.scheduler_tick_resolution_us,
            self.scheduler_jitter_threshold_ns,
            self.sampling_config.clone(),
            self.metrics_registry.clone(),
        );
        
        scheduler
    }
}
```

### 2.2.8 Running on a Dedicated Core

For maximum timing precision, the scheduler can be run on a dedicated CPU core:

```rust
/// Run a scheduler on a dedicated CPU core
pub fn run_scheduler_on_dedicated_core(
    scheduler: Arc<dyn RequestScheduler>,
    config: Arc<LoadTestConfig>,
    rate_controller: Arc<dyn RateController>,
    ticket_tx: async_channel::Sender<SchedulerTicket>,
    profiler: Option<Arc<dyn TransactionProfiler>>,
    core_id: usize,
) -> std::thread::JoinHandle<()> {
    // Create thread for dedicated core
    std::thread::spawn(move || {
        // Set core affinity for this thread
        core_affinity::set_for_current(core_id);
        
        // Set thread priority to maximum (platform-specific)
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::thread::JoinHandleExt;
            let native_handle = std::thread::current().id();
            let pid = nix::unistd::Pid::from_raw(native_handle.as_u64() as i32);
            let param = nix::sched::SchedParam::new(nix::sched::sched_get_priority_max(nix::sched::SCHED_FIFO).unwrap());
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
                rate_controller,
                ticket_tx,
                profiler,
            ).await;
        });
    })
}
```

### 2.2.9 Statistical Validity of Bernoulli Sampling

Bernoulli sampling (uniform random sampling with fixed probability) has several statistical advantages:

1. **Unbiased Representation**: Each request has an equal probability of being selected, regardless of when it occurs.

2. **Statistical Independence**: The sampling decision for each request is independent of all other decisions.

3. **Simplicity**: The implementation is straightforward and efficient.

4. **Well-Understood Properties**: Bernoulli sampling has well-established statistical properties, making it easier to analyze the results.

By making these sampling decisions at the scheduler level - before request execution - we ensure that the sampling process is not influenced by system behavior or performance, maintaining statistical validity even under heavy load or when the system under test experiences degradation.

### 2.2.10 Monitoring Sampling Behavior 

The metrics system allows for comprehensive monitoring of sampling behavior:

```rust
/// Access scheduler metrics via registry
pub fn get_scheduler_metrics(registry: &Arc<dyn MetricsRegistry>) -> Vec<ComponentMetrics> {
    registry.collect_by_type("RequestScheduler")
}

/// Get specific sampling metrics
pub fn get_sampling_metrics(registry: &Arc<dyn MetricsRegistry>) -> HashMap<String, f64> {
    let mut result = HashMap::new();
    
    for metrics in registry.collect_by_type("RequestScheduler") {
        if let Some(MetricValue::Float(rate)) = 
            metrics.metrics.get("sampling_rate") {
            result.insert(format!("{}_target", metrics.component_id), *rate);
        }
        
        if let Some(MetricValue::Float(actual)) = 
            metrics.metrics.get("actual_sampling_rate") {
            result.insert(format!("{}_actual", metrics.component_id), *actual);
        }
        
        if let Some(MetricValue::Counter(count)) = 
            metrics.metrics.get("sampled_count") {
            result.insert(format!("{}_count", metrics.component_id), *count as f64);
        }
    }
    
    result
}

/// Monitor sampling rates and verify statistical validity
pub async fn monitor_sampling_rates(registry: Arc<dyn MetricsRegistry>) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    
    loop {
        interval.tick().await;
        
        let sampling_metrics = get_sampling_metrics(&registry);
        
        for (key, value) in sampling_metrics {
            if key.ends_with("_actual") {
                let scheduler_id = key.replace("_actual", "");
                let target = sampling_metrics.get(&format!("{}_target", scheduler_id))
                    .cloned().unwrap_or(0.0);
                let count = sampling_metrics.get(&format!("{}_count", scheduler_id))
                    .cloned().unwrap_or(0.0);
                
                println!("Scheduler {}: Target sampling rate: {:.4}, Actual rate: {:.4}, Samples: {}", 
                         scheduler_id, target, value, count as u64);
                
                // Statistical validity check - actual rate should approach target rate
                // as sample size increases (Law of Large Numbers)
                if count > 1000.0 {
                    // With large sample count, actual rate should be close to target
                    // Expected standard deviation for Bernoulli distribution:
                    // sqrt(p*(1-p)/n) where p is probability and n is sample count
                    let expected_std_dev = (target * (1.0 - target) / count).sqrt();
                    let deviation = (value - target).abs();
                    
                    if deviation > 3.0 * expected_std_dev && target > 0.0 {
                        // More than 3 standard deviations suggests potential bias
                        println!("Warning: Actual sampling rate deviates significantly from target");
                        println!("Deviation: {:.4}, Expected std dev: {:.4}", deviation, expected_std_dev);
                    }
                }
            }
        }
    }
}
```

This approach ensures that both the core scheduling logic and the sampling decisions are unbiased and statistically valid, and provides metrics to verify this during testing.
