# netanvil-metrics Implementation Guide

## Overview

The `netanvil-metrics` crate provides high-performance, lock-free metrics collection for the load testing framework. It implements the dual-interface pattern to support both high-throughput operations and monitoring/debugging.

## Related Design Documents

- [Metrics Registry Design](../../section-2-10-metrics-registry.md) - Complete metrics architecture
- [Architecture Overview](../../section-1-architecture.md) - Lock-free design principles

## Key Components

### Lock-Free Collectors

From the metrics registry design:

```rust
/// High-performance counter using atomic operations
pub struct AtomicCounter {
    value: AtomicU64,
    name: &'static str,
}

impl AtomicCounter {
    #[inline(always)]
    pub fn increment(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }
    
    #[inline(always)]
    pub fn add(&self, delta: u64) {
        self.value.fetch_add(delta, Ordering::Relaxed);
    }
    
    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

/// Lock-free histogram using HdrHistogram
pub struct LockFreeHistogram {
    /// Active histogram for writing
    active: AtomicPtr<Histogram<u64>>,
    
    /// Inactive histogram for reading
    inactive: AtomicPtr<Histogram<u64>>,
    
    /// Swap lock (only taken during rotation)
    swap_lock: AtomicBool,
}

impl LockFreeHistogram {
    #[inline(always)]
    pub fn record(&self, value: u64) {
        let hist = unsafe { &*self.active.load(Ordering::Acquire) };
        hist.record(value).unwrap_or(());
    }
    
    /// Rotate histograms for reading
    pub fn rotate(&self) -> &Histogram<u64> {
        // Spin until we acquire swap lock
        while self.swap_lock.compare_exchange(
            false, true, 
            Ordering::Acquire, 
            Ordering::Relaxed
        ).is_err() {
            core::hint::spin_loop();
        }
        
        // Swap active and inactive
        let old_active = self.active.load(Ordering::Relaxed);
        let old_inactive = self.inactive.load(Ordering::Relaxed);
        
        self.active.store(old_inactive, Ordering::Release);
        self.inactive.store(old_active, Ordering::Release);
        
        // Release lock
        self.swap_lock.store(false, Ordering::Release);
        
        // Return the now-inactive histogram for reading
        unsafe { &*old_active }
    }
}
```

### Dual-Interface Pattern

Supporting both performance and monitoring:

```rust
/// Performance-optimized interface
pub struct MetricsHandle {
    counters: HashMap<MetricId, Arc<AtomicCounter>>,
    histograms: HashMap<MetricId, Arc<LockFreeHistogram>>,
    gauges: HashMap<MetricId, Arc<AtomicGauge>>,
}

impl MetricsHandle {
    /// Zero-cost metric recording
    #[inline(always)]
    pub fn record_request_latency(&self, latency_us: u64) {
        if let Some(hist) = self.histograms.get(&LATENCY_HISTOGRAM) {
            hist.record(latency_us);
        }
    }
    
    #[inline(always)]
    pub fn increment_requests(&self) {
        if let Some(counter) = self.counters.get(&REQUEST_COUNTER) {
            counter.increment();
        }
    }
}

/// Monitoring interface with richer functionality
pub struct MetricsRegistry {
    /// All registered metrics
    metrics: RwLock<HashMap<String, MetricMetadata>>,
    
    /// Metric handles for performance path
    handles: Arc<MetricsHandle>,
    
    /// Exporters for external systems
    exporters: Vec<Box<dyn MetricExporter>>,
}

impl MetricsRegistry {
    /// Register a new counter
    pub fn register_counter(&self, name: &'static str) -> Arc<AtomicCounter> {
        let counter = Arc::new(AtomicCounter::new(name));
        
        let mut metrics = self.metrics.write().unwrap();
        metrics.insert(name.to_string(), MetricMetadata {
            name: name.to_string(),
            metric_type: MetricType::Counter,
            unit: MetricUnit::Count,
            description: None,
        });
        
        counter
    }
    
    /// Get snapshot of all metrics
    pub fn snapshot(&self) -> MetricsSnapshot {
        let metrics = self.metrics.read().unwrap();
        
        MetricsSnapshot {
            timestamp: Instant::now(),
            counters: self.collect_counters(&metrics),
            histograms: self.collect_histograms(&metrics),
            gauges: self.collect_gauges(&metrics),
        }
    }
}
```

### Time-Series Aggregation

For tracking metrics over time:

```rust
pub struct TimeSeriesCollector<T> {
    /// Ring buffer of time buckets
    buckets: Vec<TimeBucket<T>>,
    
    /// Current bucket index
    current: AtomicUsize,
    
    /// Bucket duration
    bucket_duration: Duration,
    
    /// Total duration to retain
    retention: Duration,
}

struct TimeBucket<T> {
    start_time: AtomicU64,
    data: T,
}

impl TimeSeriesCollector<LockFreeHistogram> {
    pub fn new(bucket_duration: Duration, retention: Duration) -> Self {
        let bucket_count = (retention.as_secs() / bucket_duration.as_secs()) as usize;
        let buckets = (0..bucket_count)
            .map(|_| TimeBucket {
                start_time: AtomicU64::new(0),
                data: LockFreeHistogram::new(1, 1_000_000, 3),
            })
            .collect();
        
        Self {
            buckets,
            current: AtomicUsize::new(0),
            bucket_duration,
            retention,
        }
    }
    
    #[inline(always)]
    pub fn record(&self, value: u64) {
        let idx = self.current.load(Ordering::Relaxed);
        self.buckets[idx].data.record(value);
    }
    
    /// Advance to next bucket if needed
    pub fn maybe_rotate(&self) {
        let now = Instant::now().as_millis() as u64;
        let idx = self.current.load(Ordering::Relaxed);
        let bucket_start = self.buckets[idx].start_time.load(Ordering::Relaxed);
        
        if now - bucket_start > self.bucket_duration.as_millis() as u64 {
            // Move to next bucket
            let next_idx = (idx + 1) % self.buckets.len();
            self.current.store(next_idx, Ordering::Relaxed);
            
            // Reset the next bucket
            self.buckets[next_idx].start_time.store(now, Ordering::Relaxed);
            self.buckets[next_idx].data.clear();
        }
    }
}
```

### Metric Exporters

Integration with monitoring systems:

```rust
#[async_trait]
pub trait MetricExporter: Send + Sync {
    /// Export a batch of metrics
    async fn export(&self, metrics: &MetricsSnapshot) -> Result<()>;
    
    /// Get exporter name
    fn name(&self) -> &str;
}

/// Prometheus exporter
pub struct PrometheusExporter {
    endpoint: String,
    client: reqwest::Client,
}

impl PrometheusExporter {
    fn format_metrics(&self, snapshot: &MetricsSnapshot) -> String {
        let mut output = String::new();
        
        // Format counters
        for (name, value) in &snapshot.counters {
            writeln!(output, "# TYPE {} counter", name);
            writeln!(output, "{} {}", name, value);
        }
        
        // Format histograms
        for (name, hist) in &snapshot.histograms {
            writeln!(output, "# TYPE {} histogram", name);
            writeln!(output, "{}_count {}", name, hist.len());
            writeln!(output, "{}_sum {}", name, hist.sum());
            
            for percentile in [50.0, 90.0, 95.0, 99.0, 99.9] {
                let value = hist.value_at_percentile(percentile);
                writeln!(output, "{}_p{} {}", name, percentile, value);
            }
        }
        
        output
    }
}
```

### Sampling Strategies

Efficient sampling for detailed analysis:

```rust
/// Adaptive sampler that increases rate for interesting events
pub struct AdaptiveSampler {
    /// Base sampling rate
    base_rate: f64,
    
    /// Current sampling rate
    current_rate: AtomicU64, // Fixed-point representation
    
    /// Threshold for interesting events
    threshold: u64,
    
    /// Random state for sampling decisions
    rng: RefCell<XorShiftRng>,
}

impl AdaptiveSampler {
    #[inline]
    pub fn should_sample(&self, value: u64) -> bool {
        // Always sample high values
        if value > self.threshold {
            return true;
        }
        
        // Probabilistic sampling for normal values
        let rate = f64::from_bits(self.current_rate.load(Ordering::Relaxed));
        let mut rng = self.rng.borrow_mut();
        rng.gen::<f64>() < rate
    }
    
    /// Adjust sampling rate based on load
    pub fn adjust_rate(&self, current_rps: u64) {
        let new_rate = if current_rps > 10_000 {
            self.base_rate * 0.1  // Sample less at high load
        } else if current_rps > 1_000 {
            self.base_rate * 0.5
        } else {
            self.base_rate
        };
        
        self.current_rate.store(new_rate.to_bits(), Ordering::Relaxed);
    }
}
```

## Usage Patterns

### Component Metrics

How components register and use metrics:

```rust
pub struct HttpExecutor {
    metrics: HttpExecutorMetrics,
}

struct HttpExecutorMetrics {
    requests_sent: Arc<AtomicCounter>,
    requests_completed: Arc<AtomicCounter>,
    request_errors: Arc<AtomicCounter>,
    connection_pool_size: Arc<AtomicGauge>,
    latency_histogram: Arc<LockFreeHistogram>,
}

impl HttpExecutor {
    pub fn new(registry: &MetricsRegistry) -> Self {
        let metrics = HttpExecutorMetrics {
            requests_sent: registry.register_counter("http.requests.sent"),
            requests_completed: registry.register_counter("http.requests.completed"),
            request_errors: registry.register_counter("http.requests.errors"),
            connection_pool_size: registry.register_gauge("http.connections.active"),
            latency_histogram: registry.register_histogram(
                "http.request.latency",
                1, // 1 microsecond minimum
                60_000_000, // 60 seconds maximum
                3 // 3 significant figures
            ),
        };
        
        Self { metrics }
    }
    
    #[inline(always)]
    pub async fn execute(&self, request: Request) -> Result<Response> {
        self.metrics.requests_sent.increment();
        
        let start = Instant::now();
        let result = self.do_execute(request).await;
        let latency = start.elapsed().as_micros() as u64;
        
        self.metrics.latency_histogram.record(latency);
        
        match result {
            Ok(response) => {
                self.metrics.requests_completed.increment();
                Ok(response)
            }
            Err(e) => {
                self.metrics.request_errors.increment();
                Err(e)
            }
        }
    }
}
```

## Performance Considerations

1. **Cache Line Alignment**: Ensure atomics don't share cache lines
2. **Memory Ordering**: Use `Relaxed` ordering where possible
3. **Histogram Rotation**: Rotate during quiet periods
4. **Batch Operations**: Export metrics in batches

## Testing

Property-based tests for correctness:

```rust
#[test]
fn test_concurrent_histogram_safety() {
    let hist = Arc::new(LockFreeHistogram::new(1, 1_000_000, 3));
    let threads: Vec<_> = (0..10)
        .map(|i| {
            let h = hist.clone();
            thread::spawn(move || {
                for j in 0..10000 {
                    h.record((i * 10000 + j) as u64);
                }
            })
        })
        .collect();
    
    for t in threads {
        t.join().unwrap();
    }
    
    let snapshot = hist.rotate();
    assert_eq!(snapshot.len(), 100_000);
}
```