# Network Load Testing Framework: Architecture Overview

## 1. System Architecture Overview

The load testing framework is designed as a composable system with specialized components that work together to generate, execute, and measure network load. The architecture supports advanced features including kernel-level profiling, coordinated omission prevention, and distributed load generation, with protocols including HTTP/1.1 and HTTP/2.

### 1.1 Core System Components and Responsibilities

Each component has a specific, well-defined responsibility:

1. **Rate Controller**:
   - Determines the overall load level (requests per second)
   - Receives feedback from metrics to adjust rates
   - Communicates load targets to the Client Session Manager
   - Supports multiple control strategies (PID, step functions, etc.)

2. **Client Session Manager**:
   - Creates and manages simulated client sessions
   - Generates request tickets with scheduled timestamps
   - Applies realistic client behavior patterns
   - Uses statistical distributions to model realistic traffic
   - Assigns client types, IPs, and session attributes
   - Manages session lifecycles (creation, activity, termination)

3. **Request Scheduler**:
   - Dispatches tickets at their scheduled times
   - Applies unbiased sampling decisions (Bernoulli sampling)
   - Ensures timing precision and coordinated omission prevention
   - Detects and reports backpressure

4. **Request Generator**:
   - Creates request specifications based on tickets and context
   - Applies URL patterns and request parameters
   - Generates appropriate HTTP methods and bodies

5. **Request Transformer**:
   - Modifies request specifications before execution
   - Adds headers, authentication, cookies, etc.
   - Implements protocol-specific transformations
   - Ensures session consistency across requests

6. **Request Executor**:
   - Executes HTTP requests against target systems
   - Manages connection lifecycles
   - Applies network condition simulation
   - Handles protocol-specific behaviors (HTTP/1.1, HTTP/2)
   - Tracks timing information

7. **Transaction Profiler**:
   - Monitors system at kernel and runtime levels
   - Provides saturation metrics and detection
   - Analyzes transaction performance
   - Identifies bottlenecks and resource constraints

8. **Results Collector**:
   - Aggregates metrics from completed requests
   - Maintains histograms and statistics
   - Provides real-time metrics during test
   - Generates final test results

9. **Sample Recorder**:
   - Records detailed samples for selected requests
   - Works with scheduler's sampling decisions
   - Preserves full context for analysis
   - Maintains unbiased sample selection

10. **Network Condition Simulator**:
    - Simulates realistic network conditions
    - Applies bandwidth limits, latency, and packet loss
    - Ensures consistency within sessions
    - Models different client network environments

### 1.2 Pipeline Architecture

The framework's pipeline architecture follows this flow:

```
Rate Control → Client Session Management → Scheduling → Request Pipeline → Results Collection
                                                           ↓
                                    [Generation → Transformation → Execution]
```

Where:
- **Rate Control** signals the Client Session Manager how many sessions/requests to generate
- **Client Session Management** creates sessions and generates tickets with scheduled timestamps
- **Request Scheduling** dispatches those tickets at their scheduled times
- **Request Pipeline** processes each ticket through generation, transformation, and execution
- **Results Collection** aggregates metrics from completed requests

### 1.3 Component Interaction Model

The components interact through a clear flow of data and control:

1. **Rate Controller → Client Session Manager**:
   - Rate controller determines target RPS
   - Signals session manager to adjust session creation/activity

2. **Client Session Manager → Request Scheduler**:
   - Session manager generates tickets with scheduled timestamps
   - Sends tickets to scheduler's queue
   - Each ticket includes session context and intended execution time

3. **Request Scheduler → Request Pipeline**:
   - Scheduler dispatches tickets at their scheduled times
   - Makes unbiased sampling decisions
   - Provides scheduled vs. actual timing information

4. **Request Pipeline → Results Collection**:
   - Pipeline sends completed requests to collector
   - Includes timing information and response details
   - Marked tickets are sent to sample recorder with additional context

This interaction model ensures a clear flow of control and data through the system, with each component having a distinct responsibility.

### 1.4 Implementation Architecture: Static Dispatch

The implementation uses Configuration-Time Monomorphization to eliminate virtual dispatch overhead in critical paths:

```rust
/// Generic load test implementation with concrete types for all components
pub struct LoadTestImpl<R, C, S, G, T, E, P, L, N>
where
    R: RateController,
    C: ClientSessionManager,
    S: RequestScheduler,
    G: RequestGenerator,
    T: RequestTransformer,
    E: RequestExecutor,
    P: TransactionProfiler,
    L: ResultsCollector,
    N: NetworkConditionSimulator,
{
    // Component instances
    rate_controller: R,
    client_session_manager: C,
    scheduler: S,
    generator: G,
    transformer: T,
    executor: E,
    profiler: P,
    collector: L,
    network_simulator: N,
    config: LoadTestConfig,
}
```

This architecture enables:
- Zero virtual dispatch overhead in performance-critical paths
- Full compiler optimization across component boundaries
- Type safety with explicit component relationships
- Proper composition of all components
- Clear system organization and interaction model

## 2. Core Component Interfaces

### 2.1 Rate Controller

```rust
/// Interface for controllers that determine request rates during the test
pub trait RateController: MetricsProvider + Send + Sync {
    /// Get the current target RPS
    fn get_current_rps(&self) -> u64;

    /// Update controller state based on metrics
    fn update(&self, metrics: &RequestMetrics);

    /// Process external control signals if supported
    fn process_control_signals(&self, signals: &ControlSignalSet);

    /// Called at the end of the test to get rate history
    fn get_rate_history(&self) -> Vec<(u64, u64)>;

    /// Get rate adjustments with reasons
    fn get_rate_adjustments(&self) -> Vec<(u64, RateAdjustment)>;
}

/// Metric values captured during test execution
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
    /// Error rate (0.0-1.0)
    pub error_rate: f64,
    /// Current throughput (requests per second)
    pub current_throughput: f64,
}
```

Rate controllers support multiple control modes including:
- Static rates for basic load testing
- Step functions for staged load increases
- PID controllers targeting latency or error rates
- External signal controllers integrating with monitoring systems
- Saturation-aware adaptive controllers that respond to client resource constraints

#### 2.1.1 Rate Smoother

```rust
/// Interface for smoothing rate changes
pub trait RateSmoother: MetricsProvider + Send + Sync {
    /// Smooth a target RPS to avoid abrupt changes
    fn smooth_rate(&self, target_rps: u64) -> u64;

    /// Set maximum change rates
    fn set_change_limits(&mut self, relative_change_per_second: f64, absolute_change_per_second: u64);

    /// Get smoothing configuration
    fn get_config(&self) -> RateSmoothingConfig;
}

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
    pub limit_decreases: bool,
}
```

### 2.2 Client Session Manager

```rust
/// Interface for managing client sessions
pub trait ClientSessionManager: MetricsProvider + Send + Sync {
    /// Run the client session manager
    async fn run(
        &self,
        config: Arc<LoadTestConfig>,
        rate_controller: Arc<dyn RateController>,
        ticket_tx: tokio::sync::mpsc::Sender<SchedulerTicket>,
    );

    /// Update distribution of client types
    fn update_client_distribution(&self, distribution: ClientDistribution);

    /// Get current client session statistics
    fn get_session_stats(&self) -> ClientSessionStats;

    /// Signal the manager to terminate
    fn terminate(&self);
}

/// Client session statistics
#[derive(Debug, Clone)]
pub struct ClientSessionStats {
    /// Number of active sessions
    pub active_sessions: u64,
    /// Total sessions created
    pub total_sessions_created: u64,
    /// Sessions closed
    pub sessions_closed: u64,
    /// Distribution by client type
    pub distribution: HashMap<ClientType, u64>,
    /// Average session duration in seconds
    pub avg_session_duration_secs: f64,
    /// Average requests per session
    pub avg_requests_per_session: f64,
}

/// Client distribution configuration
#[derive(Debug, Clone)]
pub struct ClientDistribution {
    /// Distribution by client type
    pub distribution: HashMap<ClientType, f64>,
    /// Session creation rate per second
    pub session_creation_rate: f64,
    /// Average session duration in seconds
    pub avg_session_duration_secs: f64,
    /// Variance in session duration (coefficient of variation)
    pub session_duration_variance: f64,
}
```

#### 2.2.1 Traffic Generation Patterns

```rust
/// Traffic pattern type
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TrafficPatternType {
    /// Poisson arrivals (exponential inter-arrival times)
    Poisson,
    /// Constant rate
    ConstantRate,
    /// Bursty traffic pattern
    Bursty {
        /// Burst size
        burst_size: u32,
        /// Interval between bursts in milliseconds
        interval_ms: u32,
    },
    /// Diurnal pattern (time-of-day variation)
    Diurnal {
        /// Peak hours (0-23)
        peak_hours: Vec<u8>,
        /// Peak multiplier
        peak_multiplier: f64,
    },
    /// Custom pattern
    Custom(String),
}
```

### 2.3 Request Scheduler

```rust
/// Interface for scheduling requests
pub trait RequestScheduler: MetricsProvider + Send + Sync {
    /// Run the scheduler until the test is complete
    async fn run(
        &self,
        config: Arc<LoadTestConfig>,
        ticket_rx: tokio::sync::mpsc::Receiver<SchedulerTicket>,
        dispatch_tx: tokio::sync::mpsc::Sender<SchedulerTicket>,
    );

    /// Set the sampling rate (0.0-1.0)
    fn set_sampling_rate(&self, rate: f64);

    /// Get the current sampling rate
    fn get_sampling_rate(&self) -> f64;

    /// Get scheduler statistics
    fn get_stats(&self) -> SchedulerStats;

    /// Signal the scheduler to terminate early
    fn terminate(&self);
}

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
    /// Set by scheduler based on Bernoulli sampling
    pub should_sample: bool,
    /// Client session ID that generated this ticket
    pub client_session_id: Option<String>,
    /// Protocol-specific information
    pub protocol_info: Option<ProtocolInfo>,
}

/// Protocol-specific information
#[derive(Debug, Clone)]
pub enum ProtocolInfo {
    /// HTTP/1.1 information
    Http1 {
        /// Connection ID if using persistent connection
        connection_id: Option<String>,
    },
    /// HTTP/2 information
    Http2 {
        /// Stream ID if known
        stream_id: Option<u32>,
    },
}
```

### 2.4 Request Pipeline

The request pipeline processes tickets through generation, transformation, and execution:

```rust
/// Request pipeline interface
pub trait RequestPipeline: Send + Sync {
    /// Process a ticket through the pipeline
    async fn process(&self, ticket: SchedulerTicket) -> Result<CompletedRequest, RequestError>;

    /// Get pipeline statistics
    fn get_stats(&self) -> PipelineStats;

    /// Shut down the pipeline
    async fn shutdown(&self);
}
```

#### 2.4.1 Request Generator

```rust
/// Interface for generating requests
pub trait RequestGenerator: MetricsProvider + Send + Sync {
    /// Generate a request specification based on context
    fn generate(&self, context: &RequestContext) -> RequestSpec;

    /// Get generator configuration
    fn get_config(&self) -> GeneratorConfig;
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
    /// Request metadata for tracking
    pub metadata: RequestMetadata,
}
```

#### 2.4.2 Request Transformer

```rust
/// Interface for transforming requests
pub trait RequestTransformer: MetricsProvider + Send + Sync {
    /// Transform a request specification
    fn transform(&self, spec: RequestSpec) -> RequestSpec;

    /// Get transformer configuration
    fn get_config(&self) -> TransformerConfig;
}
```

#### 2.4.3 Request Executor

```rust
/// Interface for executing requests
pub trait RequestExecutor: MetricsProvider + Send + Sync {
    /// Execute a request
    async fn execute(
        &self,
        spec: RequestSpec,
        context: &RequestContext,
    ) -> Result<CompletedRequest, RequestError>;

    /// Shut down the executor
    async fn shutdown(&self);

    /// Get executor configuration
    fn get_config(&self) -> ExecutorConfig;
}

/// Completed request with response information
#[derive(Debug, Clone)]
pub struct CompletedRequest {
    /// The original request ID
    pub request_id: u64,
    /// Original ticket that generated this request
    pub ticket: SchedulerTicket,
    /// When request was actually sent
    pub start_time: Instant,
    /// When response was received
    pub end_time: Instant,
    /// HTTP status code returned
    pub status_code: u16,
    /// Whether an error occurred
    pub is_error: bool,
    /// Error details if any
    pub error: Option<String>,
    /// Size of the response in bytes
    pub response_size: u64,
    /// Response headers
    pub headers: HashMap<String, String>,
    /// Response body (if sampled)
    pub body: Option<Vec<u8>>,
    /// Transaction analysis from profiler (if available)
    pub transaction_analysis: Option<TransactionAnalysis>,
}
```

### 2.5 Network Condition Simulator

```rust
/// Interface for network condition simulation
pub trait NetworkConditionSimulator: MetricsProvider + Send + Sync {
    /// Get or create network condition for a session or connection
    fn get_network_condition(
        &self,
        session_id: Option<&str>,
        connection_id: &str
    ) -> NetworkCondition;

    /// Update simulator configuration
    fn update_config(&self, config: NetworkConditionConfig);

    /// Get current simulator configuration
    fn get_config(&self) -> NetworkConditionConfig;

    /// Get simulator statistics
    fn get_stats(&self) -> NetworkSimulatorStats;
}

/// Network condition configuration
#[derive(Debug, Clone)]
pub struct NetworkConditionConfig {
    /// Percentage of connections that should simulate slow conditions
    pub slow_connection_probability: f64,

    /// Whether to keep conditions consistent within a session
    pub consistent_per_session: bool,

    /// Bandwidth limitations
    pub bandwidth_config: Option<BandwidthLimitConfig>,

    /// Latency simulation
    pub latency_config: Option<LatencySimulationConfig>,

    /// Packet loss simulation
    pub packet_loss_config: Option<PacketLossConfig>,

    /// Client processing simulation
    pub processing_delay_config: Option<ProcessingDelayConfig>,
}
```

### 2.6 Transaction Profiler

```rust
/// Interface for transaction profilers
pub trait TransactionProfiler: MetricsProvider + Send + Sync {
    /// Initialize the profiler
    fn initialize(&mut self) -> Result<(), Box<dyn std::error::Error>>;

    /// Register a socket for profiling
    fn register_socket(&self, request_id: u64, socket_fd: i32) -> Result<(), Box<dyn std::error::Error>>;

    /// Record a runtime event
    fn record_runtime_event(&self, event: RuntimeEvent);

    /// Analyze a transaction, correlating kernel and runtime events
    fn analyze_transaction(&self, request_id: u64) -> TransactionAnalysis;

    /// Get client saturation metrics
    fn get_saturation_metrics(&self) -> SaturationMetrics;

    /// Clean up resources for completed requests
    fn cleanup_request(&self, request_id: u64);

    /// Shut down the profiler
    fn shutdown(&self);
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
    /// Current client resource utilization metrics
    pub resource_utilization: ResourceUtilization,
    /// Recommended actions based on saturation level
    pub recommendation: SaturationAction,
    /// Detailed analysis if available
    pub analysis: Option<String>,
}
```

### 2.7 Results Collector

```rust
/// Interface for collecting results
pub trait ResultsCollector: MetricsProvider + Send + Sync {
    /// Record a completed request
    fn record_request(&self, request: CompletedRequest);

    /// Get current metrics snapshot
    fn get_current_metrics(&self) -> RequestMetrics;

    /// Get final test results
    fn get_final_results(&self) -> TestResults;
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
    pub intended_latency_histogram: Arc<Histogram<u64>>,
    /// Histogram for actual-to-completion latencies
    pub actual_latency_histogram: Arc<Histogram<u64>>,
    /// Per-URL histograms
    pub url_histograms: Vec<Arc<Histogram<u64>>>,
    /// History of rate changes over time
    pub rate_history: Vec<(u64, u64)>,
    /// Profiling results if available
    pub profiling_results: Option<ProfilingResults>,
}
```

### 2.8 Metrics System

The metrics system provides comprehensive observability across all components while maintaining high performance.

#### 2.8.1 Metrics Provider Interface

```rust
/// Metrics provider interface for all components
pub trait MetricsProvider: Send + Sync {
    /// Get current metrics snapshot
    fn get_metrics(&self) -> ComponentMetrics;

    /// Component type identifier
    fn get_component_type(&self) -> &str;

    /// Component instance identifier
    fn get_component_id(&self) -> &str;
}

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
```

#### 2.8.2 Metrics Registry

```rust
/// Metrics registry for collecting metrics from all components
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
```

#### 2.8.3 Dual Interface for High-Throughput Components

For components that process a high volume of requests, a dual interface pattern is used:

```rust
/// Standard interface for a high-throughput component
pub trait HighThroughputComponent: Send + Sync {
    // High-performance methods without metrics overhead
}

/// Metrics-aware extension of a high-throughput component
pub trait MetricsAwareComponent: HighThroughputComponent + MetricsProvider {
    /// Register with metrics registry
    fn register_with_registry(&self, registry: Arc<dyn MetricsRegistry>) -> RegistrationHandle;
}
```

This pattern enables components to have a high-performance direct path for critical operations, with metrics collection as an optional extension.

#### 2.8.5 Request vs. Component Metrics

The metrics system distinguishes between two types of metrics:

1. **Request Metrics**: Aggregated statistics about request execution, captured in the `RequestMetrics` struct
   - Flows from `ResultsCollector` to `RateController`
   - Represents time-windowed performance data (latencies, error rates, throughput)
   - Used for dynamic rate control decisions

2. **Component Metrics**: State information about internal component behavior, captured in the `ComponentMetrics` struct
   - Collected from all components via the `MetricsProvider` interface
   - Represents component configuration, internal state, and behavior
   - Used for monitoring, debugging, and analysis

This dual approach allows both performance-critical metrics to flow directly between components and comprehensive observability of the entire system state.

### 2.9 Sample Recorder

```rust
/// Interface for recording sample data from the test
pub trait SampleRecorder: MetricsProvider + Send + Sync {
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

    /// Get current recording statistics
    fn get_recording_stats(&self) -> SampleRecordingStats;

    /// Set the output format for recorded samples
    fn set_output_format(&self, format: SampleOutputFormat);

    /// Get the configured output format
    fn get_output_format(&self) -> SampleOutputFormat;
}

/// Sample recording statistics
#[derive(Debug, Clone)]
pub struct SampleRecordingStats {
    /// Number of samples recorded
    pub sample_count: u64,
    /// Number of profiled samples recorded
    pub profiled_sample_count: u64,
    /// Number of rate change events recorded
    pub rate_change_count: u64,
    /// Number of saturation events recorded
    pub saturation_event_count: u64,
    /// Actual sampling rate (samples/total requests)
    pub effective_sampling_rate: f64,
    /// Size of recorded data in bytes
    pub data_size_bytes: u64,
}

/// Sample output format
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampleOutputFormat {
    /// JSON format (one record per line)
    JsonLines,
    /// CSV format
    Csv,
    /// Parquet format
    Parquet,
    /// Binary format (protocol buffers)
    ProtoBuf,
    /// Custom format
    Custom(String),
}
```

The Sample Recorder is responsible for capturing detailed data about selected requests for post-test analysis. It works in conjunction with the scheduler's unbiased Bernoulli sampling to ensure statistically valid data collection.

#### 2.9.1 Statistical Validity and Unbiased Sampling

The Sample Recorder maintains statistical validity through three key mechanisms:

1. **Pre-Execution Sampling Decision**: Sampling decisions are made by the scheduler at ticket creation time, before request execution, ensuring that performance does not bias sample selection
2. **Bernoulli Sampling**: Each request has an equal probability of being selected, independent of all other selections
3. **Preserved Sample Rate**: The effective sampling rate is monitored to ensure it matches the configured target rate

This approach ensures that the recorded samples represent the true distribution of all requests, regardless of their performance characteristics.

#### 2.9.2 Sample Data Structure

```rust
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
    /// Client session ID if available
    pub client_session_id: Option<String>,
    /// Request headers
    pub request_headers: Option<HashMap<String, String>>,
    /// Response headers
    pub response_headers: Option<HashMap<String, String>>,
    /// Response body (if configured to capture)
    pub response_body: Option<Vec<u8>>,
    /// Network condition applied (if any)
    pub network_condition: Option<NetworkConditionSummary>,
}

/// Network condition summary for samples
#[derive(Debug, Clone)]
pub struct NetworkConditionSummary {
    /// Whether this was a slow connection
    pub is_slow: bool,
    /// Applied upload bandwidth limit (bytes/sec)
    pub upload_limit_bps: Option<u64>,
    /// Applied download bandwidth limit (bytes/sec)
    pub download_limit_bps: Option<u64>,
    /// Applied request latency (ms)
    pub request_latency_ms: Option<u64>,
    /// Applied response latency (ms)
    pub response_latency_ms: Option<u64>,
}
```

For samples with profiling enabled, additional transaction analysis data is captured:

```rust
/// Sample with detailed transaction analysis
#[derive(Debug, Clone)]
pub struct ProfiledSample {
    /// Basic request sample
    pub sample: RequestSample,
    /// Detailed transaction analysis
    pub analysis: TransactionAnalysis,
}

/// Transaction timing phases for detailed analysis
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TransactionPhase {
    /// DNS resolution
    DnsResolution,
    /// TCP connection establishment
    TcpConnection,
    /// TLS handshake
    TlsHandshake,
    /// HTTP request writing
    RequestWrite,
    /// Waiting for first byte of response
    TimeToFirstByte,
    /// Reading response
    ResponseRead,
    /// Client-side processing
    ClientProcessing,
}
```

#### 2.9.3 Storage and Output Formats

Sample recorders support multiple output formats and storage mechanisms:

1. **Stream Processing**: Samples can be written to output streams as they are collected
2. **Batch Processing**: Samples can be accumulated in memory and written in batches
3. **File Output**: Results can be written to local or remote files
4. **Database Integration**: Samples can be sent to time-series or document databases

Multiple output formats are supported:

1. **JSON Lines**: One JSON object per line for easy parsing and streaming
2. **CSV**: Tabular format for spreadsheet analysis
3. **Parquet**: Columnar format for efficient querying
4. **Protocol Buffers**: Compact binary format for efficient storage

#### 2.9.4 Configuration

```rust
/// Sample recorder configuration
#[derive(Debug, Clone)]
pub struct SampleRecorderConfig {
    /// Output directory or URL
    pub output_path: String,
    /// Output format
    pub output_format: SampleOutputFormat,
    /// Maximum samples to keep in memory before writing
    pub batch_size: usize,
    /// Whether to capture request headers
    pub capture_request_headers: bool,
    /// Whether to capture response headers
    pub capture_response_headers: bool,
    /// Whether to capture response bodies
    pub capture_response_bodies: bool,
    /// Maximum size of response body to capture (bytes)
    pub max_response_body_size: usize,
    /// Specific headers to capture (empty = all)
    pub header_filter: Vec<String>,
}
```

#### 2.9.5 Memory-Efficient Implementation

For high-throughput testing, the Sample Recorder implements memory-efficient processing:

1. **Streaming Write**: Samples can be written to output as they arrive
2. **Batch Processing**: Multiple samples are accumulated and written in batches
3. **Compression**: Data can be compressed before storage
4. **Record Filtering**: Only selected fields may be captured to reduce storage requirements

This ensures that sample recording can occur even during high-throughput tests without affecting performance or consuming excessive memory.

#### 2.9.7 Memory-Efficient Implementation

For high-throughput testing, the Sample Recorder implements memory-efficient processing:

1. **Streaming Write**: Samples can be written to output as they arrive
2. **Batch Processing**: Multiple samples are accumulated and written in batches
3. **Compression**: Data can be compressed before storage
4. **Record Filtering**: Only selected fields may be captured to reduce storage requirements

This ensures that sample recording can occur even during high-throughput tests without affecting performance or consuming excessive memory.

### 3.1 Client Context

```rust
/// Context for request generation containing runtime information
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// Unique ID for this request
    pub request_id: u64,
    /// Scheduler ticket that triggered this request
    pub ticket: SchedulerTicket,
    /// Current timestamp
    pub timestamp: Instant,
    /// Whether detailed sampling is enabled for this request
    pub is_sampled: bool,
    /// Client session information
    pub session_info: Option<ClientSessionInfo>,
    /// Dynamic parameters that influence generation
    pub parameters: HashMap<String, String>,
}

/// Client session information
#[derive(Debug, Clone)]
pub struct ClientSessionInfo {
    /// Session ID
    pub session_id: String,
    /// Client type
    pub client_type: ClientType,
    /// Session start time
    pub start_time: Instant,
    /// Number of requests in this session so far
    pub request_count: u64,
    /// Client IP address
    pub client_ip: IpAddr,
    /// Client network condition
    pub network_condition: Option<NetworkCondition>,
}
```

### 3.2 Client Types

```rust
/// Client type
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClientType {
    /// Browser client
    Browser {
        /// Browser type
        browser_type: BrowserType,
        /// Whether HTTP/2 is enabled
        http2_enabled: bool,
        /// Connection limit
        connection_limit: u32,
    },
    /// Mobile client
    Mobile {
        /// Mobile platform
        platform: MobilePlatform,
        /// Network type
        network_type: NetworkType,
        /// Whether HTTP/2 is enabled
        http2_enabled: bool,
    },
    /// API client
    ApiClient {
        /// Client library
        client_library: ApiClientLibrary,
        /// Whether to use persistent connections
        persistent_connections: bool,
    },
}

/// Browser type
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BrowserType {
    Chrome,
    Firefox,
    Safari,
    Edge,
    Other(String),
}

/// Mobile platform
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MobilePlatform {
    iOS,
    Android,
    Other(String),
}

/// Network type
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NetworkType {
    WiFi,
    Mobile4G,
    Mobile5G,
    Mobile3G,
    Unknown,
}

/// API client library
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ApiClientLibrary {
    Curl,
    Python,
    Java,
    Node,
    CSharp,
    Other(String),
}
```

### 3.3 Network Condition

```rust
/// Network condition
#[derive(Debug, Clone)]
pub struct NetworkCondition {
    /// Whether this is a slow connection
    pub is_slow: bool,
    /// Bandwidth limit for upload (bytes/second)
    pub bandwidth_limit_upload: Option<u64>,
    /// Bandwidth limit for download (bytes/second)
    pub bandwidth_limit_download: Option<u64>,
    /// Latency to add to requests (milliseconds)
    pub request_latency_ms: Option<u64>,
    /// Latency to add to responses (milliseconds)
    pub response_latency_ms: Option<u64>,
    /// Packet loss percentage (0.0-1.0)
    pub packet_loss_percentage: Option<f64>,
    /// Client processing delay (milliseconds)
    pub processing_delay_ms: Option<u64>,
    /// Connection ID this condition applies to
    pub connection_id: String,
    /// Session ID this condition applies to (if any)
    pub session_id: Option<String>,
}
```

### 3.4 Request Sample

```rust
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
```

## 4. Component Type System

The architecture uses a type-based component system that enables static dispatch:

```rust
/// Component key for lookup
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ComponentKey {
    rate_controller: RateControllerType,
    client_session_manager: ClientSessionManagerType,
    scheduler: SchedulerType,
    generator: GeneratorType,
    transformer: TransformerType,
    executor: ExecutorType,
    profiler: ProfilerType,
    collector: CollectorType,
    network_simulator: NetworkSimulatorType,
}

/// Rate controller types
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RateControllerType {
    Static,
    Pid,
    StepFunction,
    SaturationAwarePid,
    // Other controller types...
}

/// Client session manager types
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClientSessionManagerType {
    Standard,
    BrowserBased,
    MobileFocused,
    ApiClient,
    // Other session manager types...
}
```

## 5. Builder Pattern

The framework provides a builder pattern for easy configuration:

```rust
/// Builder for load test configuration
pub struct LoadTestBuilder {
    config: LoadTestConfig,
}

impl LoadTestBuilder {
    pub fn new() -> Self {
        Self {
            config: LoadTestConfig::default(),
        }
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

    // Other builder methods...
}
```

## 6. Coordinated Omission Prevention

The framework prevents coordinated omission through several key mechanisms:

1. **Client Session Manager** generates tickets with intended timestamps
2. **Scheduler** dispatches tickets at those timestamps and records delays
3. **Metrics** track both scheduled and actual execution times

This separation of responsibilities ensures that load generation is independent from the ability to execute requests, preventing artificial reduction in load during system degradation.

## 7. Distributed Load Testing

The framework supports distributed load generation through a coordinator-worker model, enabling tests to scale beyond what a single machine can generate while maintaining accurate measurement.

### 7.1 Distributed Architecture

The distributed testing architecture consists of:

1. **Coordinator Node**:
   - Manages global test parameters
   - Distributes load across workers
   - Aggregates results from all nodes
   - Rebalances load based on worker saturation

2. **Worker Nodes**:
   - Execute the test with assigned parameters
   - Report load capacity and saturation metrics
   - Return detailed results to coordinator

### 7.2 Distributed Coordinator Interface

```rust
/// Interface for coordinating distributed load testing
pub trait DistributedCoordinator: MetricsProvider + Send + Sync {
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
#[derive(Debug, Clone)]
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

/// Node capabilities
#[derive(Debug, Clone)]
pub struct NodeCapabilities {
    /// CPU cores available
    pub cpu_cores: u32,
    /// Memory in MB
    pub memory_mb: u64,
    /// Network bandwidth in Mbps
    pub bandwidth_mbps: u64,
    /// Whether this node supports HTTP/2
    pub supports_http2: bool,
    /// Whether this node supports profiling
    pub supports_profiling: bool,
    /// Geographic region
    pub region: Option<String>,
}

/// Node assignment from coordinator
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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

### 7.3 Distributed Rate Controller Extension

The framework extends rate controllers to support distributed mode:

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

### 7.4 Load Distribution Algorithms

The system implements several load distribution algorithms:

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

### 7.5 Coordination Protocol

The distributed testing protocol follows these phases:

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

## 8. Example Usage

```rust
// Basic usage with PID controller and standard components
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_initial_rps(500)
    .with_duration(120)
    .with_pid_controller(100.0, 0.1, 0.01, 0.001)
    .with_client_session_manager(ClientSessionConfig {
        session_creation_rate: 10.0,  // 10 new sessions per second
        avg_session_duration_secs: 300.0,  // 5 minute average session
        distribution: {
            let mut dist = HashMap::new();
            dist.insert(
                ClientType::Browser {
                    browser_type: BrowserType::Chrome,
                    http2_enabled: true,
                    connection_limit: 6,
                },
                0.7,  // 70% Chrome browsers
            );
            dist.insert(
                ClientType::Mobile {
                    platform: MobilePlatform::iOS,
                    network_type: NetworkType::WiFi,
                    http2_enabled: true,
                },
                0.3,  // 30% iOS mobile clients
            );
            dist
        },
    })
    .with_standard_scheduler()
    .with_transaction_profiling(ProfilingMode::RuntimeOnly)
    .build()
    .expect("Failed to create load test");

// Run the test
let results = load_test.run().await.expect("Test failed");

// Distributed load testing example
let load_test = LoadTestBuilder::new()
    .with_urls(vec!["https://example.com/api".to_string()])
    .with_initial_rps(100)
    .with_duration(300)
    .with_pid_controller(100.0, 0.1, 0.01, 0.001)
    .with_distributed_mode(DistributedConfig {
        role: DistributedRole::Worker,
        coordinator_url: "https://coordinator.example.com".to_string(),
        node_id: "worker-1".to_string(),
        report_interval_ms: 1000,
    })
    .build()
    .expect("Failed to create load test");

// Run as part of distributed test
let results = load_test.run().await.expect("Test failed");
```
