# Enhanced Design: Network Load Testing Framework with eBPF Transaction Profiling

## 1. Overview of Enhanced Architecture

The original load testing framework design is extended to incorporate kernel-level profiling using eBPF, allowing for accurate distinction between true network/server latency and client-side runtime overhead.

This enhancement focuses on:

1. **Accurate measurement** of network transactions at the kernel level
2. **Correlation** between kernel events and runtime events
3. **Attribution** of latency to different sources (network, server, client runtime)
4. **Adaptation** of the testing approach based on detected client saturation

## 2. Core Profiling Traits and Types

### 2.1 Transaction Profiling Components

```rust
/// Profiling capability trait that components can implement
pub trait ProfilingCapability: Send + Sync {
    /// Returns true if this component supports profiling
    fn supports_profiling(&self) -> bool;

    /// Get the current profiling mode
    fn get_profiling_mode(&self) -> ProfilingMode;

    /// Set the profiling mode
    fn set_profiling_mode(&mut self, mode: ProfilingMode);
}

/// Available profiling modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfilingMode {
    /// No profiling
    None,
    /// Runtime-only profiling (tokio instrumentation)
    RuntimeOnly,
    /// Kernel-level profiling using eBPF
    KernelLevel,
    /// Comprehensive profiling (kernel + runtime correlation)
    Comprehensive,
}

/// Transaction phase markers for detailed profiling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionPhase {
    /// TCP connect phase
    Connect,
    /// TLS handshake phase
    TlsHandshake,
    /// Request sending phase
    RequestSend,
    /// Time to first byte (waiting for server response)
    TTFB,
    /// Response receiving phase
    ResponseReceive,
}

/// Events at kernel level
#[derive(Debug, Clone)]
pub struct KernelEvent {
    /// Timestamp in nanoseconds since start
    pub timestamp_ns: u64,
    /// Event type
    pub event_type: KernelEventType,
    /// Socket file descriptor
    pub socket_fd: i32,
    /// Additional data specific to event type
    pub data: KernelEventData,
}

/// Types of kernel events
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelEventType {
    /// TCP connection started (SYN sent)
    TcpConnectStart,
    /// TCP connection established
    TcpConnectComplete,
    /// Socket marked as readable by kernel
    SocketReadable,
    /// Socket data received (may be called multiple times)
    DataReceived,
    /// Socket data sent (may be called multiple times)
    DataSent,
    /// TLS handshake start (from uprobe)
    TlsHandshakeStart,
    /// TLS handshake complete (from uprobe)
    TlsHandshakeComplete,
}

/// Additional data for kernel events
#[derive(Debug, Clone)]
pub enum KernelEventData {
    /// No additional data
    None,
    /// Size of data received or sent
    DataSize(usize),
    /// Available bytes in socket buffer
    BufferState(usize),
    /// TLS-specific data
    Tls(TlsEventData),
}

/// Runtime-level events
#[derive(Debug, Clone)]
pub struct RuntimeEvent {
    /// Timestamp in system time
    pub timestamp: std::time::Instant,
    /// Event type
    pub event_type: RuntimeEventType,
    /// Request ID (for correlation)
    pub request_id: u64,
    /// Additional data specific to event type
    pub data: RuntimeEventData,
}

/// Types of runtime events
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEventType {
    /// Async connect future creation
    ConnectStart,
    /// Async connect future resolution
    ConnectComplete,
    /// TLS handshake initiated
    TlsHandshakeStart,
    /// TLS handshake completed
    TlsHandshakeComplete,
    /// Request write initiated
    RequestWriteStart,
    /// Request write completed
    RequestWriteComplete,
    /// First response read attempt
    FirstReadAttempt,
    /// First response byte received
    FirstByteReceived,
    /// Response read operation (may be multiple)
    ResponseRead,
    /// Response fully received
    ResponseComplete,
}
```

### 2.2 Profiler Interface

```rust
/// Interface for transaction profilers
pub trait TransactionProfiler: Send + Sync {
    /// Initialize the profiler
    fn initialize(&mut self) -> Result<(), Box<dyn std::error::Error>>;

    /// Register a socket for profiling
    fn register_socket(&self, request_id: u64, socket_fd: i32) -> Result<(), Box<dyn std::error::Error>>;

    /// Record a runtime event
    fn record_runtime_event(&self, event: RuntimeEvent);

    /// Get all kernel events for a request
    fn get_kernel_events(&self, request_id: u64) -> Vec<KernelEvent>;

    /// Get all runtime events for a request
    fn get_runtime_events(&self, request_id: u64) -> Vec<RuntimeEvent>;

    /// Analyze a transaction, correlating kernel and runtime events
    fn analyze_transaction(&self, request_id: u64) -> TransactionAnalysis;

    /// Get client saturation metrics
    fn get_saturation_metrics(&self) -> SaturationMetrics;

    /// Clean up resources for completed requests
    fn cleanup_request(&self, request_id: u64);

    /// Shut down the profiler
    fn shutdown(&self);
}

/// eBPF implementation of transaction profiler
pub struct EbpfTransactionProfiler {
    /// BPF program for socket tracing
    bpf_program: BpfSocketTracer,
    /// Map of socket FDs to request IDs
    socket_to_request: Arc<Mutex<HashMap<i32, u64>>>,
    /// Map of request IDs to socket FDs
    request_to_socket: Arc<Mutex<HashMap<u64, i32>>>,
    /// Kernel events by request ID
    kernel_events: Arc<Mutex<HashMap<u64, Vec<KernelEvent>>>>,
    /// Runtime events by request ID
    runtime_events: Arc<Mutex<HashMap<u64, Vec<RuntimeEvent>>>>,
    /// Event receiver from BPF
    event_receiver: Option<Receiver<KernelEvent>>,
    /// Background task handle
    background_task: Option<JoinHandle<()>>,
}

/// Runtime-only implementation of transaction profiler
pub struct RuntimeTransactionProfiler {
    /// Runtime events by request ID
    runtime_events: Arc<Mutex<HashMap<u64, Vec<RuntimeEvent>>>>,
}

/// No-op implementation of transaction profiler
pub struct NoopTransactionProfiler;
```

### 2.3 Analysis Structures

```rust
/// Complete transaction analysis with kernel and runtime correlation
#[derive(Debug, Clone)]
pub struct TransactionAnalysis {
    /// Request ID
    pub request_id: u64,
    /// True network transaction phases (kernel level)
    pub kernel_phases: HashMap<TransactionPhase, PhaseMetrics>,
    /// Runtime overhead by phase
    pub runtime_overhead: HashMap<TransactionPhase, PhaseMetrics>,
    /// Socket read events analysis
    pub read_events: Vec<ReadEventAnalysis>,
    /// Client saturation impact assessment
    pub saturation_impact: SaturationImpact,
    /// Overall analysis
    pub overall: OverallAnalysis,
}

/// Metrics for a transaction phase
#[derive(Debug, Clone)]
pub struct PhaseMetrics {
    /// Start time in nanoseconds
    pub start_ns: u64,
    /// End time in nanoseconds
    pub end_ns: u64,
    /// Duration in microseconds
    pub duration_us: u64,
}

/// Analysis of individual socket read events
#[derive(Debug, Clone)]
pub struct ReadEventAnalysis {
    /// When kernel marked socket readable
    pub kernel_ready_ns: u64,
    /// When runtime read the data
    pub runtime_read_ns: u64,
    /// Delay between kernel ready and runtime read
    pub delay_us: u64,
    /// Bytes read in this operation
    pub bytes_read: usize,
}

/// Assessment of client saturation impact
#[derive(Debug, Clone)]
pub struct SaturationImpact {
    /// Overall runtime overhead percentage
    pub overhead_percentage: f64,
    /// Maximum read delay in microseconds
    pub max_read_delay_us: u64,
    /// Average read delay in microseconds
    pub avg_read_delay_us: f64,
    /// Classification of impact
    pub impact_level: SaturationLevel,
}

/// Overall transaction analysis
#[derive(Debug, Clone)]
pub struct OverallAnalysis {
    /// Total time at kernel level in microseconds
    pub total_kernel_time_us: u64,
    /// Total runtime overhead in microseconds
    pub total_runtime_overhead_us: u64,
    /// Percentage of time attributable to runtime overhead
    pub runtime_overhead_percentage: f64,
    /// Estimated true server response time
    pub estimated_server_time_us: u64,
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
    /// Recommended actions based on saturation level
    pub recommendation: SaturationAction,
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

## 3. Extensions to Existing Traits

### 3.1 Enhanced RequestExecutor

```rust
/// Enhanced interface for executing requests with profiling support
pub trait RequestExecutor: ProfilingCapability + Send + Sync {
    /// Run the executor until all tickets are processed
    async fn run<C>(
        &self,
        config: Arc<LoadTestConfig>,
        client: Arc<C>,
        semaphore: Arc<tokio::sync::Semaphore>,
        ticket_rx: tokio::sync::mpsc::Receiver<RequestTicket>,
        result_tx: tokio::sync::mpsc::Sender<CompletedRequest>,
        // New parameter for profiler
        profiler: Option<Arc<dyn TransactionProfiler>>,
    ) where
        C: hyper::client::connect::Connect + Clone + Send + Sync + 'static;
}

/// Extended completedRequest with profiling data
#[derive(Debug, Clone)]
pub struct CompletedRequest {
    /// The original request ticket
    pub ticket: RequestTicket,
    /// When request was actually sent
    pub start_time: Instant,
    /// When response was received
    pub end_time: Instant,
    /// HTTP status code returned
    pub status_code: u16,
    /// Whether an error occurred
    pub is_error: bool,
    /// Size of the response in bytes
    pub response_size: u64,
    /// Optional transaction analysis from profiler
    pub transaction_analysis: Option<TransactionAnalysis>,
}

/// Capability to inject socket profiling hooks
pub trait SocketProfilingCapability: Send + Sync {
    /// Get socket file descriptor for a connection
    fn get_socket_fd(&self, conn: &dyn Any) -> Option<i32>;

    /// Register socket hooks for profiling
    fn register_socket_hooks(&self, fd: i32, request_id: u64, profiler: Arc<dyn TransactionProfiler>);

    /// Track a socket buffer operation
    fn track_buffer_operation(&self, fd: i32, op_type: BufferOpType, size: usize);
}
```

### 3.2 Enhanced RateController

```rust
/// Interface for controllers that determine request rates during the test
pub trait RateController: ProfilingCapability + Send + Sync {
    /// Get the current target RPS
    fn get_current_rps(&self) -> u64;

    /// Update controller state based on metrics
    fn update(&self, metrics: &RequestMetrics);

    /// Update controller state with saturation information
    fn update_with_saturation(&self, metrics: &RequestMetrics, saturation: &SaturationMetrics);

    /// Called at the end of the test to get rate history
    fn get_rate_history(&self) -> Vec<(u64, u64)>;

    /// Get rate adjustments due to saturation
    fn get_saturation_adjustments(&self) -> Vec<(u64, SaturationAction)>;
}

/// Modified PID controller that is saturation-aware
pub struct SaturationAwarePidController {
    /// Standard PID controller fields
    /* ... */

    /// Saturation threshold for rate reduction
    saturation_threshold: f64,

    /// History of saturation-based adjustments
    saturation_adjustments: Arc<Mutex<Vec<(u64, SaturationAction)>>>,

    /// Current saturation level
    current_saturation: Arc<AtomicU8>,
}

impl RateController for SaturationAwarePidController {
    // Existing implementation

    fn update_with_saturation(&self, metrics: &RequestMetrics, saturation: &SaturationMetrics) {
        // Standard update based on metrics
        self.update(metrics);

        // Additional logic for saturation handling
        match saturation.level {
            SaturationLevel::Severe => {
                // Significantly reduce rate
                let current = self.current_rps.load(Ordering::Relaxed);
                let reduced = (current as f64 * 0.7) as u64; // 30% reduction
                self.current_rps.store(reduced, Ordering::Relaxed);

                // Record adjustment
                let timestamp = self.start_time.elapsed().as_millis() as u64;
                self.saturation_adjustments.lock().unwrap().push((
                    timestamp,
                    SaturationAction::ReduceRate(reduced)
                ));

                tracing::warn!("Severe client saturation detected! Reducing target RPS: {} -> {}",
                              current, reduced);
            },
            SaturationLevel::Moderate => {
                // Slightly reduce rate if it's currently increasing
                // ...
            },
            // Handle other levels...
        }
    }

    fn get_saturation_adjustments(&self) -> Vec<(u64, SaturationAction)> {
        self.saturation_adjustments.lock().unwrap().clone()
    }
}
```

### 3.3 Enhanced SampleRecorder

```rust
/// Interface for recording sample data from the test
pub trait SampleRecorder: ProfilingCapability + Send + Sync {
    /// Record a detailed sample
    fn record_sample(&self, sample: RequestSample);

    /// Record a sample with transaction analysis
    fn record_profiled_sample(&self, sample: RequestSample, analysis: Option<TransactionAnalysis>);

    /// Record a rate change event
    fn record_rate_change(&self, timestamp_ms: u64, new_rps: u64);

    /// Record a saturation event
    fn record_saturation_event(&self, timestamp_ms: u64, metrics: &SaturationMetrics);

    /// Finalize and save all recorded data
    async fn finalize(&self);
}

/// Enhanced sample with profiling data
#[derive(Debug, Clone)]
pub struct ProfiledRequestSample {
    /// Standard request sample fields
    pub base: RequestSample,

    /// Kernel-measured latencies (if available)
    pub kernel_latencies: Option<KernelLatencies>,

    /// Runtime overhead measurements (if available)
    pub runtime_overhead: Option<RuntimeOverhead>,
}

/// Kernel-level latency measurements
#[derive(Debug, Clone)]
pub struct KernelLatencies {
    /// TCP connection time in microseconds
    pub connect_us: u64,

    /// TLS handshake time in microseconds
    pub tls_handshake_us: Option<u64>,

    /// Time to first byte in microseconds
    pub ttfb_us: u64,

    /// Full data transfer time in microseconds
    pub transfer_us: u64,
}

/// Runtime overhead measurements
#[derive(Debug, Clone)]
pub struct RuntimeOverhead {
    /// Scheduling delay in microseconds
    pub scheduling_delay_us: u64,

    /// Connect phase overhead in microseconds
    pub connect_overhead_us: u64,

    /// TTFB overhead in microseconds
    pub ttfb_overhead_us: u64,

    /// Read processing overhead in microseconds
    pub read_overhead_us: u64,

    /// Total overhead percentage
    pub total_overhead_percentage: f64,
}
```

### 3.4 Enhanced ResultReporter

```rust
/// Interface for result reporting
pub trait ResultReporter: ProfilingCapability + Send + Sync {
    /// Called periodically during the test with the latest metrics
    fn report_progress(&self, metrics: &RequestMetrics);

    /// Called periodically with saturation metrics
    fn report_saturation(&self, saturation: &SaturationMetrics);

    /// Called at the end of the test with final results
    fn report_final_results(&self, test_stats: &TestResults);

    /// Called at the end of the test with profiling results
    fn report_profiling_results(&self, profiling_stats: &ProfilingResults);
}

/// Complete test results with profiling data
#[derive(Debug, Clone)]
pub struct TestResults {
    // Existing fields...

    /// Profiling results if available
    pub profiling_results: Option<ProfilingResults>,
}

/// Aggregated profiling results
#[derive(Debug, Clone)]
pub struct ProfilingResults {
    /// Kernel-level latency histograms by phase
    pub kernel_latencies: HashMap<TransactionPhase, Arc<hdrhistogram::Histogram<u64>>>,

    /// Runtime overhead histograms by phase
    pub runtime_overheads: HashMap<TransactionPhase, Arc<hdrhistogram::Histogram<u64>>>,

    /// Overall saturation assessment
    pub saturation_assessment: SaturationAssessment,

    /// Timeline of saturation levels
    pub saturation_timeline: Vec<(u64, SaturationLevel)>,

    /// Rate adjustments due to saturation
    pub saturation_adjustments: Vec<(u64, SaturationAction)>,

    /// Estimated true server performance
    pub true_server_stats: ServerPerformanceStats,
}

/// Server performance statistics adjusted for client overhead
#[derive(Debug, Clone)]
pub struct ServerPerformanceStats {
    /// True p50 latency in microseconds
    pub true_p50_latency_us: u64,

    /// True p90 latency in microseconds
    pub true_p90_latency_us: u64,

    /// True p99 latency in microseconds
    pub true_p99_latency_us: u64,

    /// Maximum sustainable throughput (estimated)
    pub max_sustainable_rps: u64,

    /// Client overhead at max throughput
    pub overhead_at_max_throughput: f64,
}
```

## 4. Factory and Builder Integration

### 4.1 Profiler Factory

```rust
/// Factory for creating transaction profilers
pub struct TransactionProfilerFactory;

impl TransactionProfilerFactory {
    /// Create a profiler based on the specified mode
    pub fn create(mode: ProfilingMode) -> Result<Box<dyn TransactionProfiler>, Box<dyn std::error::Error>> {
        match mode {
            ProfilingMode::None => {
                Ok(Box::new(NoopTransactionProfiler))
            },
            ProfilingMode::RuntimeOnly => {
                Ok(Box::new(RuntimeTransactionProfiler::new()))
            },
            ProfilingMode::KernelLevel | ProfilingMode::Comprehensive => {
                // Check if we have necessary permissions
                if !Self::check_ebpf_permissions() {
                    return Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "eBPF profiling requires root privileges or CAP_BPF capability"
                    )));
                }

                let mut profiler = EbpfTransactionProfiler::new(mode == ProfilingMode::Comprehensive);
                profiler.initialize()?;
                Ok(Box::new(profiler))
            }
        }
    }

    /// Check if the current process has the necessary permissions for eBPF
    fn check_ebpf_permissions() -> bool {
        // Implementation details...
        true
    }
}
```

### 4.2 LoadTestBuilder Enhancements

```rust
impl LoadTestBuilder {
    // Existing builder methods...

    /// Enable transaction profiling with specified mode
    pub fn with_transaction_profiling(mut self, mode: ProfilingMode) -> Self {
        self.profiling_mode = Some(mode);
        self
    }

    /// Enable comprehensive profiling (kernel + runtime)
    pub fn with_comprehensive_profiling(self) -> Self {
        self.with_transaction_profiling(ProfilingMode::Comprehensive)
    }

    /// Enable runtime-only profiling
    pub fn with_runtime_profiling(self) -> Self {
        self.with_transaction_profiling(ProfilingMode::RuntimeOnly)
    }

    /// Enable saturation awareness for controllers
    pub fn with_saturation_awareness(mut self) -> Self {
        self.saturation_aware = true;
        self
    }

    /// Set saturation thresholds for automatic rate adjustment
    pub fn with_saturation_thresholds(
        mut self,
        moderate_threshold: f64,
        severe_threshold: f64
    ) -> Self {
        self.saturation_moderate_threshold = moderate_threshold;
        self.saturation_severe_threshold = severe_threshold;
        self
    }

    /// Enable saturation-aware PID controller
    pub fn with_saturation_aware_pid_controller(
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
        self.saturation_aware = true;
        self
    }

    /// Build method
    pub fn build(self) -> LoadTest {
        // Create the appropriate profiler based on configuration
        let profiler = if let Some(mode) = self.profiling_mode {
            match TransactionProfilerFactory::create(mode) {
                Ok(p) => Some(Arc::new(p)),
                Err(e) => {
                    tracing::warn!("Failed to create profiler: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // Rest of build implementation...

        LoadTest {
            // Existing fields...
            profiler,
            saturation_aware: self.saturation_aware,
            // ...
        }
    }
}
```

### 4.3 LoadTest Integration

```rust
pub struct LoadTest {
    // Existing fields...

    /// Transaction profiler if enabled
    profiler: Option<Arc<dyn TransactionProfiler>>,

    /// Whether to enable saturation awareness
    saturation_aware: bool,
}

impl LoadTest {
    /// Run the load test
    pub async fn run(&self) -> LoadTestResult<TestResults> {
        // Create channels
        let (ticket_tx, ticket_rx) = mpsc::channel(1000);
        let (result_tx, result_rx) = mpsc::channel(1000);
        let (metrics_tx, metrics_rx) = mpsc::channel(100);
        let (saturation_tx, saturation_rx) = mpsc::channel(10);

        // Setup saturation monitoring if applicable
        let saturation_monitor = if self.saturation_aware && self.profiler.is_some() {
            let profiler = self.profiler.clone().unwrap();
            let tx = saturation_tx.clone();

            Some(tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(1000));
                loop {
                    interval.tick().await;
                    let metrics = profiler.get_saturation_metrics();
                    if tx.send(metrics).await.is_err() {
                        break;
                    }
                }
            }))
        } else {
            None
        };

        // Start the scheduler with profiler
        let scheduler_handle = tokio::spawn({
            let config = self.config.clone();
            let controller = self.rate_controller.clone();
            let profiler = self.profiler.clone();

            async move {
                self.scheduler.run(
                    config,
                    controller,
                    ticket_tx,
                    metrics_rx,
                    profiler,
                ).await;
            }
        });

        // Start the executor with profiler
        let executor_handle = tokio::spawn({
            let config = self.config.clone();
            let client = self.http_client.clone();
            let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent));
            let profiler = self.profiler.clone();

            async move {
                self.executor.run(
                    config,
                    client,
                    semaphore,
                    ticket_rx,
                    result_tx,
                    profiler,
                ).await;
            }
        });

        // Start the results collector with saturation awareness
        let collector_handle = tokio::spawn({
            let collector = self.results_collector.clone();

            async move {
                collector.run(result_rx, metrics_tx, saturation_rx).await;
            }
        });

        // Wait for all components to complete
        // ...

        // Aggregate results including profiling data
        // ...

        // Cleanup
        if let Some(profiler) = &self.profiler {
            profiler.shutdown();
        }

        Ok(final_results)
    }
}
```

## 5. Analysis and Reporting Pipeline

### 5.1 Saturation-Aware Analysis

```rust
/// Perform advanced analysis on collected data including saturation impact
pub fn analyze_with_profiling(
    test_results: &TestResults,
    profiling_data: Option<&ProfilingResults>,
) -> EnhancedAnalysis {
    if profiling_data.is_none() {
        return EnhancedAnalysis {
            // Basic analysis without profiling data
            // ...
        };
    }

    let profiling = profiling_data.unwrap();

    // Calculate true server performance by removing client overhead
    let true_latency_p50 = calculate_true_latency(
        test_results.actual_latency_histogram.value_at_percentile(50.0),
        profiling.runtime_overheads.values(),
        50.0
    );

    let true_latency_p99 = calculate_true_latency(
        test_results.actual_latency_histogram.value_at_percentile(99.0),
        profiling.runtime_overheads.values(),
        99.0
    );

    // Estimate maximum sustainable RPS
    let max_sustainable_rps = estimate_max_sustainable_rps(
        test_results,
        profiling
    );

    // Generate phase-by-phase breakdown
    let phase_breakdown = generate_phase_breakdown(profiling);

    // Full profiling analysis
    EnhancedAnalysis {
        true_server_perf: ServerPerformanceStats {
            true_p50_latency_us: true_latency_p50,
            true_p90_latency_us: true_latency_p90,
            true_p99_latency_us: true_latency_p99,
            max_sustainable_rps,
            overhead_at_max_throughput: profiling.saturation_assessment.overhead_at_peak,
        },
        phase_breakdown,
        saturation_analysis: profiling.saturation_assessment.clone(),
        overhead_by_concurrency: analyze_overhead_by_concurrency(test_results, profiling),
        // Additional analysis...
    }
}
```

### 5.2 Enhanced Report Generation

```rust
impl CsvReporter {
    /// Generate enhanced CSV reports with profiling data
    pub fn generate_profiling_reports(&self, test_results: &TestResults) -> Result<(), Box<dyn Error>> {
        if let Some(profiling) = &test_results.profiling_results {
            // Generate kernel vs runtime latency report
            self.generate_kernel_runtime_comparison(profiling)?;

            // Generate saturation timeline
            self.generate_saturation_timeline(profiling)?;

            // Generate phase breakdown
            self.generate_phase_breakdown(profiling)?;

            // Generate true server performance report
            self.generate_true_server_performance(profiling)?;
        }

        Ok(())
    }

    fn generate_kernel_runtime_comparison(&self, profiling: &ProfilingResults) -> Result<(), Box<dyn Error>> {
        let path = self.output_dir.join(format!("{}_kernel_vs_runtime.csv", self.base_filename));
        let mut writer = csv::Writer::from_path(path)?;

        writer.write_record(&[
            "phase",
            "kernel_p50_us",
            "kernel_p90_us",
            "kernel_p99_us",
            "runtime_overhead_p50_us",
            "runtime_overhead_p90_us",
            "runtime_overhead_p99_us",
            "overhead_percentage",
        ])?;

        for phase in &[
            TransactionPhase::Connect,
            TransactionPhase::TlsHandshake,
            TransactionPhase::TTFB,
            TransactionPhase::ResponseReceive,
        ] {
            if let (Some(kernel_hist), Some(runtime_hist)) = (
                profiling.kernel_latencies.get(phase),
                profiling.runtime_overheads.get(phase)
            ) {
                let kernel_p50 = kernel_hist.value_at_percentile(50.0);
                let kernel_p90 = kernel_hist.value_at_percentile(90.0);
                let kernel_p99 = kernel_hist.value_at_percentile(99.0);

                let runtime_p50 = runtime_hist.value_at_percentile(50.0);
                let runtime_p90 = runtime_hist.value_at_percentile(90.0);
                let runtime_p99 = runtime_hist.value_at_percentile(99.0);

                let total_p50 = kernel_p50 + runtime_p50;
                let overhead_percentage = if total_p50 > 0 {
                    (runtime_p50 as f64 / total_p50 as f64) * 100.0
                } else {
                    0.0
                };

                writer.write_record(&[
                    format!("{:?}", phase),
                    kernel_p50.to_string(),
                    kernel_p90.to_string(),
                    kernel_p99.to_string(),
                    runtime_p50.to_string(),
                    runtime_p90.to_string(),
                    runtime_p99.to_string(),
                    format!("{:.2}", overhead_percentage),
                ])?;
            }
        }

        Ok(())
    }

    // Other report generation methods...
}
```

## 6. Usage Examples with Enhanced Design

```rust
// Example 1: Basic HTTP load test with runtime-only profiling
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_initial_rps(500)
    .with_duration(120)
    .with_max_concurrent(200)
    .with_runtime_profiling()  // Enable runtime profiling
    .build();

// Example 2: Advanced load test with comprehensive kernel-level profiling
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_initial_rps(100)
    .with_duration(300)
    .with_max_concurrent(200)
    .with_comprehensive_profiling()  // Enable kernel-level profiling
    .with_saturation_awareness()     // Enable saturation detection
    .build();

// Example 3: PID controller with saturation awareness
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_saturation_aware_pid_controller(
        100.0,  // Target P99 latency of 100ms
        0.1,    // kp
        0.01,   // ki
        0.001,  // kd
        0.3     // Adjust rate if saturation overhead > 30%
    )
    .with_comprehensive_profiling()
    .with_duration(600)
    .with_max_concurrent(500)
    .build();

// Example 4: Enhanced reporting with runtime overhead compensation
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_initial_rps(1000)
    .with_duration(600)
    .with_max_concurrent(1000)
    .with_comprehensive_profiling()
    .with_saturation_thresholds(0.2, 0.5) // 20% moderate, 50% severe
    .with_reporter(
        EnhancedReporterBuilder::new()
            .with_profiling_reports()
            .with_overhead_compensation(true)
            .with_realtime_dashboard(true)
            .build()
    )
    .build();
```

## 7. Implementation Challenges and Considerations

### 7.1 Permission Requirements

- eBPF profiling requires root privileges or `CAP_BPF` capability
- The framework should gracefully degrade to runtime-only profiling if permissions are insufficient

### 7.2 Cross-Platform Compatibility

- Full eBPF support is primarily available on Linux 5.8+
- Consider fallback mechanisms for other operating systems
- Runtime profiling should work universally

### 7.3 Performance Impact

- eBPF probes have minimal but non-zero overhead
- Consider sampling approaches for very high throughput tests
- Provide configuration for profiling sampling rate

### 7.4 Socket FD Extraction

- Getting socket file descriptors from hyper/tokio requires custom transport layers
- Consider implementing a custom connector for hyper that exposes socket FDs

### 7.5 Coordination with Tokio Runtime

- Tokio executor scheduling impacts measurements
- Consider coordination with tokio internal metrics if available
- Use runtime instrumentation to track executor state

## 8. Conclusion

The enhanced design integrates comprehensive transaction profiling into the load testing framework, enabling:

1. **Accurate Measurement**: Separate true network/server latency from client runtime artifacts
2. **Saturation Detection**: Identify when the client becomes the bottleneck
3. **Adaptive Testing**: Automatically adjust test parameters based on saturation
4. **Advanced Analysis**: Provide deeper insights into performance characteristics
5. **True Server Performance**: Estimate actual server metrics independent of client overhead

This redesign maintains the flexibility and extensibility of the original architecture while adding powerful profiling capabilities that dramatically improve the accuracy and utility of load testing results.
