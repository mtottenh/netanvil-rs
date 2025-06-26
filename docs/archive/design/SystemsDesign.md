# Network Load Testing Framework with Advanced Profiling

## 1. System Architecture Overview

The load testing framework is designed as a composable system with specialized components that work together to generate, execute, and measure network load. The architecture supports advanced features including kernel-level profiling, coordinated omission prevention, and distributed load generation.

### 1.1 Core System Components

![System Architecture]()

1. **Rate Controllers**: Determine and adjust request rates throughout the test based on different strategies.

2. **Request Schedulers**: Schedule individual requests according to the current rate, with proper timing distributions.

3. **Request Generators/Transformers**: Create and modify request specifications before execution.

4. **Request Executors**: Execute HTTP/HTTPS requests against target systems, with configurable network properties.

5. **Transaction Profilers**: Collect detailed metrics at both kernel and runtime levels.

6. **Results Collectors**: Collect and aggregate metrics from completed requests.

7. **Sample Recorders**: Record detailed samples for later analysis.

8. **Result Reporters**: Format and present test results during and after the test.

9. **Distributed Coordinators**: Enable multi-node load generation with centralized control.

### 1.2 Design Principles

The framework is designed around several key principles:

1. **Composability**: Components are designed with well-defined interfaces that can be combined in different ways.

2. **Separation of Concerns**: Each component focuses on a specific aspect of the load testing process.

3. **Extensibility**: The trait-based design makes it easy to implement new components that extend functionality.

4. **Accuracy**: The framework includes features to prevent coordinated omission and provides true server performance metrics.

5. **Adaptability**: Components can adjust behavior based on feedback during the test execution.

### 1.3 Request Pipeline Architecture

The request pipeline processes each request through several stages:

```
Request Generation → Transform → Schedule → Execute → Profile → Collect → Record → Report
```

Each stage is handled by specialized components with well-defined interfaces, allowing for flexible composition and extension.

## 2. Core Trait Definitions

### 2.1 Rate Control

Rate controllers determine how many requests per second should be generated during the test.

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

/// External control signal interface
pub trait ExternalControlSignal: Send + Sync {
    /// Get the current external control value
    fn get_current_value(&self) -> f64;

    /// Register a callback for external signal updates
    fn register_update_callback(&mut self, callback: Box<dyn Fn(f64) + Send + Sync>);

    /// Get the signal name or description
    fn get_name(&self) -> &str;

    /// Get historical values with timestamps (for reporting)
    fn get_history(&self) -> Vec<(u64, f64)>;
}
```

#### 2.1.1 Saturation-Aware Controllers

Controllers can also take client saturation into account when adjusting rates:

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
    /// Recommended actions based on saturation level
    pub recommendation: SaturationAction,
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

### 2.2 Request Scheduling

Schedulers determine when requests are issued to maintain the target rate with proper timing models.

```rust
/// Interface for scheduling requests
pub trait RequestScheduler: ProfilingCapability + Send + Sync {
    /// Run the scheduler until the test is complete
    async fn run(
        &self,
        config: Arc<LoadTestConfig>,
        rate_controller: Arc<dyn RateController>,
        ticket_tx: tokio::sync::mpsc::Sender<RequestTicket>,
        metrics_rx: tokio::sync::mpsc::Receiver<RequestMetrics>,
        profiler: Option<Arc<dyn TransactionProfiler>>,
    );
}

/// A ticket representing a scheduled request
#[derive(Debug, Clone)]
pub struct RequestTicket {
    /// Unique identifier for this request
    pub id: u64,
    /// When this request SHOULD be sent according to the schedule
    pub scheduled_time: Instant,
    /// Which URL to request (index into config.urls)
    pub url_index: usize,
    /// Whether this is a warmup request (not counted in stats)
    pub is_warmup: bool,
}

/// Types of request schedulers
pub enum SchedulerType {
    /// Generates requests at constant intervals
    ConstantRate,
    /// Generates requests with exponentially distributed inter-arrival times (Poisson process)
    Poisson,
}
```

### 2.3 Request Generation & Transformation Pipeline

Request generation and transformation handles creating and modifying the request specifications.

```rust
/// Interface for generating URLs based on patterns
pub trait UrlPatternGenerator: Send + Sync {
    /// Generate a URL for the given request
    fn generate_url(&self, ticket: &RequestTicket) -> String;

    /// Get a description of this pattern for logging
    fn get_pattern_description(&self) -> String;
}

/// Full request specification
#[derive(Debug, Clone)]
pub struct RequestSpec {
    /// HTTP method (GET, POST, etc.)
    pub method: String,
    /// URL or URL pattern
    pub url_spec: UrlSpec,
    /// Headers to include
    pub headers: Vec<(String, String)>,
    /// Request body (if any)
    pub body: Option<RequestBody>,
    /// Simulated client IP configuration
    pub client_ip: Option<ClientIpSpec>,
}

/// URL specification - either static or pattern-based
#[derive(Debug, Clone)]
pub enum UrlSpec {
    /// Static URL
    Static(String),
    /// Dynamic URL from pattern generator
    Pattern(Box<dyn UrlPatternGenerator + Send + Sync>),
}

/// Request generation capability
pub trait RequestGenerator: Send + Sync {
    /// Generate a request specification based on a ticket
    fn generate(&self, ticket: &RequestTicket) -> RequestSpec;

    /// Get statistics about generated requests
    fn get_stats(&self) -> RequestGenerationStats;
}

/// Interface for transforming requests before execution
pub trait RequestTransformer: Send + Sync {
    /// Transform a request specification
    fn transform(&self, spec: RequestSpec) -> RequestSpec;

    /// Get the transformer name/description
    fn get_name(&self) -> &str;

    /// Get a reference to the transformer as Any for downcasting
    fn as_any(&self) -> &dyn std::any::Any;
}
```

### 2.4 Request Execution

Executors handle the actual execution of HTTP requests against the target systems.

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
        // Optional profiler for detailed transaction analysis
        profiler: Option<Arc<dyn TransactionProfiler>>,
    ) where
        C: hyper::client::connect::Connect + Clone + Send + Sync + 'static;
}

/// Completed request with timing information
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
```

### 2.5 Transaction Profiling

The profiling system captures detailed metrics at both kernel and runtime levels to provide accurate performance analysis.

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
```

### 2.6 Results Collection and Recording

Results collectors aggregate metrics from completed requests for analysis and reporting.

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

/// A sample point for detailed analysis
#[derive(Debug, Clone)]
pub struct RequestSample {
    /// Test elapsed time when request completed (ms)
    pub timestamp: u64,
    /// Which URL was requested
    pub url_index: usize,
    /// URL that was requested
    pub url: String,
    /// When the request was scheduled (ms since test start)
    pub scheduled_time: u64,
    /// When the request was actually sent (ms since test start)
    pub start_time: u64,
    /// When the response completed (ms since test start)
    pub end_time: u64,
    /// Latency from scheduled time to completion (μs)
    pub intended_latency: u64,
    /// Latency from actual start to completion (μs)
    pub actual_latency: u64,
    /// HTTP status code
    pub status_code: u16,
    /// Whether this was an error
    pub is_error: bool,
}

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

/// Complete test results
#[derive(Debug, Clone)]
pub struct TestResults {
    /// Total number of warmup requests sent
    pub warmup_requests: u64,
    /// Total number of measured requests
    pub total_requests: u64,
    /// Number of errors encountered
    pub error_count: u64,
    /// Distribution of status codes (code -> count)
    pub status_counts: HashMap<u16, u64>,
    /// Histogram for intended-to-completion latencies
    pub intended_latency_histogram: Arc<hdrhistogram::Histogram<u64>>,
    /// Histogram for actual-to-completion latencies
    pub actual_latency_histogram: Arc<hdrhistogram::Histogram<u64>>,
    /// Per-URL histograms
    pub url_histograms: Vec<Arc<hdrhistogram::Histogram<u64>>>,
    /// History of rate changes over time
    pub rate_history: Vec<(u64, u64)>,
    /// Profiling results if available
    pub profiling_results: Option<ProfilingResults>,
}
```

### 2.7 Configuration

The overall load test configuration structure:

```rust
/// Configuration for the load test
#[derive(Debug, Clone)]
pub struct LoadTestConfig {
    /// Initial target RPS
    pub initial_rps: u64,
    /// Minimum RPS limit for controllers
    pub min_rps: u64,
    /// Maximum RPS limit for controllers
    pub max_rps: u64,
    /// Total test duration in seconds
    pub test_duration_secs: u64,
    /// Maximum number of concurrent requests
    pub max_concurrent: usize,
    /// URLs to test (with different cache characteristics)
    pub urls: Vec<String>,
    /// Time to spend warming up the cache before measuring
    pub cache_warmup_time: u64,
    /// Rate control mode to use
    pub rate_control_mode: RateControlMode,
    /// Sample recording configuration
    pub sampling_config: SamplingConfig,
    /// Profiling configuration if enabled
    pub profiling_config: Option<ProfilingConfig>,
    /// Distributed testing configuration
    pub distributed_config: Option<DistributedConfig>,
}

/// Profiling configuration
#[derive(Debug, Clone)]
pub struct ProfilingConfig {
    /// Profiling mode
    pub mode: ProfilingMode,
    /// Whether to enable saturation awareness
    pub saturation_aware: bool,
    /// Sampling rate for profiling (0.0-1.0)
    pub sampling_rate: f64,
    /// Moderate saturation threshold
    pub moderate_saturation_threshold: f64,
    /// Severe saturation threshold
    pub severe_saturation_threshold: f64,
}
```

## 3. Distributed Load Testing Architecture

The framework supports distributed load generation with centralized control.

### 3.1 Distributed Coordinator Trait

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

### 3.2 Controller Extension for Distributed Testing

```rust
/// Extension trait for distributed rate controllers
pub trait DistributedRateController: RateController {
    /// Initialize with distributed coordinator
    fn initialize_distributed(&mut self, coordinator: Arc<dyn DistributedCoordinator>);

    /// Adjust local RPS based on global assignment
    fn adjust_to_assignment(&self, assignment: &NodeAssignment);

    /// Process global control signals
    fn process_global_signals(&self, signals: &HashMap<String, f64>);

    /// Get status for reporting to coordinator
    fn get_node_status(&self) -> NodeStatus;
}
```

### 3.3 Integration with External Control Signal

```rust
/// Network-based external control signal
pub struct NetworkControlSignal {
    /// Signal name
    name: String,
    /// Coordinator for communication
    coordinator: Arc<dyn DistributedCoordinator>,
    /// Current value
    current_value: Arc<Mutex<f64>>,
    /// Signal history
    history: Arc<Mutex<Vec<(u64, f64)>>>,
    /// Start time for timestamps
    start_time: Instant,
    /// Update interval
    update_interval: Duration,
    /// Callbacks to invoke on updates
    callbacks: Arc<Mutex<Vec<Box<dyn Fn(f64) + Send + Sync>>>>,
}

impl NetworkControlSignal {
    /// Create a new network control signal
    pub fn new(
        name: &str,
        coordinator: Arc<dyn DistributedCoordinator>,
        initial_value: f64,
        update_interval: Duration,
    ) -> Self {
        let signal = Self {
            name: name.to_string(),
            coordinator,
            current_value: Arc::new(Mutex::new(initial_value)),
            history: Arc::new(Mutex::new(vec![(0, initial_value)])),
            start_time: Instant::now(),
            update_interval,
            callbacks: Arc::new(Mutex::new(Vec::new())),
        };

        // Start background thread to poll for updates
        signal.start_polling();

        signal
    }

    /// Start polling for updates
    fn start_polling(&self) {
        // Implementation details...
    }
}

impl ExternalControlSignal for NetworkControlSignal {
    // Implementation of ExternalControlSignal trait...
}
```

## 4. Enhanced Load Test Builder

The `LoadTestBuilder` provides a fluent interface for configuring load tests:

```rust
pub struct LoadTestBuilder {
    config: LoadTestConfig,
    rate_controller: Option<Box<dyn RateController>>,
    scheduler_type: SchedulerType,
    use_custom_executor: bool,
    custom_headers: Vec<(String, String)>,
    apply_random_delays: bool,
    min_delay_ms: u64,
    max_delay_ms: u64,
    profiling_mode: Option<ProfilingMode>,
    saturation_aware: bool,
    saturation_moderate_threshold: f64,
    saturation_severe_threshold: f64,
    distributed_config: Option<DistributedConfig>,
    url_generator: Option<Box<dyn UrlPatternGenerator + Send + Sync>>,
    request_transformers: Vec<Box<dyn RequestTransformer>>,
    response_processors: Vec<Box<dyn ResponseProcessor>>,
    external_signal: Option<Box<dyn ExternalControlSignal + Send + Sync>>,
}

impl LoadTestBuilder {
    /// Create a new builder with default settings
    pub fn new() -> Self {
        // Implementation...
    }

    /// Set the test configuration
    pub fn with_config(mut self, config: LoadTestConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the URLs to test
    pub fn with_urls(mut self, urls: Vec<String>) -> Self {
        self.config.urls = urls;
        self
    }

    /// Set the initial request rate
    pub fn with_initial_rps(mut self, rps: u64) -> Self {
        self.config.initial_rps = rps;
        self
    }

    /// Set the test duration in seconds
    pub fn with_duration(mut self, duration_secs: u64) -> Self {
        self.config.test_duration_secs = duration_secs;
        self
    }

    /// Set the maximum concurrent requests
    pub fn with_max_concurrent(mut self, max_concurrent: usize) -> Self {
        self.config.max_concurrent = max_concurrent;
        self
    }

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

    /// Use a saturation-aware PID controller
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

    /// Enable transaction profiling with specified mode
    pub fn with_transaction_profiling(mut self, mode: ProfilingMode) -> Self {
        self.profiling_mode = Some(mode);
        self
    }

    /// Enable distributed testing with coordinator
    pub fn with_distributed_coordinator(
        mut self,
        coordinator_url: String,
        node_id: String,
    ) -> Self {
        self.distributed_config = Some(DistributedConfig {
            coordinator_url,
            node_id,
            role: DistributedRole::Worker,
        });
        self
    }

    /// Create a channel-based external control signal
    pub fn with_channel_control_signal(
        mut self,
        name: &str,
        initial_value: f64
    ) -> (Self, tokio::sync::mpsc::Sender<f64>) {
        let (signal, tx) = ChannelControlSignal::new(name, initial_value);
        self.external_signal = Some(Box::new(signal));
        (self, tx)
    }

    /// Use a Poisson process for request scheduling
    pub fn with_poisson_scheduler(mut self) -> Self {
        self.scheduler_type = SchedulerType::Poisson;
        self
    }

    /// Build the load test
    pub fn build(self) -> LoadTest {
        // Create components based on configuration
        // ...

        LoadTest {
            config: self.config.clone(),
            rate_controller,
            scheduler,
            executor,
            results_collector,
            profiler,
            distributed_coordinator,
            // ...
        }
    }
}
```

## 5. Distributed Testing Implementation

### 5.1 Coordinator Implementation

```rust
/// HTTP-based distributed coordinator
pub struct HttpDistributedCoordinator {
    /// Base URL for the coordinator service
    base_url: String,
    /// This node's ID
    node_id: String,
    /// HTTP client
    client: reqwest::Client,
    /// Node information
    node_info: NodeInfo,
    /// Current node assignment
    assignment: Arc<RwLock<Option<NodeAssignment>>>,
    /// Last reported status
    last_status: Arc<RwLock<Option<NodeStatus>>>,
    /// Global test status
    global_status: Arc<RwLock<Option<GlobalTestStatus>>>,
    /// Background task handle
    background_task: Arc<Mutex<Option<JoinHandle<()>>>>,
}
```

### 5.2 Protocol for Distributed Load Generation

1. **Registration Phase**:
   - Worker nodes register with coordinator
   - Coordinator assigns initial load distribution
   - Workers acknowledge assignments

2. **Synchronization Phase**:
   - Coordinator ensures all nodes are ready
   - Synchronizes test start time across nodes

3. **Execution Phase**:
   - Workers report metrics periodically
   - Coordinator redistributes load based on saturation
   - Control signals are propagated to all nodes

4. **Termination Phase**:
   - Workers report final results
   - Coordinator aggregates and presents unified report

### 5.3 Load Distribution Algorithms

The coordinator uses different algorithms to distribute load:

1. **Even Distribution**:
   - Assigns equal RPS to all nodes
   - Simple baseline approach

2. **Capability-Based**:
   - Distributes load proportional to node capabilities
   - Takes max_rps and max_concurrent into account

3. **Saturation-Aware**:
   - Dynamically adjusts based on node saturation
   - Reduces load on saturated nodes

4. **Proximity-Based**:
   - Assigns requests based on network proximity
   - Reduces latency variation

## 6. Request Pipeline Integration

### 6.1 Pipeline Component Composition

```rust
/// Request execution pipeline
pub struct RequestPipeline {
    /// Request generators for creating base requests
    generators: Vec<Box<dyn RequestGenerator>>,
    /// Request transformers for modifying requests
    transformers: Vec<Box<dyn RequestTransformer>>,
    /// Connection manager for handling connections
    connection_manager: Box<dyn EnhancedConnectionManager>,
    /// Response processors for analyzing responses
    response_processors: Vec<Box<dyn ResponseProcessor>>,
    /// Transaction profiler
    profiler: Option<Arc<dyn TransactionProfiler>>,
    /// HTTP client for executing requests
    client: Option<Client<Box<dyn HttpConnector>, Body>>,
}

impl RequestPipeline {
    /// Execute a request through the pipeline
    pub async fn execute(&self, ticket: RequestTicket) -> Result<CompletedRequest, Box<dyn Error>> {
        // 1. Generate base request
        let base_generator = self.generators.first()
            .ok_or("No request generator configured")?;
        let mut spec = base_generator.generate(&ticket);

        // 2. Apply transformers in sequence
        for transformer in &self.transformers {
            spec = transformer.transform(spec);
        }

        // 3. Start profiling if enabled
        if let Some(profiler) = &self.profiler {
            profiler.record_runtime_event(RuntimeEvent {
                timestamp: Instant::now(),
                event_type: RuntimeEventType::ConnectStart,
                request_id: ticket.id,
                data: RuntimeEventData::None,
            });
        }

        // 4. Build and execute HTTP request
        let request = self.build_http_request(spec.clone())?;
        let start_time = Instant::now();
        let response = self.client.as_ref().unwrap().request(request).await?;

        // 5. Process response with all processors
        let mut response_metrics = ResponseMetrics::new();
        for processor in &self.response_processors {
            processor.process(&response, &mut response_metrics);
        }

        // 6. Read response body
        let body_bytes = hyper::body::to_bytes(response.into_body()).await?;

        // 7. Complete profiling and analyze if enabled
        let transaction_analysis = if let Some(profiler) = &self.profiler {
            profiler.record_runtime_event(RuntimeEvent {
                timestamp: Instant::now(),
                event_type: RuntimeEventType::ResponseComplete,
                request_id: ticket.id,
                data: RuntimeEventData::DataSize(body_bytes.len()),
            });

            Some(profiler.analyze_transaction(ticket.id))
        } else {
            None
        };

        // 8. Create completed request
        let completed = CompletedRequest {
            ticket,
            start_time,
            end_time: Instant::now(),
            status_code: response.status().as_u16(),
            is_error: !response.status().is_success(),
            response_size: body_bytes.len() as u64,
            transaction_analysis,
        };

        Ok(completed)
    }
}
```

## 7. Example Usage Scenarios

### 7.1 Basic Load Testing

```rust
// Example 1: Basic HTTP load test
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_initial_rps(500)
    .with_duration(120)
    .with_max_concurrent(200)
    .build();

let results = load_test.run().await.unwrap();
```

### 7.2 PID Controller with Latency Target

```rust
// Example 2: PID controller targeting 100ms P99 latency
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_initial_rps(100)
    .with_duration(300)
    .with_pid_controller(100.0, 0.1, 0.01, 0.001)
    .with_poisson_scheduler()
    .build();

let results = load_test.run().await.unwrap();
```

### 7.3 Advanced Profiling with Saturation Awareness

```rust
// Example 3: Advanced load test with comprehensive profiling
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_initial_rps(100)
    .with_duration(300)
    .with_max_concurrent(200)
    .with_transaction_profiling(ProfilingMode::Comprehensive)
    .with_saturation_aware_pid_controller(
        100.0,  // Target P99 latency of 100ms
        0.1,    // kp
        0.01,   // ki
        0.001,  // kd
        0.3     // Adjust rate if saturation overhead > 30%
    )
    .build();

let results = load_test.run().await.unwrap();
```

### 7.4 Distributed Load Testing

```rust
// Example 4: Distributed load testing as worker node
let load_test = LoadTestBuilder::new()
    .with_distributed_coordinator(
        "https://coordinator.example.com".to_string(),
        "worker-node-1".to_string()
    )
    .with_transaction_profiling(ProfilingMode::RuntimeOnly)
    .with_saturation_awareness()
    .build();

let results = load_test.run().await.unwrap();
```

### 7.5 External Control Signal Integration

```rust
// Example 5: Using external control signal
let (mut builder, control_tx) = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_channel_control_signal("cpu-utilization", 50.0);  // Initial target: 50% CPU

let load_test = builder
    .with_initial_rps(100)
    .with_duration(300)
    .with_max_concurrent(200)
    .with_pid_controller(50.0, 0.1, 0.01, 0.001)
    .with_pid_metric_type(MetricType::ExternalSignal)
    .build();

// In another thread or process, update the control signal
tokio::spawn(async move {
    // Start test with 50% CPU target
    tokio::time::sleep(Duration::from_secs(60)).await;

    // After 1 minute, change target to 70% CPU
    control_tx.send(70.0).await.unwrap();

    // After another minute, change target to 30% CPU
    tokio::time::sleep(Duration::from_secs(60)).await;
    control_tx.send(30.0).await.unwrap();
});

let results = load_test.run().await.unwrap();
```

## 8. Distributed Load Coordination Flow

![Distributed Coordination]

1. **Node Registration**:
   - Worker nodes register with capabilities
   - Coordinator evaluates total test capacity

2. **Initial Assignment**:
   - Coordinator assigns initial load
   - Nodes acknowledge assignments

3. **Test Execution**:
   - Periodic status reports from nodes
   - Dynamic load balancing based on saturation
   - External signals propagated to all nodes

4. **Final Results**:
   - Nodes report detailed results
   - Coordinator aggregates and analyzes
   - Unified report generation

## 9. Integration with eBPF Profiling

For systems that support it, eBPF profiling provides kernel-level insights:

![eBPF Integration]

The eBPF profiler:

1. Attaches to kernel socket operations
2. Captures precise timing of network events
3. Correlates kernel events with runtime events
4. Calculates true network vs. runtime overhead
5. Provides saturation metrics for rate control

## 10. Implementation Considerations

### 10.1 Platform Compatibility

- Full eBPF profiling requires Linux 5.8+
- Runtime profiling works on all platforms
- Graceful fallback to runtime-only profiling

### 10.2 Performance Impact

- eBPF probes have minimal but non-zero overhead
- Consider sampling rate for high-throughput tests
- Monitor for profiling impact on test accuracy

### 10.3 Permissions Requirements

- eBPF profiling requires root privileges or CAP_BPF capability
- Connection socket access may require elevated permissions
- Check capabilities at startup and degrade gracefully

### 10.4 Scaling Considerations

- Coordinator becomes potential bottleneck in large-scale tests
- Consider hierarchical coordination for very large deployments
- Monitor coordinator resource usage during tests

## 11. Conclusion

This framework provides a comprehensive solution for accurate network load testing with:

1. **Coordinated Omission Prevention**: Through proper request scheduling and timing
2. **True Performance Metrics**: By separating runtime overhead from network latency
3. **Saturation Awareness**: To prevent false conclusions from client bottlenecks
4. **Distributed Testing**: For generating high loads with accurate measurement
5. **Extensible Architecture**: To accommodate different testing requirements

The trait-based design ensures components can be extended, replaced, or composed to meet specific testing needs, while the profiling capabilities provide unprecedented insight into true system performance.
