# Rate Control System with Metrics Integration and Distributed Load Control

## 1. Introduction

The Rate Control system is a core component of the load testing framework, determining how many requests per second should be generated during test execution. This design combines comprehensive metrics integration for observability with sophisticated distributed load control capabilities, enabling both precise monitoring and coordinated multi-node testing.

Key features include:

- **Unified Metrics Integration**: Lock-free metrics collection with registry integration
- **Advanced Control Algorithms**: PID controllers, step functions, and external signal-based control
- **Rate Smoothing**: Prevent abrupt changes that could skew results
- **Saturation Awareness**: Detect and adapt to client-side bottlenecks
- **Distributed Load Control**: Coordinate rate across multiple nodes with centralized control
- **Composable Design**: Mix and match controllers, smoothers, and adapters for different scenarios

## 2. Core Interfaces and Types

### 2.1 Rate Controller with Metrics Integration

```rust
/// Interface for controllers that determine request rates during the test
pub trait RateController: MetricsProvider + Send + Sync {
    /// Get the current target RPS
    fn get_current_rps(&self) -> u64;

    /// Update controller state based on metrics
    fn update(&self, metrics: &RequestMetrics);

    /// Process external control signals if supported
    fn process_control_signals(&self, signals: &ControlSignalSet) {
        // Default implementation does nothing
    }

    /// Called at the end of the test to get rate history
    fn get_rate_history(&self) -> Vec<(u64, u64)>;

    /// Get rate adjustments with reasons
    fn get_rate_adjustments(&self) -> Vec<(u64, RateAdjustment)>;
}

/// Metric values captured during the test execution
#[derive(Debug, Clone)]
pub struct RequestMetrics {
    /// Timestamp when metrics were collected (ms since test start)
    pub timestamp: u64,
    /// Size of the time window these metrics represent (ms)
    pub window_size_ms: u64,
    /// Number of requests captured in this window
    pub request_count: u64,
    /// 50th percentile latency in microseconds
    pub p50_latency_us: u64,
    /// 90th percentile latency in microseconds
    pub p90_latency_us: u64,
    /// 99th percentile latency in microseconds
    pub p99_latency_us: u64,
    /// Error rate (0.0 - 1.0)
    pub error_rate: f64,
    /// Current throughput (requests per second)
    pub current_throughput: f64,
}

/// Rate adjustment with reason
#[derive(Debug, Clone)]
pub struct RateAdjustment {
    /// New target RPS
    pub new_rps: u64,
    /// Previous target RPS
    pub old_rps: u64,
    /// Reason for adjustment
    pub reason: AdjustmentReason,
    /// Additional context for the adjustment
    pub context: String,
}

/// Reasons for rate adjustments
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjustmentReason {
    /// Initial rate setting
    Initial,
    /// PID control adjustment
    PidControl,
    /// Step function change
    StepFunction,
    /// Manual adjustment
    Manual,
    /// External signal influence
    ExternalSignal,
    /// Client saturation detected
    Saturation,
    /// Critical error rate exceeded
    ErrorThreshold,
    /// Rate smoothing applied
    RateSmoothing,
    /// Distributed coordination adjustment
    DistributedControl,
    /// Other reason
    Other,
}
```

### 2.2 Rate Control Modes

```rust
/// Modes for controlling request rate
#[derive(Debug, Clone)]
pub enum RateControlMode {
    /// Keep constant rate
    Static,
    /// PID controller parameters
    Pid {
        /// Target metric value (e.g. 200ms for latency)
        target_metric: f64,
        /// Metric type to control
        metric_type: MetricType,
        /// Proportional gain
        kp: f64,
        /// Integral gain
        ki: f64,
        /// Derivative gain
        kd: f64,
        /// Update interval in milliseconds
        update_interval_ms: u64,
    },
    /// External signal controlled PID
    ExternalSignalPid {
        /// Target metric value
        target_metric: f64,
        /// Proportional gain
        kp: f64,
        /// Integral gain
        ki: f64,
        /// Derivative gain
        kd: f64,
        /// Update interval in milliseconds
        update_interval_ms: u64,
        /// Signal name to subscribe to
        signal_name: String,
    },
    /// Saturation-aware PID controller
    SaturationAwarePid {
        /// Target metric value
        target_metric: f64,
        /// Proportional gain
        kp: f64,
        /// Integral gain
        ki: f64,
        /// Derivative gain
        kd: f64,
        /// Update interval in milliseconds
        update_interval_ms: u64,
        /// Saturation threshold for adjustment
        saturation_threshold: f64,
    },
    /// Step function (time in seconds -> RPS)
    StepFunction(Vec<(u64, u64)>),
    /// Custom controller (clients can implement their own)
    Custom,
}

/// Types of metrics that can be targeted by the PID controller
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetricType {
    /// 50th percentile latency
    LatencyP50,
    /// 90th percentile latency
    LatencyP90,
    /// 95th percentile latency
    LatencyP95,
    /// 99th percentile latency
    LatencyP99,
    /// Error rate (0.0-1.0)
    ErrorRate,
    /// Custom metric provided by an external signal
    ExternalSignal,
}
```

## 3. Rate Controller Metrics

### 3.1 Lock-Free Metrics Implementation

```rust
/// Lock-free metrics collection for rate controllers
pub struct RateControllerMetrics {
    /// Component ID
    id: String,
    
    /// Current target RPS
    current_rps: AtomicU64,
    
    /// Minimum RPS
    min_rps: AtomicU64,
    
    /// Maximum RPS
    max_rps: AtomicU64,
    
    /// Number of rate adjustments
    adjustment_count: AtomicU64,
    
    /// Last adjustment timestamp
    last_adjustment: AtomicU64,
    
    /// PID controller metrics (if applicable)
    error_value: AtomicF64,
    integral_term: AtomicF64,
    derivative_term: AtomicF64,
    
    /// Bounded history of rate changes
    /// Protected with RwLock but updated infrequently
    rate_history: Arc<RwLock<VecDeque<(u64, u64, String)>>>,
    
    /// Smoothing metrics (if applicable)
    smoothing_limits_applied: AtomicU64,
    max_smoothing_change: AtomicU64,
    
    /// Distributed control metrics (if applicable)
    distributed_proportion_factor: AtomicF64,
    rebalance_count: AtomicU64,
}

impl RateControllerMetrics {
    /// Create new rate controller metrics
    pub fn new(id: String, initial_rps: u64, min_rps: u64, max_rps: u64) -> Self {
        Self {
            id,
            current_rps: AtomicU64::new(initial_rps),
            min_rps: AtomicU64::new(min_rps),
            max_rps: AtomicU64::new(max_rps),
            adjustment_count: AtomicU64::new(0),
            last_adjustment: AtomicU64::new(current_timestamp()),
            error_value: AtomicF64::new(0.0),
            integral_term: AtomicF64::new(0.0),
            derivative_term: AtomicF64::new(0.0),
            rate_history: Arc::new(RwLock::new(VecDeque::with_capacity(100))),
            smoothing_limits_applied: AtomicU64::new(0),
            max_smoothing_change: AtomicU64::new(0),
            distributed_proportion_factor: AtomicF64::new(1.0),
            rebalance_count: AtomicU64::new(0),
        }
    }
    
    /// Update the current RPS
    pub fn update_rps(&self, new_rps: u64, reason: &str) {
        // Store previous value for history
        let old_rps = self.current_rps.load(Ordering::Relaxed);
        
        // Update current RPS
        self.current_rps.store(new_rps, Ordering::Relaxed);
        
        // Increment adjustment count
        self.adjustment_count.fetch_add(1, Ordering::Relaxed);
        
        // Update timestamp
        let now = current_timestamp();
        self.last_adjustment.store(now, Ordering::Relaxed);
        
        // Record in history (with bounded size)
        let reason_str = reason.to_string();
        if let Ok(mut history) = self.rate_history.write() {
            history.push_back((now, new_rps, reason_str));
            
            // Keep bounded size
            while history.len() > 100 {
                history.pop_front();
            }
        }
    }
    
    /// Update PID controller metrics
    pub fn update_pid_metrics(&self, error: f64, integral: f64, derivative: f64) {
        self.error_value.store(error, Ordering::Relaxed);
        self.integral_term.store(integral, Ordering::Relaxed);
        self.derivative_term.store(derivative, Ordering::Relaxed);
    }
    
    /// Record smoothing metrics
    pub fn record_smoothing(&self, requested_rps: u64, actual_rps: u64) {
        if requested_rps != actual_rps {
            self.smoothing_limits_applied.fetch_add(1, Ordering::Relaxed);
            
            let change = (requested_rps as i64 - actual_rps as i64).abs() as u64;
            let current_max = self.max_smoothing_change.load(Ordering::Relaxed);
            
            // Update max change if larger
            if change > current_max {
                let _ = self.max_smoothing_change.compare_exchange(
                    current_max, change, Ordering::Relaxed, Ordering::Relaxed
                );
            }
        }
    }
    
    /// Update distributed control metrics
    pub fn update_distributed_metrics(&self, proportion_factor: f64) {
        self.distributed_proportion_factor.store(proportion_factor, Ordering::Relaxed);
        self.rebalance_count.fetch_add(1, Ordering::Relaxed);
    }
    
    /// Get a snapshot of metrics for registry
    pub fn get_snapshot(&self) -> HashMap<String, MetricValue> {
        let mut metrics = HashMap::new();
        
        // Add atomic counter values
        metrics.insert("current_rps".to_string(), 
                      MetricValue::Gauge(self.current_rps.load(Ordering::Relaxed) as i64));
                      
        metrics.insert("min_rps".to_string(),
                      MetricValue::Gauge(self.min_rps.load(Ordering::Relaxed) as i64));
                      
        metrics.insert("max_rps".to_string(),
                      MetricValue::Gauge(self.max_rps.load(Ordering::Relaxed) as i64));
                      
        metrics.insert("adjustment_count".to_string(),
                      MetricValue::Counter(self.adjustment_count.load(Ordering::Relaxed)));
                      
        metrics.insert("last_adjustment".to_string(),
                      MetricValue::Counter(self.last_adjustment.load(Ordering::Relaxed)));
        
        // Add PID controller metrics if applicable
        metrics.insert("error_value".to_string(),
                      MetricValue::Float(self.error_value.load(Ordering::Relaxed)));
                      
        metrics.insert("integral_term".to_string(),
                      MetricValue::Float(self.integral_term.load(Ordering::Relaxed)));
                      
        metrics.insert("derivative_term".to_string(),
                      MetricValue::Float(self.derivative_term.load(Ordering::Relaxed)));
        
        // Add smoothing metrics if applicable
        metrics.insert("smoothing_limits_applied".to_string(),
                      MetricValue::Counter(self.smoothing_limits_applied.load(Ordering::Relaxed)));
                      
        metrics.insert("max_smoothing_change".to_string(),
                      MetricValue::Counter(self.max_smoothing_change.load(Ordering::Relaxed)));
        
        // Add distributed control metrics if applicable
        metrics.insert("distributed_proportion_factor".to_string(),
                      MetricValue::Float(self.distributed_proportion_factor.load(Ordering::Relaxed)));
                      
        metrics.insert("rebalance_count".to_string(),
                      MetricValue::Counter(self.rebalance_count.load(Ordering::Relaxed)));
        
        // Add recent history as a text metric
        if let Ok(history) = self.rate_history.read() {
            // Convert the last 5 adjustments to a compact string
            let recent_history = history.iter()
                .rev()
                .take(5)
                .map(|(ts, rps, reason)| format!("{}:{}:{}", ts, rps, reason))
                .collect::<Vec<_>>()
                .join("; ");
                
            metrics.insert("recent_adjustments".to_string(),
                          MetricValue::Text(recent_history));
        }
        
        metrics
    }
}

/// Helper function to get current timestamp
fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
```

## 4. Control Signal System with Metrics

### 4.1 Control Signal Interface

```rust
/// External control signal interface
pub trait ControlSignal: MetricsProvider + Send + Sync {
    /// Get the current signal value
    fn get_current_value(&self) -> f64;

    /// Register a callback for signal updates
    fn register_update_callback(&mut self, callback: Box<dyn Fn(f64) + Send + Sync>);

    /// Get historical values with timestamps (for reporting)
    fn get_history(&self) -> Vec<(u64, f64)>;
}

/// Set of control signals available to controllers
#[derive(Clone)]
pub struct ControlSignalSet {
    /// Named signals
    signals: Arc<DashMap<String, Arc<dyn ControlSignal>>>,
}

impl ControlSignalSet {
    /// Create a new control signal set
    pub fn new() -> Self {
        Self {
            signals: Arc::new(DashMap::new()),
        }
    }

    /// Register a new control signal
    pub fn register_signal(&self, name: &str, signal: Arc<dyn ControlSignal>) {
        self.signals.insert(name.to_string(), signal);
    }

    /// Get a control signal by name
    pub fn get_signal(&self, name: &str) -> Option<Arc<dyn ControlSignal>> {
        self.signals.get(name).map(|s| s.clone())
    }

    /// Get all signal names
    pub fn get_signal_names(&self) -> Vec<String> {
        self.signals.iter().map(|s| s.key().clone()).collect()
    }
}
```

### 4.2 Signal Metrics Implementation

```rust
/// Metrics for control signals
pub struct ControlSignalMetrics {
    /// Component ID
    id: String,
    
    /// Current signal value
    current_value: AtomicF64,
    
    /// Signal update count
    update_count: AtomicU64,
    
    /// Last update timestamp
    last_update: AtomicU64,
    
    /// Minimum value observed
    min_value: AtomicF64,
    
    /// Maximum value observed
    max_value: AtomicF64,
    
    /// Callback count
    callback_count: AtomicU64,
    
    /// History of signal values
    /// Protected with RwLock but updated selectively
    value_history: Arc<RwLock<VecDeque<(u64, f64)>>>,
}

impl ControlSignalMetrics {
    /// Create new control signal metrics
    pub fn new(id: String, initial_value: f64) -> Self {
        Self {
            id,
            current_value: AtomicF64::new(initial_value),
            update_count: AtomicU64::new(0),
            last_update: AtomicU64::new(current_timestamp()),
            min_value: AtomicF64::new(initial_value),
            max_value: AtomicF64::new(initial_value),
            callback_count: AtomicU64::new(0),
            value_history: Arc::new(RwLock::new(VecDeque::with_capacity(100))),
        }
    }
    
    /// Update signal value
    pub fn update_value(&self, new_value: f64) {
        // Update current value
        self.current_value.store(new_value, Ordering::Relaxed);
        
        // Increment update count
        self.update_count.fetch_add(1, Ordering::Relaxed);
        
        // Update timestamp
        let now = current_timestamp();
        self.last_update.store(now, Ordering::Relaxed);
        
        // Update min/max values
        let current_min = self.min_value.load(Ordering::Relaxed);
        if new_value < current_min {
            self.min_value.store(new_value, Ordering::Relaxed);
        }
        
        let current_max = self.max_value.load(Ordering::Relaxed);
        if new_value > current_max {
            self.max_value.store(new_value, Ordering::Relaxed);
        }
        
        // Record in history (with bounded size)
        // Only sample some updates to reduce contention
        if self.update_count.load(Ordering::Relaxed) % 10 == 0 {
            if let Ok(mut history) = self.value_history.write() {
                history.push_back((now, new_value));
                
                // Keep bounded size
                while history.len() > 100 {
                    history.pop_front();
                }
            }
        }
    }
    
    /// Record callback execution
    pub fn record_callback(&self) {
        self.callback_count.fetch_add(1, Ordering::Relaxed);
    }
    
    /// Get a snapshot of metrics for registry
    pub fn get_snapshot(&self) -> HashMap<String, MetricValue> {
        let mut metrics = HashMap::new();
        
        // Add atomic values
        metrics.insert("current_value".to_string(), 
                      MetricValue::Float(self.current_value.load(Ordering::Relaxed)));
                      
        metrics.insert("update_count".to_string(),
                      MetricValue::Counter(self.update_count.load(Ordering::Relaxed)));
                      
        metrics.insert("last_update".to_string(),
                      MetricValue::Counter(self.last_update.load(Ordering::Relaxed)));
                      
        metrics.insert("min_value".to_string(),
                      MetricValue::Float(self.min_value.load(Ordering::Relaxed)));
                      
        metrics.insert("max_value".to_string(),
                      MetricValue::Float(self.max_value.load(Ordering::Relaxed)));
                      
        metrics.insert("callback_count".to_string(),
                      MetricValue::Counter(self.callback_count.load(Ordering::Relaxed)));
        
        // Add recent history as a text metric
        if let Ok(history) = self.value_history.read() {
            // Calculate average of recent values (last 10)
            let recent_avg = if !history.is_empty() {
                let recent = history.iter().rev().take(10);
                let (sum, count) = recent.fold((0.0, 0), |(sum, count), (_, value)| {
                    (sum + value, count + 1)
                });
                
                if count > 0 {
                    sum / count as f64
                } else {
                    0.0
                }
            } else {
                0.0
            };
            
            metrics.insert("recent_avg".to_string(),
                          MetricValue::Float(recent_avg));
        }
        
        metrics
    }
}
```

## 5. Rate Smoothing with Metrics Integration

### 5.1 Rate Smoothing Configuration

```rust
/// Rate smoothing configuration
#[derive(Debug, Clone)]
pub struct RateSmoothingConfig {
    /// Whether smoothing is enabled
    pub enabled: bool,
    /// Maximum relative change per second (0.0-1.0)
    /// e.g., 0.1 means max 10% change per second
    pub max_relative_change_per_second: f64,
    /// Maximum absolute change per second (RPS)
    pub max_absolute_change_per_second: u64,
    /// Minimum time between adjustments (ms)
    pub min_adjustment_interval_ms: u64,
    /// Whether to apply aggressive limiting to decreases
    /// (Some systems are more sensitive to sudden load drops)
    pub limit_decreases: bool,
}

impl Default for RateSmoothingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_relative_change_per_second: 0.1, // 10% per second
            max_absolute_change_per_second: 100, // 100 RPS per second
            min_adjustment_interval_ms: 100,     // 100ms
            limit_decreases: true,
        }
    }
}
```

### 5.2 Rate Smoother with Metrics

```rust
/// Rate smoother to prevent abrupt rate changes
pub struct RateSmoother {
    /// Component ID
    id: String,
    
    /// Metrics collection
    metrics: Arc<RateControllerMetrics>,
    
    /// Smoothing configuration
    config: RateSmoothingConfig,
    
    /// Last adjustment time
    last_adjustment: Mutex<Instant>,
    
    /// Rate adjustment history
    adjustments: Mutex<Vec<(u64, RateAdjustment)>>,
    
    /// Start time
    start_time: Instant,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl RateSmoother {
    /// Create a new rate smoother
    pub fn new(
        id: String,
        initial_rps: u64,
        min_rps: u64,
        max_rps: u64,
        config: RateSmoothingConfig
    ) -> Self {
        // Create metrics
        let metrics = Arc::new(RateControllerMetrics::new(
            id.clone(), initial_rps, min_rps, max_rps
        ));
        
        Self {
            id,
            metrics,
            config,
            last_adjustment: Mutex::new(Instant::now()),
            adjustments: Mutex::new(Vec::new()),
            start_time: Instant::now(),
            registry_handle: Mutex::new(None),
        }
    }
    
    /// Apply smoothing to a target RPS value
    pub fn smooth_rate(&self, target_rps: u64) -> u64 {
        if !self.config.enabled {
            return target_rps;
        }
        
        let current_rps = self.metrics.current_rps.load(Ordering::Relaxed);
        
        // If minimal change, return directly
        if (target_rps as i64 - current_rps as i64).abs() <= 1 {
            return target_rps;
        }
        
        // Check if enough time has passed since last adjustment
        let mut last_adjustment = self.last_adjustment.lock().unwrap();
        let elapsed_ms = last_adjustment.elapsed().as_millis() as u64;
        
        if elapsed_ms < self.config.min_adjustment_interval_ms {
            // Too soon, return current RPS
            return current_rps;
        }
        
        // Calculate maximum allowed change based on time elapsed
        let elapsed_seconds = elapsed_ms as f64 / 1000.0;
        
        // Relative limit
        let max_rel_change = self.config.max_relative_change_per_second * elapsed_seconds;
        let rel_limit = (current_rps as f64 * max_rel_change).round() as u64;
        
        // Absolute limit
        let abs_limit = (self.config.max_absolute_change_per_second as f64 * elapsed_seconds).round() as u64;
        
        // Apply the more restrictive limit
        let max_change = rel_limit.min(abs_limit).max(1); // Ensure at least 1 RPS change
        
        // Calculate direction and magnitude of change
        let is_increase = target_rps > current_rps;
        let raw_change = if is_increase {
            target_rps - current_rps
        } else {
            current_rps - target_rps
        };
        
        // Apply potentially different limits for increases vs decreases
        let limited_change = if !is_increase && !self.config.limit_decreases {
            // Don't limit decreases if configured that way
            raw_change
        } else {
            // Apply limit
            raw_change.min(max_change)
        };
        
        // Calculate new RPS
        let new_rps = if is_increase {
            current_rps + limited_change
        } else {
            current_rps - limited_change
        };
        
        // Update metrics for smoothing
        self.metrics.record_smoothing(target_rps, new_rps);
        
        // Update last adjustment time
        *last_adjustment = Instant::now();
        
        // Record adjustment if significant
        if new_rps != current_rps {
            let context = format!(
                "Target: {}, Limited change: {}/{} RPS ({}%)",
                target_rps, limited_change, raw_change,
                (limited_change as f64 / current_rps as f64 * 100.0).round()
            );
            
            // Update metrics with new RPS
            self.metrics.update_rps(new_rps, &context);
            
            // Record adjustment
            let now = self.start_time.elapsed().as_millis() as u64;
            let adjustment = RateAdjustment {
                new_rps,
                old_rps: current_rps,
                reason: AdjustmentReason::RateSmoothing,
                context,
            };
            
            let mut adjustments = self.adjustments.lock().unwrap();
            adjustments.push((now, adjustment));
        }
        
        new_rps
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

impl MetricsProvider for RateSmoother {
    fn get_metrics(&self) -> ComponentMetrics {
        ComponentMetrics {
            component_type: "RateSmoother".to_string(),
            component_id: self.id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            metrics: self.metrics.get_snapshot(),
            status: Some(format!("Rate Smoother: RelChange={:.1}%, AbsChange={} RPS/s",
                      self.config.max_relative_change_per_second * 100.0,
                      self.config.max_absolute_change_per_second)),
        }
    }
    
    fn get_component_type(&self) -> &str {
        "RateSmoother"
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}
```

### 5.3 Smoothed Rate Controller with Metrics

```rust
/// Rate controller that applies smoothing to another controller
pub struct SmoothedRateController {
    /// Controller ID
    id: String,
    
    /// Underlying controller
    controller: Arc<dyn RateController>,
    
    /// Rate smoother
    smoother: Arc<RateSmoother>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl SmoothedRateController {
    /// Create a new smoothed rate controller
    pub fn new(
        id: String,
        controller: Arc<dyn RateController>,
        initial_rps: u64,
        min_rps: u64,
        max_rps: u64,
        smoothing_config: RateSmoothingConfig,
    ) -> Self {
        Self {
            id,
            controller,
            smoother: Arc::new(RateSmoother::new(
                format!("{}-smoother", id),
                initial_rps,
                min_rps,
                max_rps,
                smoothing_config,
            )),
            registry_handle: Mutex::new(None),
        }
    }
    
    /// Register with metrics registry
    pub fn register_with_registry(&self, registry: Arc<dyn MetricsRegistry>) -> RegistrationHandle {
        // Register the smoother first
        self.smoother.register_with_registry(registry.clone());
        
        // Register this controller
        let provider = Arc::new(self.clone()) as Arc<dyn MetricsProvider>;
        let handle = registry.register_provider(provider);
        
        let mut registry_handle = self.registry_handle.lock().unwrap();
        *registry_handle = Some(handle.clone());
        
        handle
    }
}

impl RateController for SmoothedRateController {
    fn get_current_rps(&self) -> u64 {
        // Get target RPS from underlying controller
        let target_rps = self.controller.get_current_rps();
        
        // Apply smoothing
        self.smoother.smooth_rate(target_rps)
    }

    fn update(&self, metrics: &RequestMetrics) {
        // Forward to underlying controller
        self.controller.update(metrics);
    }

    fn process_control_signals(&self, signals: &ControlSignalSet) {
        // Forward to underlying controller
        self.controller.process_control_signals(signals);
    }

    fn get_rate_history(&self) -> Vec<(u64, u64)> {
        // Combine underlying controller history with our smoothed adjustments
        let mut history = self.controller.get_rate_history();
        
        // Add smoothed adjustments where they don't already exist
        for (time, adjustment) in self.smoother.adjustments.lock().unwrap().iter() {
            // Check if this timestamp already exists
            let exists = history.iter().any(|(t, _)| *t == *time);
            
            if !exists {
                history.push((*time, adjustment.new_rps));
            }
        }
        
        // Sort by timestamp
        history.sort_by_key(|(t, _)| *t);
        
        history
    }

    fn get_rate_adjustments(&self) -> Vec<(u64, RateAdjustment)> {
        // Combine underlying controller adjustments with our smoothed adjustments
        let mut adjustments = self.controller.get_rate_adjustments();
        
        // Add smoother adjustments
        adjustments.extend(self.smoother.adjustments.lock().unwrap().clone());
        
        // Sort by timestamp
        adjustments.sort_by_key(|(t, _)| *t);
        
        adjustments
    }
}

impl MetricsProvider for SmoothedRateController {
    fn get_metrics(&self) -> ComponentMetrics {
        ComponentMetrics {
            component_type: "RateController".to_string(),
            component_id: self.id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            metrics: HashMap::new(), // This controller delegates metrics to its components
            status: Some(format!("Smoothed Rate Controller ({})", self.id)),
        }
    }
    
    fn get_component_type(&self) -> &str {
        "RateController"
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}
```

## 6. Saturation Awareness with Metrics

### 6.1 Saturation Metrics

```rust
/// Client saturation metrics
#[derive(Debug, Clone)]
pub struct SaturationMetrics {
    /// Average overhead percentage across recent transactions
    pub avg_overhead_percentage: f64,
    /// 95th percentile overhead percentage
    pub p95_overhead_percentage: f64,
    /// Average read delay in microseconds
    pub avg_read_delay_us: f64,
    /// 95th percentile read delay
    pub p95_read_delay_us: u64,
    /// Classification of current saturation
    pub level: SaturationLevel,
    /// Current client resource utilization metrics
    pub resource_utilization: ResourceUtilization,
    /// Recommended actions based on saturation level
    pub recommendation: SaturationAction,
    /// Detailed analysis if available
    pub analysis: Option<String>,
}

/// Client saturation level classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaturationLevel {
    /// Insignificant saturation (<10% overhead)
    None,
    /// Mild saturation (10-20% overhead)
    Mild,
    /// Moderate saturation (20-50% overhead)
    Moderate,
    /// Severe saturation (>50% overhead)
    Severe,
}

/// Client resource utilization metrics
#[derive(Debug, Clone)]
pub struct ResourceUtilization {
    /// CPU utilization (0.0-1.0)
    pub cpu_utilization: f64,
    /// Memory utilization (0.0-1.0)
    pub memory_utilization: f64,
    /// Connection pool utilization (0.0-1.0)
    pub connection_pool_utilization: f64,
    /// Tokio runtime task queue depth
    pub task_queue_depth: u64,
    /// Network transmission queue depth
    pub network_queue_depth: u64,
    /// Average backpressure wait time (ms)
    pub backpressure_wait_ms: f64,
}

/// Recommended actions based on saturation
#[derive(Debug, Clone)]
pub enum SaturationAction {
    /// No action needed
    None,
    /// Warning about mild saturation
    WarnOnly,
    /// Reduce rate to specified target
    ReduceRate(u64),
    /// Stop increasing rate
    StopRateIncrease,
    /// Distribute load across more clients
    DistributeLoad(u32),
}
```

### 6.2 Saturation Signal Provider with Metrics

```rust
/// Saturation signal provider
pub struct SaturationSignalProvider {
    /// Signal ID
    id: String,
    
    /// Profiler for detailed transaction analysis
    profiler: Arc<dyn TransactionProfiler>,
    
    /// Current saturation metrics
    current_metrics: Arc<RwLock<SaturationMetrics>>,
    
    /// Saturation level history
    history: Arc<Mutex<Vec<(u64, SaturationLevel)>>>,
    
    /// Update callbacks
    callbacks: Arc<Mutex<Vec<Box<dyn Fn(f64) + Send + Sync>>>>,
    
    /// Start time
    start_time: Instant,
    
    /// Last update time
    last_update: Arc<AtomicU64>,
    
    /// Update interval
    update_interval: Duration,
    
    /// Signal metrics
    metrics: Arc<ControlSignalMetrics>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl SaturationSignalProvider {
    /// Create a new saturation signal provider
    pub fn new(
        id: String,
        profiler: Arc<dyn TransactionProfiler>,
        update_interval: Duration,
    ) -> Self {
        // Initial signal value of 0.0 (no saturation)
        let metrics = Arc::new(ControlSignalMetrics::new(
            id.clone(), 0.0
        ));
        
        Self {
            id,
            profiler,
            current_metrics: Arc::new(RwLock::new(SaturationMetrics::default())),
            history: Arc::new(Mutex::new(Vec::new())),
            callbacks: Arc::new(Mutex::new(Vec::new())),
            start_time: Instant::now(),
            last_update: Arc::new(AtomicU64::new(0)),
            update_interval,
            metrics,
            registry_handle: Mutex::new(None),
        }
    }
    
    /// Start background task to update saturation metrics
    pub fn start_monitoring(&self) -> JoinHandle<()> {
        let profiler = self.profiler.clone();
        let current_metrics = self.current_metrics.clone();
        let history = self.history.clone();
        let callbacks = self.callbacks.clone();
        let start_time = self.start_time;
        let last_update = self.last_update.clone();
        let update_interval = self.update_interval;
        let metrics = self.metrics.clone();
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(update_interval);
            
            loop {
                interval.tick().await;
                
                // Get saturation metrics from profiler
                let saturation = profiler.get_saturation_metrics();
                
                // Update current metrics
                let mut current = current_metrics.write().unwrap();
                *current = saturation.clone();
                
                // Update history
                let now = start_time.elapsed().as_millis() as u64;
                let mut hist = history.lock().unwrap();
                hist.push((now, saturation.level));
                
                // Update timestamp
                last_update.store(now, Ordering::Relaxed);
                
                // Map saturation level to a numerical value
                let value = match saturation.level {
                    SaturationLevel::None => 0.0,
                    SaturationLevel::Mild => 0.33,
                    SaturationLevel::Moderate => 0.66,
                    SaturationLevel::Severe => 1.0,
                };
                
                // Update signal metrics
                metrics.update_value(value);
                
                // Notify callbacks
                let callbacks_ref = callbacks.lock().unwrap();
                for callback in callbacks_ref.iter() {
                    callback(value);
                    metrics.record_callback();
                }
            }
        })
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

impl ControlSignal for SaturationSignalProvider {
    fn get_current_value(&self) -> f64 {
        // Map saturation level to a numerical value between 0.0 and 1.0
        let metrics = self.current_metrics.read().unwrap();
        match metrics.level {
            SaturationLevel::None => 0.0,
            SaturationLevel::Mild => 0.33,
            SaturationLevel::Moderate => 0.66,
            SaturationLevel::Severe => 1.0,
        }
    }

    fn register_update_callback(&mut self, callback: Box<dyn Fn(f64) + Send + Sync>) {
        let mut callbacks = self.callbacks.lock().unwrap();
        callbacks.push(callback);
    }

    fn get_history(&self) -> Vec<(u64, f64)> {
        let history = self.history.lock().unwrap();
        history
            .iter()
            .map(|(ts, level)| {
                let value = match level {
                    SaturationLevel::None => 0.0,
                    SaturationLevel::Mild => 0.33,
                    SaturationLevel::Moderate => 0.66,
                    SaturationLevel::Severe => 1.0,
                };
                (*ts, value)
            })
            .collect()
    }
}

impl MetricsProvider for SaturationSignalProvider {
    fn get_metrics(&self) -> ComponentMetrics {
        // Create combined metrics from signal metrics and saturation details
        let mut metrics = self.metrics.get_snapshot();
        
        // Add saturation-specific metrics
        let saturation = self.current_metrics.read().unwrap();
        
        metrics.insert("saturation_level".to_string(),
                      MetricValue::Float(match saturation.level {
                          SaturationLevel::None => 0.0,
                          SaturationLevel::Mild => 0.33,
                          SaturationLevel::Moderate => 0.66,
                          SaturationLevel::Severe => 1.0,
                      }));
                      
        metrics.insert("overhead_percentage_avg".to_string(),
                      MetricValue::Float(saturation.avg_overhead_percentage));
                      
        metrics.insert("overhead_percentage_p95".to_string(),
                      MetricValue::Float(saturation.p95_overhead_percentage));
                      
        metrics.insert("read_delay_us_avg".to_string(),
                      MetricValue::Float(saturation.avg_read_delay_us));
                      
        metrics.insert("read_delay_us_p95".to_string(),
                      MetricValue::Duration(saturation.p95_read_delay_us));
        
        // Resource utilization metrics
        let resources = &saturation.resource_utilization;
        
        metrics.insert("cpu_utilization".to_string(),
                      MetricValue::Float(resources.cpu_utilization));
                      
        metrics.insert("memory_utilization".to_string(),
                      MetricValue::Float(resources.memory_utilization));
                      
        metrics.insert("connection_pool_utilization".to_string(),
                      MetricValue::Float(resources.connection_pool_utilization));
                      
        metrics.insert("task_queue_depth".to_string(),
                      MetricValue::Counter(resources.task_queue_depth));
                      
        metrics.insert("network_queue_depth".to_string(),
                      MetricValue::Counter(resources.network_queue_depth));
                      
        metrics.insert("backpressure_wait_ms".to_string(),
                      MetricValue::Float(resources.backpressure_wait_ms));
        
        ComponentMetrics {
            component_type: "ControlSignal".to_string(),
            component_id: self.id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            metrics,
            status: Some(format!("Saturation Signal: Level={:?}", saturation.level)),
        }
    }
    
    fn get_component_type(&self) -> &str {
        "ControlSignal"
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}
```

### 6.3 Saturation-Aware Controller with Metrics

```rust
/// Adapter that adjusts a rate controller based on saturation
pub struct SaturationRateAdapter {
    /// Controller ID
    id: String,
    
    /// Underlying controller
    controller: Arc<dyn RateController>,
    
    /// Saturation signal
    saturation_signal: Arc<dyn ControlSignal>,
    
    /// Threshold for moderate saturation
    moderate_threshold: f64,
    
    /// Threshold for severe saturation
    severe_threshold: f64,
    
    /// Rate adjustments due to saturation
    adjustments: Arc<Mutex<Vec<(u64, RateAdjustment)>>>,
    
    /// Start time
    start_time: Instant,
    
    /// Metrics collection
    metrics: Arc<RateControllerMetrics>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl SaturationRateAdapter {
    /// Create a new saturation rate adapter
    pub fn new(
        id: String,
        controller: Arc<dyn RateController>,
        saturation_signal: Arc<dyn ControlSignal>,
        initial_rps: u64,
        min_rps: u64,
        max_rps: u64,
        moderate_threshold: f64,
        severe_threshold: f64,
    ) -> Self {
        // Create metrics
        let metrics = Arc::new(RateControllerMetrics::new(
            id.clone(), initial_rps, min_rps, max_rps
        ));
        
        Self {
            id,
            controller,
            saturation_signal,
            moderate_threshold,
            severe_threshold,
            adjustments: Arc::new(Mutex::new(Vec::new())),
            start_time: Instant::now(),
            metrics,
            registry_handle: Mutex::new(None),
        }
    }

    fn record_adjustment(&self, new_rps: u64, old_rps: u64, context: String) {
        // Record adjustment
        let now = self.start_time.elapsed().as_millis() as u64;
        let adjustment = RateAdjustment {
            new_rps,
            old_rps,
            reason: AdjustmentReason::Saturation,
            context,
        };
        
        // Update metrics
        self.metrics.update_rps(new_rps, &context);
        
        let mut adjustments = self.adjustments.lock().unwrap();
        adjustments.push((now, adjustment));
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

impl RateController for SaturationRateAdapter {
    fn get_current_rps(&self) -> u64 {
        // Base RPS from underlying controller
        let base_rps = self.controller.get_current_rps();
        
        // Check saturation and adjust if needed
        let saturation = self.saturation_signal.get_current_value();
        
        if saturation > self.severe_threshold {
            // Severe saturation - reduce by 30%
            let new_rps = (base_rps as f64 * 0.7) as u64;
            
            // Only record if different from current
            if new_rps != base_rps {
                self.record_adjustment(new_rps, base_rps, 
                    format!("Severe saturation detected ({})", saturation));
            }
            
            return new_rps;
        } else if saturation > self.moderate_threshold {
            // Moderate saturation - reduce by 10%
            let new_rps = (base_rps as f64 * 0.9) as u64;
            
            // Only record if different from current
            if new_rps != base_rps {
                self.record_adjustment(new_rps, base_rps,
                    format!("Moderate saturation detected ({})", saturation));
            }
            
            return new_rps;
        }
        
        // No saturation adjustment needed
        base_rps
    }
    
    fn update(&self, metrics: &RequestMetrics) {
        // Forward to underlying controller
        self.controller.update(metrics);
    }
    
    fn process_control_signals(&self, signals: &ControlSignalSet) {
        // Forward to underlying controller
        self.controller.process_control_signals(signals);
    }
    
    fn get_rate_history(&self) -> Vec<(u64, u64)> {
        // Combine underlying controller history with our saturation adjustments
        let mut history = self.controller.get_rate_history();
        
        // Add saturation adjustments where they don't already exist
        for (time, adjustment) in self.adjustments.lock().unwrap().iter() {
            // Check if this timestamp already exists
            let exists = history.iter().any(|(t, _)| *t == *time);
            
            if !exists {
                history.push((*time, adjustment.new_rps));
            }
        }
        
        // Sort by timestamp
        history.sort_by_key(|(t, _)| *t);
        
        history
    }
    
    fn get_rate_adjustments(&self) -> Vec<(u64, RateAdjustment)> {
        // Combine underlying controller adjustments with our saturation adjustments
        let mut result = self.controller.get_rate_adjustments();
        let saturation_adjustments = self.adjustments.lock().unwrap().clone();
        result.extend(saturation_adjustments.into_iter());
        
        // Sort by timestamp
        result.sort_by_key(|(t, _)| *t);
        
        result
    }
}

impl MetricsProvider for SaturationRateAdapter {
    fn get_metrics(&self) -> ComponentMetrics {
        ComponentMetrics {
            component_type: "RateController".to_string(),
            component_id: self.id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            metrics: self.metrics.get_snapshot(),
            status: Some(format!("Saturation-Aware Controller ({}): Moderate={:.2}, Severe={:.2}",
                       self.id, self.moderate_threshold, self.severe_threshold)),
        }
    }
    
    fn get_component_type(&self) -> &str {
        "RateController"
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}
```

## 7. PID Controller Implementation with Metrics

```rust
/// PID controller for rate adjustment
pub struct PidRateController {
    /// Controller ID
    id: String,
    
    /// Config parameters
    config: Arc<LoadTestConfig>,
    
    /// Metrics collection
    metrics: Arc<RateControllerMetrics>,
    
    /// PID parameters
    kp: f64,
    ki: f64,
    kd: f64,
    
    /// Target metric value
    target_metric: f64,
    
    /// Metric type
    metric_type: MetricType,
    
    /// Update interval
    update_interval_ms: u64,
    
    /// Integral term
    integral: Mutex<f64>,
    
    /// Last error
    last_error: Mutex<f64>,
    
    /// Rate adjustments
    adjustments: Mutex<Vec<(u64, RateAdjustment)>>,
    
    /// Start time
    start_time: Instant,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl PidRateController {
    /// Create a new PID controller
    pub fn new(
        id: String,
        initial_rps: u64,
        min_rps: u64,
        max_rps: u64,
        rate_control_mode: RateControlMode,
    ) -> Self {
        // Extract PID parameters from rate control mode
        let (target_metric, metric_type, kp, ki, kd, update_interval_ms) = match &rate_control_mode {
            RateControlMode::Pid { 
                target_metric, metric_type, kp, ki, kd, update_interval_ms
            } => (
                *target_metric, *metric_type, *kp, *ki, *kd, *update_interval_ms
            ),
            // Default values for other modes
            _ => (100.0, MetricType::LatencyP99, 0.1, 0.01, 0.001, 500),
        };
        
        // Create metrics
        let metrics = Arc::new(RateControllerMetrics::new(
            id.clone(), initial_rps, min_rps, max_rps
        ));
        
        // Initialize controller
        let controller = Self {
            id,
            config: Arc::new(LoadTestConfig {
                initial_rps,
                min_rps,
                max_rps,
                rate_control_mode,
                // Other config fields...
                ..Default::default()
            }),
            metrics,
            kp,
            ki,
            kd,
            target_metric,
            metric_type,
            update_interval_ms,
            integral: Mutex::new(0.0),
            last_error: Mutex::new(0.0),
            adjustments: Mutex::new(Vec::new()),
            start_time: Instant::now(),
            registry_handle: Mutex::new(None),
        };
        
        // Log initial adjustment
        let initial_adjustment = RateAdjustment {
            new_rps: initial_rps,
            old_rps: initial_rps,
            reason: AdjustmentReason::Initial,
            context: "Initial PID controller setup".to_string(),
        };
        
        let mut adjustments = controller.adjustments.lock().unwrap();
        adjustments.push((0, initial_adjustment));
        
        // Update metrics
        controller.metrics.update_rps(initial_rps, "Initial setup");
        
        controller
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

impl RateController for PidRateController {
    fn get_current_rps(&self) -> u64 {
        self.metrics.current_rps.load(Ordering::Relaxed)
    }

    fn update(&self, metrics: &RequestMetrics) {
        if metrics.window_size_ms < self.update_interval_ms {
            // Wait for enough data
            return;
        }
        
        // Get current metric value
        let current_value = match self.metric_type {
            MetricType::LatencyP50 => metrics.p50_latency_us as f64,
            MetricType::LatencyP90 => metrics.p90_latency_us as f64,
            MetricType::LatencyP99 => metrics.p99_latency_us as f64,
            MetricType::ErrorRate => metrics.error_rate,
            MetricType::ExternalSignal => {
                // External signal is handled in process_control_signals
                return;
            }
            _ => metrics.p99_latency_us as f64,
        };
        
        // Calculate PID terms
        let error = self.target_metric - current_value;
        
        // Update integral term
        let mut integral = self.integral.lock().unwrap();
        *integral += error * (metrics.window_size_ms as f64 / 1000.0);
        
        // Apply anti-windup by clamping integral term
        let max_integral = (self.config.max_rps - self.config.min_rps) as f64 / self.ki;
        *integral = integral.clamp(-max_integral, max_integral);
        
        // Calculate derivative term
        let derivative = {
            let mut last_error = self.last_error.lock().unwrap();
            let derivative = (error - *last_error) / (metrics.window_size_ms as f64 / 1000.0);
            *last_error = error;
            derivative
        };
        
        // Calculate PID output
        let p_term = self.kp * error;
        let i_term = self.ki * *integral;
        let d_term = self.kd * derivative;
        let mut output = p_term + i_term + d_term;
        
        // Update metrics for PID terms
        self.metrics.update_pid_metrics(error, *integral, derivative);
        
        // Handle error rate specially (inverse relationship)
        if self.metric_type == MetricType::ErrorRate {
            output = -output;  // Invert - higher error rate means lower RPS
        }
        
        // Get current RPS
        let current_rps = self.metrics.current_rps.load(Ordering::Relaxed);
        
        // Calculate new RPS
        let new_rps = (current_rps as f64 + output).round() as u64;
        let new_rps = new_rps.clamp(self.config.min_rps, self.config.max_rps);
        
        // Skip update if minimal change
        if (new_rps as i64 - current_rps as i64).abs() < 2 {
            return;
        }
        
        // Create context string
        let context = format!(
            "Target: {:.2}, Current: {:.2}, Error: {:.2}, P: {:.2}, I: {:.2}, D: {:.2}",
            self.target_metric, current_value, error, p_term, i_term, d_term
        );
        
        // Update metrics with new RPS
        self.metrics.update_rps(new_rps, &context);
        
        // Record adjustment
        let now = self.start_time.elapsed().as_millis() as u64;
        let adjustment = RateAdjustment {
            new_rps,
            old_rps: current_rps,
            reason: AdjustmentReason::PidControl,
            context,
        };
        
        // Record adjustment
        let mut adjustments = self.adjustments.lock().unwrap();
        adjustments.push((now, adjustment));
    }

    fn process_control_signals(&self, signals: &ControlSignalSet) {
        // Only process if we're using external signal
        if self.metric_type != MetricType::ExternalSignal {
            return;
        }
        
        // Get relevant signal from original PID config
        if let RateControlMode::ExternalSignalPid { signal_name, .. } = &self.config.rate_control_mode {
            if let Some(signal) = signals.get_signal(signal_name) {
                let current_value = signal.get_current_value();
                
                // Calculate PID terms
                let error = self.target_metric - current_value;
                
                // Update integral term
                let mut integral = self.integral.lock().unwrap();
                *integral += error * 0.1; // Simplified time factor
                
                // Apply anti-windup by clamping integral term
                let max_integral = (self.config.max_rps - self.config.min_rps) as f64 / self.ki;
                *integral = integral.clamp(-max_integral, max_integral);
                
                // Calculate derivative term
                let derivative = {
                    let mut last_error = self.last_error.lock().unwrap();
                    let derivative = (error - *last_error) / 0.1; // Simplified time factor
                    *last_error = error;
                    derivative
                };
                
                // Calculate PID output
                let p_term = self.kp * error;
                let i_term = self.ki * *integral;
                let d_term = self.kd * derivative;
                let output = p_term + i_term + d_term;
                
                // Update metrics for PID terms
                self.metrics.update_pid_metrics(error, *integral, derivative);
                
                // Get current RPS
                let current_rps = self.metrics.current_rps.load(Ordering::Relaxed);
                
                // Calculate new RPS
                let new_rps = (current_rps as f64 + output).round() as u64;
                let new_rps = new_rps.clamp(self.config.min_rps, self.config.max_rps);
                
                // Create context string
                let context = format!(
                    "Signal: {}, Value: {:.2}, Target: {:.2}, Error: {:.2}",
                    signal_name, current_value, self.target_metric, error
                );
                
                // Update metrics with new RPS
                self.metrics.update_rps(new_rps, &context);
                
                // Record adjustment
                let now = self.start_time.elapsed().as_millis() as u64;
                let adjustment = RateAdjustment {
                    new_rps,
                    old_rps: current_rps,
                    reason: AdjustmentReason::ExternalSignal,
                    context,
                };
                
                // Record adjustment
                let mut adjustments = self.adjustments.lock().unwrap();
                adjustments.push((now, adjustment));
            }
        }
    }

    fn get_rate_history(&self) -> Vec<(u64, u64)> {
        // Extract from adjustments
        let adjustments = self.adjustments.lock().unwrap();
        adjustments.iter()
            .map(|(time, adjustment)| (*time, adjustment.new_rps))
            .collect()
    }

    fn get_rate_adjustments(&self) -> Vec<(u64, RateAdjustment)> {
        let adjustments = self.adjustments.lock().unwrap();
        adjustments.clone()
    }
}

impl MetricsProvider for PidRateController {
    fn get_metrics(&self) -> ComponentMetrics {
        ComponentMetrics {
            component_type: "RateController".to_string(),
            component_id: self.id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            metrics: self.metrics.get_snapshot(),
            status: Some(format!("PID Controller: Target={} {}", 
                     self.target_metric, 
                     match self.metric_type {
                         MetricType::LatencyP50 => "p50 latency",
                         MetricType::LatencyP90 => "p90 latency",
                         MetricType::LatencyP99 => "p99 latency",
                         MetricType::ErrorRate => "error rate",
                         MetricType::ExternalSignal => "external signal",
                         _ => "unknown metric",
                     })),
        }
    }
    
    fn get_component_type(&self) -> &str {
        "RateController"
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}
```

## 8. Distributed Load Control with Metrics

### 8.1 Distributed Coordinator Interface

```rust
/// Interface for coordinating distributed load testing
pub trait DistributedCoordinator: Send + Sync {
    /// Initialize coordination with other nodes
    async fn initialize(&mut self) -> Result<(), Box<dyn std::error::Error>>;

    /// Register this node with the coordinator
    async fn register_node(&mut self, node_info: NodeInfo) -> Result<NodeAssignment, Box<dyn std::error::Error>>;

    /// Report node status including saturation metrics
    async fn report_node_status(&self, status: NodeStatus) -> Result<(), Box<dyn std::error::Error>>;

    /// Get global test status and control signals
    async fn get_global_status(&self) -> Result<GlobalTestStatus, Box<dyn std::error::Error>>;

    /// Synchronize test start across all nodes
    async fn sync_test_start(&self) -> Result<Instant, Box<dyn std::error::Error>>;

    /// Report final node results
    async fn report_final_results(&self, results: NodeResults) -> Result<(), Box<dyn std::error::Error>>;

    /// Shutdown coordination
    async fn shutdown(&self) -> Result<(), Box<dyn std::error::Error>>;
}

/// Node information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    /// Unique identifier for this node
    pub node_id: String,
    /// Network capabilities
    pub capabilities: NodeCapabilities,
    /// Maximum RPS this node can generate
    pub max_rps: u64,
    /// Maximum concurrent connections
    pub max_concurrent: usize,
}

/// Node assignment from coordinator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeAssignment {
    /// Target RPS for this node
    pub target_rps: u64,
    /// Assigned test parameters
    pub test_params: TestParameters,
    /// URL patterns to test (may be a subset)
    pub url_patterns: Vec<String>,
    /// Whether this node should collect detailed samples
    pub detailed_sampling: bool,
}

/// Node status report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatus {
    /// Current RPS being generated
    pub current_rps: u64,
    /// Current saturation metrics
    pub saturation: SaturationMetrics,
    /// Success rate (0.0-1.0)
    pub success_rate: f64,
    /// Current P99 latency in milliseconds
    pub p99_latency_ms: f64,
}

/// Global test status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalTestStatus {
    /// Total RPS across all nodes
    pub total_rps: u64,
    /// New target RPS for this node
    pub new_target_rps: Option<u64>,
    /// Control signals
    pub control_signals: HashMap<String, f64>,
    /// Global actions to take
    pub actions: Vec<GlobalAction>,
}

/// Global action from coordinator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GlobalAction {
    /// Update test parameters
    UpdateParams(TestParameters),
    /// Adjust RPS by factor
    AdjustRps(f64),
    /// Redistribute load (rebalance across nodes)
    RedistributeLoad,
    /// Terminate test
    TerminateTest,
}
```

### 8.2 Distributed Rate Control Signal

```rust
/// Control signal sent from coordinator to worker nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedControlSignal {
    /// Load proportion factor (0.0-2.0)
    /// - 1.0 means take your even share of the load
    /// - <1.0 means take less than your even share
    /// - >1.0 means take more than your even share
    pub load_proportion_factor: f64,
    
    /// Target metric for the local PID controller
    pub target_metric_value: f64,
    
    /// Coordinator's assessment of this client's saturation (0.0-1.0)
    pub saturation_assessment: f64,
    
    /// Maximum rate of change per second (relative)
    /// e.g., 0.1 means max 10% change per second
    pub max_ramp_rate: f64,
    
    /// Absolute maximum RPS this client should not exceed
    pub absolute_max_rps: Option<u64>,
    
    /// Suggested starting RPS (optional)
    pub suggested_start_rps: Option<u64>,
    
    /// Timestamp from coordinator
    pub timestamp: u64,
}
```

### 8.3 Load Proportion Smoother with Metrics

```rust
/// Metrics for load proportion smoother
pub struct LoadProportionMetrics {
    /// Component ID
    id: String,
    
    /// Current proportion factor
    current_factor: AtomicF64,
    
    /// Target proportion factor
    target_factor: AtomicF64,
    
    /// Maximum change rate (proportion per second)
    max_ramp_rate: AtomicF64,
    
    /// Factor adjustment count
    adjustment_count: AtomicU64,
    
    /// Last update timestamp
    last_update: AtomicU64,
    
    /// History of factor changes
    /// Protected with RwLock but updated infrequently
    factor_history: Arc<RwLock<VecDeque<(u64, f64, String)>>>,
}

impl LoadProportionMetrics {
    /// Create new load proportion metrics
    pub fn new(id: String, initial_factor: f64, max_ramp_rate: f64) -> Self {
        Self {
            id,
            current_factor: AtomicF64::new(initial_factor),
            target_factor: AtomicF64::new(initial_factor),
            max_ramp_rate: AtomicF64::new(max_ramp_rate),
            adjustment_count: AtomicU64::new(0),
            last_update: AtomicU64::new(current_timestamp()),
            factor_history: Arc::new(RwLock::new(VecDeque::with_capacity(100))),
        }
    }
    
    /// Update target factor
    pub fn update_target_factor(&self, new_target: f64, reason: &str) {
        self.target_factor.store(new_target, Ordering::Relaxed);
        
        // Record in history
        let now = current_timestamp();
        let reason_str = reason.to_string();
        
        if let Ok(mut history) = self.factor_history.write() {
            history.push_back((now, new_target, reason_str));
            
            // Keep bounded size
            while history.len() > 100 {
                history.pop_front();
            }
        }
        
        self.last_update.store(now, Ordering::Relaxed);
    }
    
    /// Update current factor
    pub fn update_current_factor(&self, new_factor: f64) {
        self.current_factor.store(new_factor, Ordering::Relaxed);
        self.adjustment_count.fetch_add(1, Ordering::Relaxed);
    }
    
    /// Update ramp rate
    pub fn update_ramp_rate(&self, new_rate: f64) {
        self.max_ramp_rate.store(new_rate, Ordering::Relaxed);
    }
    
    /// Get a snapshot of metrics for registry
    pub fn get_snapshot(&self) -> HashMap<String, MetricValue> {
        let mut metrics = HashMap::new();
        
        // Add atomic values
        metrics.insert("current_factor".to_string(), 
                      MetricValue::Float(self.current_factor.load(Ordering::Relaxed)));
                      
        metrics.insert("target_factor".to_string(),
                      MetricValue::Float(self.target_factor.load(Ordering::Relaxed)));
                      
        metrics.insert("max_ramp_rate".to_string(),
                      MetricValue::Float(self.max_ramp_rate.load(Ordering::Relaxed)));
                      
        metrics.insert("adjustment_count".to_string(),
                      MetricValue::Counter(self.adjustment_count.load(Ordering::Relaxed)));
                      
        metrics.insert("last_update".to_string(),
                      MetricValue::Counter(self.last_update.load(Ordering::Relaxed)));
        
        // Add recent history as a text metric
        if let Ok(history) = self.factor_history.read() {
            // Convert the last 3 adjustments to a compact string
            let recent_history = history.iter()
                .rev()
                .take(3)
                .map(|(ts, factor, reason)| format!("{}:{:.2}:{}", ts, factor, reason))
                .collect::<Vec<_>>()
                .join("; ");
                
            metrics.insert("recent_adjustments".to_string(),
                          MetricValue::Text(recent_history));
        }
        
        metrics
    }
}

/// Load proportion smoother for gradual transitions
pub struct LoadProportionSmoother {
    /// Component ID
    id: String,
    
    /// Metrics collection
    metrics: Arc<LoadProportionMetrics>,
    
    /// Last update time
    last_update: Mutex<Instant>,
    
    /// Start time
    start_time: Instant,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl LoadProportionSmoother {
    /// Create a new load proportion smoother
    pub fn new(id: String, initial_factor: f64, max_ramp_rate: f64) -> Self {
        // Create metrics
        let metrics = Arc::new(LoadProportionMetrics::new(
            id.clone(), initial_factor, max_ramp_rate
        ));
        
        Self {
            id,
            metrics,
            last_update: Mutex::new(Instant::now()),
            start_time: Instant::now(),
            registry_handle: Mutex::new(None),
        }
    }
    
    /// Set a new target proportion factor
    pub fn set_target_factor(&self, new_target: f64, new_ramp_rate: Option<f64>, reason: &str) {
        // Update metrics
        self.metrics.update_target_factor(new_target, reason);
        
        if let Some(ramp_rate) = new_ramp_rate {
            self.metrics.update_ramp_rate(ramp_rate);
        }
    }
    
    /// Get current smoothed factor, updating it based on elapsed time
    pub fn get_current_factor(&self) -> f64 {
        let target = self.metrics.target_factor.load(Ordering::Relaxed);
        let current = self.metrics.current_factor.load(Ordering::Relaxed);
        
        if (current - target).abs() < 0.001 {
            // Already close enough to target
            return current;
        }
        
        // Calculate time since last update
        let mut last_update = self.last_update.lock().unwrap();
        let elapsed_secs = last_update.elapsed().as_secs_f64();
        *last_update = Instant::now();
        
        // Calculate maximum change for this time period
        let max_ramp_rate = self.metrics.max_ramp_rate.load(Ordering::Relaxed);
        let max_change = max_ramp_rate * elapsed_secs;
        
        // Calculate new value with limited change rate
        let change = target - current;
        let limited_change = change.signum() * change.abs().min(max_change);
        let new_value = current + limited_change;
        
        // Update current factor in metrics
        self.metrics.update_current_factor(new_value);
        
        new_value
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

impl MetricsProvider for LoadProportionSmoother {
    fn get_metrics(&self) -> ComponentMetrics {
        ComponentMetrics {
            component_type: "LoadProportionSmoother".to_string(),
            component_id: self.id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            metrics: self.metrics.get_snapshot(),
            status: Some(format!("Load Proportion Smoother: Current={:.2}, Target={:.2}, MaxRamp={:.2}/s",
                      self.metrics.current_factor.load(Ordering::Relaxed),
                      self.metrics.target_factor.load(Ordering::Relaxed),
                      self.metrics.max_ramp_rate.load(Ordering::Relaxed))),
        }
    }
    
    fn get_component_type(&self) -> &str {
        "LoadProportionSmoother"
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}
```

### 8.4 Distributed Rate Adapter with Metrics

```rust
/// Distributed rate controller adapter
pub struct DistributedRateAdapter {
    /// Controller ID
    id: String,
    
    /// Underlying local controller
    local_controller: Arc<dyn RateController>,
    
    /// Load proportion smoother
    load_smoother: Arc<LoadProportionSmoother>,
    
    /// Target metric updater
    target_updater: Arc<AtomicF64>,
    
    /// Coordinator for communication
    coordinator: Arc<dyn DistributedCoordinator>,
    
    /// Node status
    node_status: Arc<RwLock<NodeStatus>>,
    
    /// Rate adjustment history
    adjustments: Mutex<Vec<(u64, RateAdjustment)>>,
    
    /// Start time
    start_time: Instant,
    
    /// Metrics collection
    metrics: Arc<RateControllerMetrics>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl DistributedRateAdapter {
    /// Create a new distributed rate adapter
    pub fn new(
        id: String,
        local_controller: Arc<dyn RateController>,
        coordinator: Arc<dyn DistributedCoordinator>,
        initial_rps: u64,
        min_rps: u64,
        max_rps: u64,
        initial_proportion: f64,
        max_ramp_rate: f64,
    ) -> Self {
        // Create metrics
        let metrics = Arc::new(RateControllerMetrics::new(
            id.clone(), initial_rps, min_rps, max_rps
        ));
        
        // Record initial distributed proportion
        metrics.update_distributed_metrics(initial_proportion);
        
        Self {
            id,
            local_controller,
            load_smoother: Arc::new(LoadProportionSmoother::new(
                format!("{}-proportion", id),
                initial_proportion,
                max_ramp_rate,
            )),
            target_updater: Arc::new(AtomicF64::new(100.0)), // Default target metric value
            coordinator,
            node_status: Arc::new(RwLock::new(NodeStatus::default())),
            adjustments: Mutex::new(Vec::new()),
            start_time: Instant::now(),
            metrics,
            registry_handle: Mutex::new(None),
        }
    }
    
    /// Start background task to update from coordinator
    pub fn start_coordination(&self) -> JoinHandle<()> {
        let coordinator = self.coordinator.clone();
        let node_status = self.node_status.clone();
        let load_smoother = self.load_smoother.clone();
        let target_updater = self.target_updater.clone();
        let metrics = self.metrics.clone();
        let adjustments = self.adjustments.clone();
        let start_time = self.start_time;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            
            loop {
                interval.tick().await;
                
                // Get global status from coordinator
                match coordinator.get_global_status().await {
                    Ok(status) => {
                        // Process control signals
                        if let Some(new_target_rps) = status.new_target_rps {
                            // Calculate factor based on assigned RPS
                            let current_rps = {
                                let status = node_status.read().unwrap();
                                status.current_rps
                            };
                            
                            if current_rps > 0 {
                                let proportion = new_target_rps as f64 / current_rps as f64;
                                load_smoother.set_target_factor(proportion, None, "Coordinator assigned RPS");
                                
                                // Record in metrics
                                metrics.update_distributed_metrics(proportion);
                                
                                // Record adjustment
                                let now = start_time.elapsed().as_millis() as u64;
                                let mut adj = adjustments.lock().unwrap();
                                adj.push((now, RateAdjustment {
                                    new_rps: new_target_rps,
                                    old_rps: current_rps,
                                    reason: AdjustmentReason::DistributedControl,
                                    context: format!("Coordinator assigned new target RPS: {}", new_target_rps),
                                }));
                            }
                        }
                        
                        // Process actions
                        for action in &status.actions {
                            match action {
                                GlobalAction::AdjustRps(factor) => {
                                    load_smoother.set_target_factor(*factor, None, "Coordinator RPS adjustment");
                                    
                                    // Record in metrics
                                    metrics.update_distributed_metrics(*factor);
                                    
                                    // Record adjustment
                                    let now = start_time.elapsed().as_millis() as u64;
                                    let current_rps = {
                                        let status = node_status.read().unwrap();
                                        status.current_rps
                                    };
                                    let new_rps = (current_rps as f64 * factor) as u64;
                                    
                                    let mut adj = adjustments.lock().unwrap();
                                    adj.push((now, RateAdjustment {
                                        new_rps,
                                        old_rps: current_rps,
                                        reason: AdjustmentReason::DistributedControl,
                                        context: format!("Coordinator adjustment factor: {:.2}", factor),
                                    }));
                                },
                                GlobalAction::RedistributeLoad => {
                                    // This will be handled by the new_target_rps field
                                    let mut adj = adjustments.lock().unwrap();
                                    let now = start_time.elapsed().as_millis() as u64;
                                    adj.push((now, RateAdjustment {
                                        new_rps: 0, // Placeholder, actual value set by new_target_rps
                                        old_rps: 0,
                                        reason: AdjustmentReason::DistributedControl,
                                        context: "Load redistribution initiated".to_string(),
                                    }));
                                    
                                    // Record rebalance
                                    metrics.rebalance_count.fetch_add(1, Ordering::Relaxed);
                                },
                                _ => {
                                    // Other actions handled elsewhere
                                }
                            }
                        }
                        
                        // Update target metric if present in control signals
                        if let Some(target) = status.control_signals.get("target_latency_ms") {
                            target_updater.store(*target, Ordering::Relaxed);
                        }
                    },
                    Err(e) => {
                        // Log error but continue
                        eprintln!("Error getting global status: {}", e);
                    }
                }
                
                // Report node status back to coordinator
                let status = node_status.read().unwrap().clone();
                let _ = coordinator.report_node_status(status).await;
            }
        })
    }
    
    /// Update node status
    pub fn update_status(&self, metrics: &RequestMetrics, saturation: Option<SaturationMetrics>) {
        let mut status = self.node_status.write().unwrap();
        
        // Update from metrics
        status.current_rps = metrics.current_throughput.round() as u64;
        status.p99_latency_ms = metrics.p99_latency_us as f64 / 1000.0;
        status.success_rate = 1.0 - metrics.error_rate;
        
        // Update from saturation if provided
        if let Some(sat) = saturation {
            status.saturation = sat;
        }
    }
    
    /// Register with metrics registry
    pub fn register_with_registry(&self, registry: Arc<dyn MetricsRegistry>) -> RegistrationHandle {
        // Register the load smoother first
        self.load_smoother.register_with_registry(registry.clone());
        
        // Register this controller
        let provider = Arc::new(self.clone()) as Arc<dyn MetricsProvider>;
        let handle = registry.register_provider(provider);
        
        let mut registry_handle = self.registry_handle.lock().unwrap();
        *registry_handle = Some(handle.clone());
        
        handle
    }
}

impl RateController for DistributedRateAdapter {
    fn get_current_rps(&self) -> u64 {
        // Get base RPS from local controller
        let base_rps = self.local_controller.get_current_rps();
        
        // Apply load proportion factor with smoothing
        let proportion = self.load_smoother.get_current_factor();
        
        // Calculate adjusted RPS
        let adjusted_rps = (base_rps as f64 * proportion).round() as u64;
        
        // Update metrics
        let current_proportion = self.metrics.distributed_proportion_factor.load(Ordering::Relaxed);
        if (proportion - current_proportion).abs() > 0.01 {
            self.metrics.update_distributed_metrics(proportion);
        }
        
        adjusted_rps
    }
    
    fn update(&self, metrics: &RequestMetrics) {
        // Update local controller
        self.local_controller.update(metrics);
        
        // Update node status
        self.update_status(metrics, None);
    }
    
    fn process_control_signals(&self, signals: &ControlSignalSet) {
        // Update local controller
        self.local_controller.process_control_signals(signals);
        
        // Check for saturation signal
        if let Some(saturation) = signals.get_signal("saturation") {
            let saturation_value = saturation.get_current_value();
            let node_saturation = match saturation_value {
                v if v < 0.1 => SaturationLevel::None,
                v if v < 0.4 => SaturationLevel::Mild,
                v if v < 0.7 => SaturationLevel::Moderate,
                _ => SaturationLevel::Severe,
            };
            
            // Update node status with saturation
            let mut status = self.node_status.write().unwrap();
            status.saturation.level = node_saturation;
        }
    }
    
    fn get_rate_history(&self) -> Vec<(u64, u64)> {
        // Get history from local controller
        let mut history = self.local_controller.get_rate_history();
        
        // Add distributed adjustments where they don't already exist
        for (time, adjustment) in self.adjustments.lock().unwrap().iter() {
            // Check if this timestamp already exists
            let exists = history.iter().any(|(t, _)| *t == *time);
            
            if !exists && adjustment.new_rps > 0 {
                history.push((*time, adjustment.new_rps));
            }
        }
        
        // Sort by timestamp
        history.sort_by_key(|(t, _)| *t);
        
        history
    }
    
    fn get_rate_adjustments(&self) -> Vec<(u64, RateAdjustment)> {
        // Combine local and distributed adjustments
        let mut result = self.local_controller.get_rate_adjustments();
        let distributed_adjustments = self.adjustments.lock().unwrap().clone();
        result.extend(distributed_adjustments.into_iter());
        
        // Sort by timestamp
        result.sort_by_key(|(t, _)| *t);
        
        result
    }
}

impl MetricsProvider for DistributedRateAdapter {
    fn get_metrics(&self) -> ComponentMetrics {
        ComponentMetrics {
            component_type: "RateController".to_string(),
            component_id: self.id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            metrics: self.metrics.get_snapshot(),
            status: Some(format!("Distributed Rate Controller ({}): Factor={:.2}",
                       self.id,
                       self.metrics.distributed_proportion_factor.load(Ordering::Relaxed))),
        }
    }
    
    fn get_component_type(&self) -> &str {
        "RateController"
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}
```

## 9. Rate Controller Factory and Builder Pattern

### 9.1 Rate Controller Factory with Registry Integration

```rust
/// Factory for creating rate controllers
pub struct RateControllerFactory;

impl RateControllerFactory {
    /// Create a rate controller based on configuration
    pub fn create_controller(
        config: &LoadTestConfig,
        metrics_registry: Option<Arc<dyn MetricsRegistry>>,
        profiler: Option<Arc<dyn TransactionProfiler>>,
    ) -> Arc<dyn RateController> {
        // Generate a unique ID for this controller
        let id = format!("controller-{}", Uuid::new_v4());
        
        // Create base controller
        let raw_controller: Arc<dyn RateController> = match &config.rate_control_mode {
            RateControlMode::Static => {
                let controller = Arc::new(StaticRateController::new(
                    id.clone(),
                    config.initial_rps,
                    config.min_rps,
                    config.max_rps,
                ));
                
                if let Some(registry) = &metrics_registry {
                    controller.register_with_registry(registry.clone());
                }
                
                controller
            },
            RateControlMode::Pid { .. } => {
                let controller = Arc::new(PidRateController::new(
                    id.clone(),
                    config.initial_rps,
                    config.min_rps,
                    config.max_rps,
                    config.rate_control_mode.clone(),
                ));
                
                if let Some(registry) = &metrics_registry {
                    controller.register_with_registry(registry.clone());
                }
                
                controller
            },
            RateControlMode::ExternalSignalPid { .. } => {
                let controller = Arc::new(PidRateController::new(
                    id.clone(),
                    config.initial_rps,
                    config.min_rps,
                    config.max_rps,
                    config.rate_control_mode.clone(),
                ));
                
                if let Some(registry) = &metrics_registry {
                    controller.register_with_registry(registry.clone());
                }
                
                controller
            },
            RateControlMode::SaturationAwarePid { .. } => {
                // Need profiler for saturation awareness
                if let Some(prof) = &profiler {
                    // Create signal set
                    let signal_set = Arc::new(ControlSignalSet::new());
                    
                    // Create saturation signal
                    let saturation_signal = Arc::new(SaturationSignalProvider::new(
                        format!("{}-saturation", id),
                        prof.clone(),
                        Duration::from_millis(500),
                    ));
                    
                    // Register with registry if provided
                    if let Some(registry) = &metrics_registry {
                        saturation_signal.register_with_registry(registry.clone());
                    }
                    
                    // Start monitoring
                    saturation_signal.start_monitoring();
                    
                    // Register signal
                    signal_set.register_signal("saturation", saturation_signal.clone());
                    
                    // Create base PID controller
                    let pid_controller = Arc::new(PidRateController::new(
                        format!("{}-pid", id),
                        config.initial_rps,
                        config.min_rps,
                        config.max_rps,
                        config.rate_control_mode.clone(),
                    ));
                    
                    if let Some(registry) = &metrics_registry {
                        pid_controller.register_with_registry(registry.clone());
                    }
                    
                    // Extract saturation threshold
                    let saturation_threshold = if let RateControlMode::SaturationAwarePid { saturation_threshold, .. } = config.rate_control_mode {
                        saturation_threshold
                    } else {
                        0.5 // Default
                    };
                    
                    // Create saturation adapter
                    let adapter = Arc::new(SaturationRateAdapter::new(
                        id.clone(),
                        pid_controller,
                        saturation_signal,
                        config.initial_rps,
                        config.min_rps,
                        config.max_rps,
                        0.2, // Moderate threshold
                        saturation_threshold, // Severe threshold
                    ));
                    
                    if let Some(registry) = &metrics_registry {
                        adapter.register_with_registry(registry.clone());
                    }
                    
                    adapter
                } else {
                    // Fall back to regular PID if no profiler
                    let controller = Arc::new(PidRateController::new(
                        id.clone(),
                        config.initial_rps,
                        config.min_rps,
                        config.max_rps,
                        config.rate_control_mode.clone(),
                    ));
                    
                    if let Some(registry) = &metrics_registry {
                        controller.register_with_registry(registry.clone());
                    }
                    
                    controller
                }
            },
            RateControlMode::StepFunction(steps) => {
                let controller = Arc::new(StepFunctionController::new(
                    id.clone(),
                    config.initial_rps,
                    config.min_rps,
                    config.max_rps,
                    steps.clone(),
                ));
                
                if let Some(registry) = &metrics_registry {
                    controller.register_with_registry(registry.clone());
                }
                
                controller
            },
            RateControlMode::Custom => {
                panic!("Custom controller must be provided directly")
            },
        };
        
        // Apply rate smoothing if enabled
        let smoothed_controller = if config.rate_smoothing_config.enabled {
            let smoothed_id = format!("smoothed-{}", id);
            let controller = Arc::new(SmoothedRateController::new(
                smoothed_id,
                raw_controller,
                config.initial_rps,
                config.min_rps,
                config.max_rps,
                config.rate_smoothing_config.clone(),
            ));
            
            if let Some(registry) = &metrics_registry {
                controller.register_with_registry(registry.clone());
            }
            
            controller
        } else {
            raw_controller
        };
        
        // Apply distributed adapter if enabled
        if let Some(dist_config) = &config.distributed_config {
            if let Some(coordinator) = &dist_config.coordinator {
                let dist_id = format!("distributed-{}", id);
                let controller = Arc::new(DistributedRateAdapter::new(
                    dist_id,
                    smoothed_controller,
                    coordinator.clone(),
                    config.initial_rps,
                    config.min_rps,
                    config.max_rps,
                    1.0, // Initial proportion
                    0.1, // Maximum ramp rate
                ));
                
                if let Some(registry) = &metrics_registry {
                    controller.register_with_registry(registry.clone());
                }
                
                // Start coordination
                controller.start_coordination();
                
                return controller;
            }
        }
        
        // Return smoothed controller if no distributed config
        smoothed_controller
    }
}
```

### 9.2 Builder Pattern with Combined Features

```rust
impl LoadTestBuilder {
    /// Use a PID controller
    pub fn with_pid_controller(
        mut self,
        target_metric: f64,
        kp: f64,
        ki: f64,
        kd: f64,
    ) -> Self {
        self.config.rate_control_mode = RateControlMode::Pid {
            target_metric,
            metric_type: MetricType::LatencyP99,
            kp,
            ki,
            kd,
            update_interval_ms: 500,
        };
        self
    }
    
    /// Use a PID controller with specific metric type
    pub fn with_pid_metric_type(
        mut self,
        metric_type: MetricType,
    ) -> Self {
        if let RateControlMode::Pid { 
            target_metric, kp, ki, kd, update_interval_ms, ..
        } = self.config.rate_control_mode {
            self.config.rate_control_mode = RateControlMode::Pid {
                target_metric,
                metric_type,
                kp,
                ki,
                kd,
                update_interval_ms,
            };
        }
        self
    }
    
    /// Use external signal for PID control
    pub fn with_external_signal_pid(
        mut self,
        target_metric: f64,
        kp: f64,
        ki: f64,
        kd: f64,
        signal_name: &str,
    ) -> Self {
        self.config.rate_control_mode = RateControlMode::ExternalSignalPid {
            target_metric,
            kp,
            ki,
            kd,
            update_interval_ms: 500,
            signal_name: signal_name.to_string(),
        };
        self
    }
    
    /// Use a saturation-aware PID controller
    pub fn with_saturation_aware_pid(
        mut self,
        target_metric: f64,
        kp: f64,
        ki: f64,
        kd: f64,
        saturation_threshold: f64,
    ) -> Self {
        self.config.rate_control_mode = RateControlMode::SaturationAwarePid {
            target_metric,
            kp,
            ki,
            kd,
            update_interval_ms: 500,
            saturation_threshold,
        };
        self
    }
    
    /// Use a step function controller
    pub fn with_step_function(mut self, steps: Vec<(u64, u64)>) -> Self {
        self.config.rate_control_mode = RateControlMode::StepFunction(steps);
        self
    }
    
    /// Use a smoothed rate control
    pub fn with_smoothed_rate_control(mut self, config: RateSmoothingConfig) -> Self {
        self.config.rate_smoothing_config = config;
        self
    }
    
    /// With default smoothing (10% max change per second)
    pub fn with_default_smoothing(mut self) -> Self {
        self.config.rate_smoothing_config = RateSmoothingConfig::default();
        self
    }
    
    /// With custom max change rate
    pub fn with_max_change_rate(mut self, max_relative_change_per_second: f64) -> Self {
        self.config.rate_smoothing_config.enabled = true;
        self.config.rate_smoothing_config.max_relative_change_per_second = max_relative_change_per_second;
        self
    }
    
    /// With metrics collection
    pub fn with_metrics_collection(mut self, collection_interval_ms: u64) -> Self {
        let registry = Arc::new(DefaultMetricsRegistry::new());
        self.metrics_registry = Some(registry.clone());
        
        // Start periodic collection if interval provided
        if collection_interval_ms > 0 {
            registry.start_periodic_collection(Duration::from_millis(collection_interval_ms));
        }
        
        self
    }
    
    /// With distributed coordinator
    pub fn with_distributed_coordinator(
        mut self,
        coordinator_url: String,
        node_id: String,
    ) -> Self {
        let coordinator = Arc::new(HttpDistributedCoordinator::new(
            coordinator_url,
            node_id,
        ));
        
        self.config.distributed_config = Some(DistributedConfig {
            coordinator: Some(coordinator),
            role: DistributedRole::Worker,
        });
        
        self
    }
    
    /// Build the rate controller with all selected features
    fn build_rate_controller(&self) -> Arc<dyn RateController> {
        RateControllerFactory::create_controller(
            &self.config,
            self.metrics_registry.clone(),
            self.profiler.clone(),
        )
    }
}
```

## 10. Usage Examples

### 10.1 Basic Controller with Metrics

```rust
// Example 1: Create a PID controller with metrics registry
let metrics_registry = Arc::new(DefaultMetricsRegistry::new());
let controller = PidRateController::new(
    "controller-1".to_string(),
    100,  // initial RPS
    10,   // min RPS
    1000, // max RPS
    RateControlMode::Pid {
        target_metric: 100.0,  // Target 100ms latency
        metric_type: MetricType::LatencyP99,
        kp: 0.1,
        ki: 0.01,
        kd: 0.001,
        update_interval_ms: 500,
    },
);
controller.register_with_registry(metrics_registry.clone());

// Start metrics collection
metrics_registry.start_periodic_collection(Duration::from_millis(1000));

// Use the controller in the load test
let load_test = LoadTest::new(controller, /* other components */);
```

### 10.2 Smoothed Controller with Builder Pattern

```rust
// Example 2: Using builder pattern with metrics registry
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_initial_rps(100)
    .with_duration(300)
    .with_pid_controller(100.0, 0.1, 0.01, 0.001)  // 100ms target latency
    .with_default_smoothing()  // Add 10% max change rate smoothing
    .with_metrics_collection(1000)  // Enable metrics collection with 1s interval
    .build();
```

### 10.3 Saturation-Aware Controller

```rust
// Example 3: Saturation-aware controller with metrics
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_initial_rps(100)
    .with_duration(300)
    .with_saturation_aware_pid(
        100.0,  // Target P99 latency of 100ms
        0.1,    // kp
        0.01,   // ki
        0.001,  // kd
        0.5     // Adjust rate if saturation > 50%
    )
    .with_default_smoothing()
    .with_metrics_collection(1000)
    .build();
```

### 10.4 Distributed Load Testing with Metrics

```rust
// Example 4: Distributed load testing as worker node
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_initial_rps(100)
    .with_duration(300)
    .with_pid_controller(100.0, 0.1, 0.01, 0.001)
    .with_default_smoothing()
    .with_metrics_collection(1000)
    .with_distributed_coordinator(
        "https://coordinator.example.com".to_string(),
        "worker-node-1".to_string()
    )
    .build();
```

### 10.5 External Signal Integration

```rust
// Example 5: Using external control signal from system metrics
let metrics_registry = Arc::new(DefaultMetricsRegistry::new());

// Create CPU utilization signal provider
let cpu_signal = Arc::new(CpuUtilizationSignal::new(
    "cpu-signal".to_string(),
    Duration::from_millis(500),
));
cpu_signal.register_with_registry(metrics_registry.clone());
cpu_signal.start_monitoring();

// Create signal set and register CPU signal
let signal_set = Arc::new(ControlSignalSet::new());
signal_set.register_signal("cpu", cpu_signal);

// Create controller that targets 50% CPU utilization
let controller = LoadTestBuilder::new()
    .with_external_signal_pid(
        50.0,   // Target 50% CPU utilization
        0.1,    // kp
        0.01,   // ki
        0.001,  // kd
        "cpu"   // Signal name
    )
    .with_default_smoothing()
    .with_metrics_registry(metrics_registry)
    .build();

// Provide signal set to controller during updates
controller.process_control_signals(&signal_set);
```

### 10.6 Combined Features for Advanced Scenarios

```rust
// Example 6: Combine saturation awareness, smoothing, and distributed control
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_initial_rps(100)
    .with_duration(600)
    .with_saturation_aware_pid(
        100.0,  // Target P99 latency of 100ms
        0.1,    // kp
        0.01,   // ki
        0.001,  // kd
        0.5     // Adjust rate if saturation > 50%
    )
    .with_smoothed_rate_control(RateSmoothingConfig {
        enabled: true,
        max_relative_change_per_second: 0.05,  // 5% per second (gradual)
        max_absolute_change_per_second: 50,    // Max 50 RPS per second
        min_adjustment_interval_ms: 200,       // 200ms between adjustments
        limit_decreases: true,                 // Limit rate decreases too
    })
    .with_metrics_collection(500)  // 500ms metrics collection interval
    .with_distributed_coordinator(
        "https://coordinator.example.com".to_string(),
        "worker-node-1".to_string()
    )
    .build();
```

### 10.7 Accessing Metrics

```rust
// Example 7: Monitoring metrics from various components
let registry = Arc::clone(&metrics_registry);

// Subscribe to metrics updates
let mut metrics_receiver = registry.subscribe();

tokio::spawn(async move {
    while let Some(metrics) = metrics_receiver.recv().await {
        // Check rate controller metrics
        for (id, component) in metrics.iter() {
            if component.component_type == "RateController" {
                if let Some(MetricValue::Gauge(current_rps)) = component.metrics.get("current_rps") {
                    println!("Controller {}: RPS = {}", id, current_rps);
                }
                
                if let Some(MetricValue::Float(error)) = component.metrics.get("error_value") {
                    println!("Controller {}: PID Error = {:.2}", id, error);
                }
            }
            
            if component.component_type == "ControlSignal" && id.contains("saturation") {
                if let Some(MetricValue::Float(saturation)) = component.metrics.get("current_value") {
                    println!("Saturation level: {:.2}", saturation);
                    
                    if saturation > 0.7 {
                        println!("WARNING: High client saturation detected!");
                    }
                }
            }
            
            if component.component_type == "LoadProportionSmoother" {
                if let Some(MetricValue::Float(factor)) = component.metrics.get("current_factor") {
                    println!("Load proportion factor: {:.2}", factor);
                }
            }
        }
    }
});
```

## 11. Conclusion

This combined rate control system provides a comprehensive solution for load testing scenarios, integrating metrics-based observability with sophisticated distributed control capabilities. The design enables:

1. **Comprehensive Monitoring**: Every component publishes detailed metrics through the registry
2. **Sophisticated Control Algorithms**: PID controllers, step functions, and signal-based control
3. **Precise Rate Management**: Rate smoothing prevents abrupt changes that would skew results
4. **Client Saturation Awareness**: System detects and adapts to client-side bottlenecks
5. **Distributed Load Control**: Coordinates rate across multiple nodes with centralized control
6. **Component Composition**: Mix and match controllers, smoothers, and adapters for different scenarios

The unified metrics registry integration enables thorough analysis of rate control behavior, while the distributed coordination features provide sophisticated multi-node test capabilities, all with minimal performance impact on the critical request path.
