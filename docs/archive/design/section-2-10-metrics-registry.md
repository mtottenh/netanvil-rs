# Unified Metrics Collection: Hybrid Registry Design

## 1. Introduction and Goals

The load testing framework requires a unified approach to metrics collection that satisfies several key requirements:

- **High Performance**: Minimal impact on the core load generation path
- **Composability**: Consistent with the trait-based component architecture
- **Comprehensive**: Collect metrics from both system components and request processing
- **Correlation**: Enable analysis of relationships between component state and request outcomes
- **Scalability**: Work efficiently in distributed testing scenarios

This document outlines a hybrid registry pattern with dual interface for high-throughput components.

## 2. Core Architecture: Hybrid Registry Pattern

### 2.1 Core Interfaces

```rust
/// Component metrics snapshot
#[derive(Debug, Clone)]
pub struct ComponentMetrics {
    /// Component type (e.g., "RateController", "RequestScheduler")
    pub component_type: String,
    
    /// Component instance identifier
    pub component_id: String,
    
    /// Timestamp when metrics were collected
    pub timestamp: u64,
    
    /// Metric values as key-value pairs
    pub metrics: HashMap<String, MetricValue>,
    
    /// Component-specific status information
    pub status: Option<String>,
}

/// Metric value types
#[derive(Debug, Clone)]
pub enum MetricValue {
    /// Counter (monotonically increasing)
    Counter(u64),
    
    /// Gauge (can go up or down)
    Gauge(i64),
    
    /// Floating point value
    Float(f64),
    
    /// Duration in nanoseconds
    Duration(u64),
    
    /// Histogram data
    Histogram(Arc<HistogramData>),
    
    /// String value
    Text(String),
}

/// Metrics provider interface
pub trait MetricsProvider: Send + Sync {
    /// Get current metrics snapshot
    fn get_metrics(&self) -> ComponentMetrics;
    
    /// Component type identifier
    fn get_component_type(&self) -> &str;
    
    /// Component instance identifier
    fn get_component_id(&self) -> &str;
}

/// Metrics registry interface
pub trait MetricsRegistry: Send + Sync {
    /// Register a metrics provider
    fn register_provider(&self, provider: Arc<dyn MetricsProvider>) -> RegistrationHandle;
    
    /// Unregister a provider by ID
    fn unregister_provider(&self, id: &str);
    
    /// Collect metrics from all registered providers
    fn collect_all(&self) -> HashMap<String, ComponentMetrics>;
    
    /// Collect metrics from specific component type
    fn collect_by_type(&self, component_type: &str) -> Vec<ComponentMetrics>;
    
    /// Start periodic collection
    fn start_periodic_collection(&self, interval: Duration) -> JoinHandle<()>;
    
    /// Subscribe to metrics updates
    fn subscribe(&self) -> mpsc::Receiver<HashMap<String, ComponentMetrics>>;
}

/// Registration handle for automatic cleanup
pub struct RegistrationHandle {
    id: String,
    registry: Weak<dyn MetricsRegistry>,
}

impl Drop for RegistrationHandle {
    fn drop(&mut self) {
        // Automatically unregister when handle is dropped
        if let Some(registry) = self.registry.upgrade() {
            registry.unregister_provider(&self.id);
        }
    }
}
```

### 2.2 Registry Implementation

```rust
pub struct DefaultMetricsRegistry {
    /// Registered providers
    providers: Arc<DashMap<String, Arc<dyn MetricsProvider>>>,
    
    /// Last collected metrics by provider ID
    metrics_cache: Arc<DashMap<String, ComponentMetrics>>,
    
    /// Subscription channels
    subscribers: Arc<Mutex<Vec<mpsc::Sender<HashMap<String, ComponentMetrics>>>>>,
    
    /// Collection task handle
    collection_task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl MetricsRegistry for DefaultMetricsRegistry {
    fn register_provider(&self, provider: Arc<dyn MetricsProvider>) -> RegistrationHandle {
        let id = format!("{}:{}", provider.get_component_type(), provider.get_component_id());
        self.providers.insert(id.clone(), provider);
        
        RegistrationHandle {
            id,
            registry: Arc::downgrade(&(self as Arc<dyn MetricsRegistry>)),
        }
    }
    
    fn unregister_provider(&self, id: &str) {
        self.providers.remove(id);
        self.metrics_cache.remove(id);
    }
    
    fn collect_all(&self) -> HashMap<String, ComponentMetrics> {
        let mut results = HashMap::new();
        
        for entry in self.providers.iter() {
            let id = entry.key().clone();
            let provider = entry.value().clone();
            
            // Collect metrics in a non-blocking manner
            let metrics = provider.get_metrics();
            results.insert(id.clone(), metrics.clone());
            self.metrics_cache.insert(id, metrics);
        }
        
        // Notify subscribers
        self.notify_subscribers(&results);
        
        results
    }
    
    fn collect_by_type(&self, component_type: &str) -> Vec<ComponentMetrics> {
        let mut results = Vec::new();
        
        for entry in self.providers.iter() {
            let provider = entry.value().clone();
            
            if provider.get_component_type() == component_type {
                results.push(provider.get_metrics());
            }
        }
        
        results
    }
    
    fn start_periodic_collection(&self, interval: Duration) -> JoinHandle<()> {
        let registry = self.clone();
        let task = tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(interval);
            
            loop {
                interval_timer.tick().await;
                registry.collect_all();
            }
        });
        
        let mut collection_task = self.collection_task.lock().unwrap();
        *collection_task = Some(task.clone());
        
        task
    }
    
    fn subscribe(&self) -> mpsc::Receiver<HashMap<String, ComponentMetrics>> {
        let (tx, rx) = mpsc::channel(100);
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.push(tx);
        rx
    }
}

impl DefaultMetricsRegistry {
    pub fn new() -> Self {
        Self {
            providers: Arc::new(DashMap::new()),
            metrics_cache: Arc::new(DashMap::new()),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            collection_task: Arc::new(Mutex::new(None)),
        }
    }
    
    fn notify_subscribers(&self, metrics: &HashMap<String, ComponentMetrics>) {
        let subscribers = self.subscribers.lock().unwrap();
        
        // Remove closed channels while notifying
        for tx in subscribers.iter() {
            let _ = tx.try_send(metrics.clone());
        }
    }
}
```

## 3. Component Metrics Collection

### 3.1 Lock-Free Metrics Implementation

Components should implement efficient metrics collection using atomic operations:

```rust
pub struct RateControllerMetrics {
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
    
    /// History of rate changes (limited size with RwLock protection)
    rate_history: Arc<RwLock<VecDeque<(u64, u64)>>>,
}

impl RateControllerMetrics {
    pub fn new(initial_rps: u64, min_rps: u64, max_rps: u64) -> Self {
        Self {
            current_rps: AtomicU64::new(initial_rps),
            min_rps: AtomicU64::new(min_rps),
            max_rps: AtomicU64::new(max_rps),
            adjustment_count: AtomicU64::new(0),
            last_adjustment: AtomicU64::new(0),
            rate_history: Arc::new(RwLock::new(VecDeque::with_capacity(100))),
        }
    }
    
    pub fn update_rps(&self, new_rps: u64) {
        self.current_rps.store(new_rps, Ordering::Relaxed);
        self.adjustment_count.fetch_add(1, Ordering::Relaxed);
        
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        
        self.last_adjustment.store(now, Ordering::Relaxed);
        
        // Update history (with bounded size)
        if let Ok(mut history) = self.rate_history.write() {
            history.push_back((now, new_rps));
            
            // Keep bounded size
            while history.len() > 100 {
                history.pop_front();
            }
        }
    }
    
    pub fn get_snapshot(&self) -> HashMap<String, MetricValue> {
        let mut metrics = HashMap::new();
        
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
        
        // Include recent history as string (could be optimized)
        if let Ok(history) = self.rate_history.read() {
            let history_str = history.iter()
                .map(|(ts, rps)| format!("{}:{}", ts, rps))
                .collect::<Vec<_>>()
                .join(",");
                
            metrics.insert("recent_history".to_string(),
                          MetricValue::Text(history_str));
        }
        
        metrics
    }
}
```

### 3.2 Implementing Metrics Provider for Components

A typical component implementation would look like:

```rust
pub struct PidRateController {
    id: String,
    config: Arc<LoadTestConfig>,
    metrics: Arc<RateControllerMetrics>,
    // Other fields...
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
            status: Some(format!("PID Controller: Target={}", self.config.target_metric)),
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

## 4. Dual Interface for High-Throughput Components

### 4.1 Result Collectors with Dual Interface

For components that process a high volume of requests like Results Collectors, we implement a dual interface:

```rust
/// Standard interface for results collection (high-performance path)
pub trait ResultsCollector: Send + Sync {
    /// Record a completed request
    fn record_request(&self, request: CompletedRequest);
    
    /// Get current metrics snapshot
    fn get_current_metrics(&self) -> RequestMetrics;
    
    /// Get final test results
    fn get_final_results(&self) -> TestResults;
}

/// Metrics-aware collector that also implements MetricsProvider
pub trait MetricsAwareResultsCollector: ResultsCollector + MetricsProvider {
    /// Register with metrics registry
    fn register_with_registry(&self, registry: Arc<dyn MetricsRegistry>) -> RegistrationHandle;
}
```

#### Implementation Example:

```rust
pub struct StandardResultsCollector {
    id: String,
    config: Arc<LoadTestConfig>,
    metrics: Arc<ResultsMetrics>,
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl ResultsCollector for StandardResultsCollector {
    fn record_request(&self, request: CompletedRequest) {
        // High-performance, direct path for processing requests
        // No locking or allocation in the hot path
        self.metrics.add_request(&request);
    }
    
    fn get_current_metrics(&self) -> RequestMetrics {
        // Extract current metrics from internal state
        self.metrics.get_current_metrics()
    }
    
    fn get_final_results(&self) -> TestResults {
        // Generate final results at test end
        self.metrics.generate_test_results()
    }
}

impl MetricsProvider for StandardResultsCollector {
    fn get_metrics(&self) -> ComponentMetrics {
        // This path is called less frequently for monitoring
        let mut metrics = HashMap::new();
        
        // Add aggregated metrics
        metrics.insert("total_requests".to_string(),
                      MetricValue::Counter(self.metrics.total_count.load(Ordering::Relaxed)));
                      
        metrics.insert("successful_requests".to_string(),
                      MetricValue::Counter(self.metrics.success_count.load(Ordering::Relaxed)));
                      
        metrics.insert("error_requests".to_string(),
                      MetricValue::Counter(self.metrics.error_count.load(Ordering::Relaxed)));
                      
        metrics.insert("p50_latency_us".to_string(),
                      MetricValue::Duration(self.metrics.get_p50_latency()));
                      
        metrics.insert("p99_latency_us".to_string(),
                      MetricValue::Duration(self.metrics.get_p99_latency()));
        
        // Return formatted metrics
        ComponentMetrics {
            component_type: "ResultsCollector".to_string(),
            component_id: self.id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            metrics,
            status: None,
        }
    }
    
    fn get_component_type(&self) -> &str {
        "ResultsCollector" 
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}

impl MetricsAwareResultsCollector for StandardResultsCollector {
    fn register_with_registry(&self, registry: Arc<dyn MetricsRegistry>) -> RegistrationHandle {
        let provider = Arc::new(self.clone()) as Arc<dyn MetricsProvider>;
        let handle = registry.register_provider(provider);
        
        let mut registry_handle = self.registry_handle.lock().unwrap();
        *registry_handle = Some(handle.clone());
        
        handle
    }
}
```

### 4.2 Sample Recorders with Dual Interface

Similar to Results Collectors, Sample Recorders also benefit from a dual interface approach:

```rust
/// Standard interface for sample recording (high-performance path)
pub trait SampleRecorder: Send + Sync {
    /// Record a detailed sample
    fn record_sample(&self, sample: RequestSample);
    
    /// Record a sample with transaction analysis
    fn record_profiled_sample(&self, sample: RequestSample, analysis: Option<TransactionAnalysis>);
    
    /// Finalize and save all recorded data
    async fn finalize(&self);
}

/// Metrics-aware sample recorder
pub trait MetricsAwareSampleRecorder: SampleRecorder + MetricsProvider {
    /// Register with metrics registry
    fn register_with_registry(&self, registry: Arc<dyn MetricsRegistry>) -> RegistrationHandle;
}
```

## 5. Integration with the Builder Pattern

The builder pattern allows easy configuration of metrics collection:

```rust
impl LoadTestBuilder {
    /// Enable metrics collection
    pub fn with_metrics_collection(mut self, collection_interval_ms: u64) -> Self {
        let registry = Arc::new(DefaultMetricsRegistry::new());
        self.metrics_registry = Some(registry.clone());
        
        // Start periodic collection if interval provided
        if collection_interval_ms > 0 {
            registry.start_periodic_collection(Duration::from_millis(collection_interval_ms));
        }
        
        self
    }
    
    /// Build the test with all components
    pub fn build(self) -> LoadTest {
        // Create components
        let rate_controller = self.build_rate_controller();
        let scheduler = self.build_scheduler();
        let executor = self.build_executor();
        let results_collector = self.build_results_collector();
        let sample_recorder = self.build_sample_recorder();
        
        // Register components with registry if enabled
        if let Some(registry) = &self.metrics_registry {
            // Register regular components directly
            let _ = registry.register_provider(rate_controller.clone() as Arc<dyn MetricsProvider>);
            let _ = registry.register_provider(scheduler.clone() as Arc<dyn MetricsProvider>);
            let _ = registry.register_provider(executor.clone() as Arc<dyn MetricsProvider>);
            
            // Register high-throughput components using dual interface
            if let Ok(metrics_aware) = results_collector.clone()
                .downcast::<dyn MetricsAwareResultsCollector>() {
                metrics_aware.register_with_registry(registry.clone());
            }
            
            if let Ok(metrics_aware) = sample_recorder.clone()
                .downcast::<dyn MetricsAwareSampleRecorder>() {
                metrics_aware.register_with_registry(registry.clone());
            }
        }
        
        // Create and return load test
        LoadTest {
            config: self.config,
            rate_controller,
            scheduler,
            executor,
            results_collector,
            sample_recorder,
            metrics_registry: self.metrics_registry,
        }
    }
}
```

## 6. Lock-Free Metrics Implementation for High-Throughput Components

For high-throughput components, a careful implementation of metrics tracking is crucial:

```rust
pub struct ResultsMetrics {
    /// Total request count
    total_count: AtomicU64,
    
    /// Successful request count
    success_count: AtomicU64,
    
    /// Error request count
    error_count: AtomicU64,
    
    /// Latency histogram (protected by RwLock for occasional updates)
    latency_histogram: Arc<RwLock<Histogram<u64>>>,
    
    /// Last update timestamp
    last_update: AtomicU64,
    
    /// Concurrent update count tracker (for detecting contention)
    concurrent_updates: AtomicU32,
}

impl ResultsMetrics {
    /// Add a request to the metrics
    pub fn add_request(&self, request: &CompletedRequest) {
        // Increment concurrent updates counter
        let concurrent = self.concurrent_updates.fetch_add(1, Ordering::Acquire);
        
        // Track relevant counts with atomic operations
        self.total_count.fetch_add(1, Ordering::Relaxed);
        
        if request.is_error {
            self.error_count.fetch_add(1, Ordering::Relaxed);
        } else {
            self.success_count.fetch_add(1, Ordering::Relaxed);
        }
        
        // Update timestamp
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
            
        self.last_update.store(now, Ordering::Relaxed);
        
        // Update histogram selectively to reduce contention
        // Only update every N requests to avoid lock contention
        if self.total_count.load(Ordering::Relaxed) % 100 == 0 {
            if let Ok(mut hist) = self.latency_histogram.try_write() {
                let latency_us = request.end_time
                    .duration_since(request.start_time)
                    .unwrap_or_default()
                    .as_micros() as u64;
                    
                let _ = hist.record(latency_us);
            }
        }
        
        // Decrement concurrent updates counter
        self.concurrent_updates.fetch_sub(1, Ordering::Release);
    }
    
    /// Get percentile latency values with minimal locking
    pub fn get_p50_latency(&self) -> u64 {
        if let Ok(hist) = self.latency_histogram.try_read() {
            hist.value_at_percentile(50.0)
        } else {
            0 // Default if locked
        }
    }
    
    pub fn get_p99_latency(&self) -> u64 {
        if let Ok(hist) = self.latency_histogram.try_read() {
            hist.value_at_percentile(99.0)
        } else {
            0 // Default if locked
        }
    }
    
    /// Get current metrics for direct interface
    pub fn get_current_metrics(&self) -> RequestMetrics {
        RequestMetrics {
            timestamp: self.last_update.load(Ordering::Relaxed),
            window_size_ms: 0, // Not applicable for cumulative metrics
            request_count: self.total_count.load(Ordering::Relaxed),
            p50_latency_us: self.get_p50_latency(),
            p90_latency_us: self.get_p90_latency(),
            p99_latency_us: self.get_p99_latency(),
            error_rate: self.calculate_error_rate(),
            current_throughput: self.calculate_throughput(),
        }
    }
    
    /// Calculate error rate
    fn calculate_error_rate(&self) -> f64 {
        let total = self.total_count.load(Ordering::Relaxed);
        let errors = self.error_count.load(Ordering::Relaxed);
        
        if total > 0 {
            errors as f64 / total as f64
        } else {
            0.0
        }
    }
    
    // Other helper methods...
}
```

## 7. Usage Examples

### 7.1 Metrics-Aware Component Implementation

```rust
pub struct PoissonScheduler {
    id: String,
    config: SchedulerConfig,
    metrics: Arc<SchedulerMetrics>,
    timer: Arc<PrecisionTimer>,
    rng: Arc<Mutex<SmallRng>>,
    // Other fields...
}

impl PoissonScheduler {
    pub fn new(id: String, config: SchedulerConfig) -> Self {
        let metrics = Arc::new(SchedulerMetrics::new());
        
        Self {
            id,
            config,
            metrics,
            timer: Arc::new(PrecisionTimer::new(
                Duration::from_micros(config.tick_resolution_us),
                Duration::from_nanos(config.jitter_threshold_ns),
            )),
            rng: Arc::new(Mutex::new(SmallRng::from_entropy())),
            // Initialize other fields...
        }
    }
    
    fn record_request(&self, id: u64, scheduled_time: Instant, is_warmup: bool) {
        // Record request in metrics
        let now = Instant::now();
        let delay = now.duration_since(scheduled_time).as_nanos() as u64;
        
        self.metrics.record_request(is_warmup, delay);
    }
}

impl RequestScheduler for PoissonScheduler {
    // Implementation of the request scheduler interface
    // ...
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
            status: Some(format!("Poisson Scheduler: RPS={}", self.config.target_rps)),
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

### 7.2 Accessing Component and Request Metrics

```rust
// Get metrics from registry (for external components)
pub fn get_system_metrics(registry: &Arc<dyn MetricsRegistry>) -> HashMap<String, ComponentMetrics> {
    registry.collect_all()
}

// Get specific component type metrics
pub fn get_rate_controller_metrics(registry: &Arc<dyn MetricsRegistry>) -> Vec<ComponentMetrics> {
    registry.collect_by_type("RateController")
}

// Subscribe to metrics updates
pub fn subscribe_to_metrics(registry: &Arc<dyn MetricsRegistry>) -> mpsc::Receiver<HashMap<String, ComponentMetrics>> {
    registry.subscribe()
}

// Directly access request metrics (high-performance path)
pub fn get_request_metrics(results_collector: &Arc<dyn ResultsCollector>) -> RequestMetrics {
    results_collector.get_current_metrics()
}
```

## 8. Conclusion

The hybrid registry pattern with dual interface for high-throughput components provides the ideal balance between:

1. **Performance**: Minimal overhead on critical request processing paths
2. **Observability**: Comprehensive metrics for all system components
3. **Flexibility**: Components can choose the appropriate implementation based on their characteristics
4. **Composability**: Consistent with the trait-based architecture

This approach ensures:

- High-performance request processing with direct interfaces
- Comprehensive component-level metrics through the registry
- Unified metrics access for system monitoring
- Easy integration through the builder pattern

Implementation should prioritize:

1. Lock-free data structures for metrics
2. Separate hot paths from monitoring paths
3. Minimal contention in high-throughput components
4. Consistent metrics naming and structure

This metrics collection architecture provides a solid foundation for monitoring, debugging, and analyzing load test behavior across all components.
