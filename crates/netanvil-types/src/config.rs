use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::distribution::{CountDistribution, ValueDistribution};

fn default_true() -> bool {
    true
}

/// Top-level test configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    /// Target URL(s)
    pub targets: Vec<String>,
    /// HTTP method
    pub method: String,
    /// Test duration
    pub duration: Duration,
    /// Rate configuration
    pub rate: RateConfig,
    /// Scheduler type
    pub scheduler: SchedulerConfig,
    /// Headers to add to every request
    pub headers: Vec<(String, String)>,
    /// Number of worker cores (0 = auto-detect)
    pub num_cores: usize,
    /// Connection settings
    pub connections: ConnectionConfig,
    /// How often the coordinator runs its control loop.
    /// Minimum 1 second. Default 3 seconds.
    /// `metrics_interval` is derived internally as `control_interval / 2`.
    pub control_interval: Duration,
    /// HTTP status codes >= this threshold count as errors in metrics.
    /// Set to 0 to disable HTTP error classification (only transport errors count).
    /// Default: 400 (all 4xx and 5xx are errors).
    pub error_status_threshold: u16,
    /// Optional URL to poll for external metrics (JSON object).
    /// Used with PID `TargetMetric::External` to control rate based on
    /// server-reported metrics (e.g. proxy load, queue depth).
    pub external_metrics_url: Option<String>,
    /// JSON field name to extract from the external metrics response.
    /// E.g. "load" extracts the numeric value from `{"load": 82.5}`.
    pub external_metrics_field: Option<String>,
    /// Optional bandwidth limit in bits per second for modem speed simulation.
    /// When set, the HTTP executor throttles TCP reads to this rate and tunes
    /// per-socket receive buffers to create real TCP-level backpressure.
    /// Example values: 56_000 (56kbps modem), 1_000_000 (1 Mbps), 10_000_000 (10 Mbps).
    #[serde(default)]
    pub bandwidth_limit_bps: Option<u64>,
    /// Warmup duration excluded from metrics. Workers run normally during
    /// warmup (connections establish, caches warm) but the coordinator
    /// discards all metrics until the warmup period ends.
    #[serde(default)]
    pub warmup_duration: Option<Duration>,
    /// Stop after this many total requests (across all cores).
    #[serde(default)]
    pub max_requests: Option<u64>,
    /// Stop if cumulative error count exceeds this threshold.
    #[serde(default)]
    pub autostop_threshold: Option<u64>,
    /// Stop if cumulative refused/failed connection count exceeds this threshold.
    #[serde(default)]
    pub refusestop_threshold: Option<u64>,
    /// Whether to wait for in-flight requests on shutdown. Default: true.
    #[serde(default = "default_true")]
    pub graceful_shutdown: bool,
    /// Optional plugin configuration for custom request generation.
    /// When set, the test uses the plugin's generator instead of the default
    /// `SimpleGenerator`. In distributed mode, the leader embeds the plugin
    /// source (script text or WASM bytes) so agents can instantiate generators.
    #[serde(default)]
    pub plugin: Option<PluginConfig>,
    /// Protocol-specific configuration for TCP/UDP tests.
    /// When set, the agent uses this to construct non-HTTP executors.
    /// Serializable for transmission from leader to agent nodes.
    #[serde(default)]
    pub protocol: Option<ProtocolConfig>,
    /// HTTP version selection for load traffic.
    /// Controls ALPN negotiation and hyper client HTTP/2 settings.
    #[serde(default)]
    pub http_version: HttpVersion,
    /// TLS configuration for load traffic connections to the target.
    /// When set, all HTTPS connections use the specified client certs,
    /// verification settings, SNI, and cipher preferences.
    #[serde(default)]
    pub tls_client: Option<TlsClientConfig>,
    /// Stop when cumulative bytes received reaches this threshold.
    #[serde(default)]
    pub target_bytes: Option<u64>,
    /// Response headers to track value distributions for.
    /// Each header name listed here will have its response values counted
    /// per-core and aggregated (e.g., `["X-Cache"]` for cache-hit tracking).
    #[serde(default)]
    pub tracked_response_headers: Vec<String>,
    /// Enable MD5 body verification. When true, the metrics collector
    /// computes MD5 of response bodies and compares with the Content-MD5
    /// header. Mismatches are counted in MetricsSnapshot::md5_mismatches.
    #[serde(default)]
    pub md5_check_enabled: bool,
    /// Response headers to extract as numeric PID signals.
    /// Each configured header is parsed as f64 per-request, aggregated
    /// per metrics window, and injected into `MetricsSummary::external_signals`
    /// for use with `TargetMetric::External { name }`.
    #[serde(default)]
    pub response_signal_headers: Vec<ResponseSignalConfig>,
    /// Per-request event log configuration.
    /// When set, each I/O worker core writes an Arrow IPC file with one row
    /// per completed request (timing, status, bytes, errors).
    #[serde(default)]
    pub event_log: Option<EventLogOutput>,
    /// Fraction of TCP reads to sample for CPU affinity observation (0.0-1.0).
    /// When > 0.0, uses io_uring linked SQEs to atomically read SO_INCOMING_CPU.
    /// Default: 0.0 (disabled).
    #[serde(default)]
    pub health_sample_rate: f64,
    /// Path for control-plane trace recording. When set, records per-tick
    /// controller decisions to a structured file for post-test analysis.
    /// Extension determines format: `.jsonl` (default), `.arrow` (future).
    #[serde(default)]
    pub control_trace: Option<String>,
}

/// Per-request event log output configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLogOutput {
    /// Directory path for per-core Arrow IPC files.
    /// Created automatically if it doesn't exist.
    pub output_dir: String,
    /// Fraction of requests to log (0.0-1.0). Default: 1.0 (log all).
    #[serde(default = "default_sample_rate")]
    pub sample_rate: f64,
}

fn default_sample_rate() -> f64 {
    1.0
}

/// Protocol-specific configuration for non-HTTP tests.
///
/// Serializable for transmission from leader to agent nodes.
/// Uses hex-encoded payloads for clean JSON representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProtocolConfig {
    /// TCP test configuration.
    Tcp {
        /// Test mode: "echo", "rr", "sink", "source", "bidir"
        mode: String,
        /// Hex-encoded payload (e.g. "48454c4c4f" for "HELLO")
        payload_hex: String,
        /// Framing: "raw", "length-prefix:N", "delimiter:XX", "fixed:N"
        framing: String,
        /// Request payload size distribution for RR protocol mode.
        request_size: ValueDistribution<u16>,
        /// Response payload size distribution for RR/SOURCE/BIDIR modes.
        response_size: ValueDistribution<u32>,
    },
    /// UDP test configuration.
    Udp {
        /// Hex-encoded payload.
        payload_hex: String,
        /// Whether to wait for a response datagram.
        expect_response: bool,
    },
    /// DNS test configuration.
    Dns {
        /// Comma-separated domain names to query.
        domains: String,
        /// Query type: "A", "AAAA", "MX", etc.
        query_type: String,
        /// Whether to set the RD (recursion desired) flag.
        recursion: bool,
    },
    /// Redis test configuration.
    Redis {
        /// Optional AUTH password.
        password: Option<String>,
        /// Database number (SELECT).
        db: Option<u16>,
        /// Default command if no plugin specified.
        command: String,
        /// Default args if no plugin specified.
        args: Vec<String>,
    },
}

/// Plugin configuration embedded in TestConfig.
///
/// Carries the plugin type and source code/binary so it can be serialized
/// over the wire from leader to agent nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    /// Which plugin runtime to use.
    pub plugin_type: PluginType,
    /// Plugin source: script text (for Lua/hybrid) or WASM binary bytes.
    pub source: Vec<u8>,
}

/// Available plugin runtime types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginType {
    /// Lua config script → native Rust hot path. Zero per-request overhead.
    Hybrid,
    /// LuaJIT per-request generator. ~2μs per call.
    Lua,
    /// WASM module per-request generator. ~2.7μs per call.
    Wasm,
    /// V8 JavaScript per-request generator. Requires the `v8` feature flag.
    Js,
}

/// HTTP version to use for load traffic connections.
///
/// Controls ALPN negotiation and hyper client configuration. This is a
/// transport-level setting — plugins produce version-agnostic `HttpRequestSpec`
/// and the executor handles version negotiation transparently.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HttpVersion {
    /// HTTP/1.1 only. ALPN: ["http/1.1"]. Default behavior.
    #[default]
    Http1,
    /// HTTP/2 only (TLS). ALPN: ["h2"]. Fails if server doesn't support h2.
    Http2,
    /// HTTP/2 cleartext (h2c). No TLS. Uses HTTP/2 prior knowledge.
    Http2c,
    /// Let ALPN negotiate. ALPN: ["h2", "http/1.1"].
    Auto,
}

impl TestConfig {
    /// Get the initial RPS from the rate config.
    pub fn initial_rps(&self) -> f64 {
        match &self.rate {
            RateConfig::Static { rps } => *rps,
            RateConfig::Step { steps } => steps.first().map(|(_, rps)| *rps).unwrap_or(100.0),
            RateConfig::Adaptive {
                warmup,
                initial_rps,
                bounds,
                ..
            } => initial_rps
                .unwrap_or_else(|| warmup.as_ref().map(|w| w.rps).unwrap_or(bounds.min_rps)),
        }
    }
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            targets: vec!["http://localhost:8080".to_string()],
            method: "GET".to_string(),
            duration: Duration::from_secs(10),
            rate: RateConfig::Static { rps: 100.0 },
            scheduler: SchedulerConfig::default(),
            headers: Vec::new(),
            num_cores: 0,
            connections: ConnectionConfig::default(),
            control_interval: Duration::from_secs(3),
            error_status_threshold: 400,
            external_metrics_url: None,
            external_metrics_field: None,
            bandwidth_limit_bps: None,
            warmup_duration: None,
            max_requests: None,
            autostop_threshold: None,
            refusestop_threshold: None,
            graceful_shutdown: true,
            plugin: None,
            protocol: None,
            http_version: HttpVersion::default(),
            tls_client: None,
            target_bytes: None,
            tracked_response_headers: Vec::new(),
            md5_check_enabled: false,
            response_signal_headers: Vec::new(),
            event_log: None,
            health_sample_rate: 0.0,
            control_trace: None,
        }
    }
}

/// Which scheduling discipline to use for request timing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum SchedulerConfig {
    /// Fixed intervals: exactly 1/rate between each request.
    #[default]
    ConstantRate,
    /// Poisson process: exponentially distributed inter-arrival times.
    /// Models realistic independent user arrivals.
    Poisson {
        /// Optional seed for deterministic replay.
        seed: Option<u64>,
    },
}

/// How request rate is controlled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RateConfig {
    /// Constant request rate.
    Static { rps: f64 },
    /// Rate changes at specified time offsets. Vec of (offset_from_start, rps).
    Step { steps: Vec<(Duration, f64)> },
    /// Unified adaptive controller: feedback-based rate control with
    /// composable constraints (threshold and/or PID setpoint tracking).
    Adaptive {
        bounds: BoundsConfig,
        #[serde(default)]
        warmup: Option<WarmupConfig>,
        /// Starting rate when no warmup is configured. Defaults to bounds.min_rps.
        #[serde(default)]
        initial_rps: Option<f64>,
        constraints: Vec<ConstraintConfig>,
        /// How rate increases when no constraints object.
        #[serde(default)]
        increase: Option<IncreasePolicyConfig>,
        /// Post-backoff increase suppression.
        #[serde(default)]
        cooldown: Option<CooldownPolicyConfig>,
        /// Known-good rate floor (prevents deep drops on transient spikes).
        #[serde(default)]
        floor: Option<FloorPolicyConfig>,
        /// Per-tick rate-of-change limits (asymmetric: increases bounded
        /// more tightly than decreases).
        #[serde(default)]
        rate_change_limits: Option<RateChangeLimitsConfig>,
    },
}

/// Which metric the PID controller targets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TargetMetric {
    LatencyP50,
    LatencyP90,
    LatencyP99,
    ErrorRate,
    /// Send throughput in Mbps. PID increases rate when below target.
    ThroughputSend,
    /// Receive throughput in Mbps. PID increases rate when below target.
    ThroughputRecv,
    /// Server-reported metric, identified by name.
    /// The coordinator polls an external endpoint and injects the value
    /// into MetricsSummary::external_signals.
    External {
        name: String,
    },
}

/// Connection pool and timeout settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    /// Maximum concurrent connections per core
    pub max_connections_per_core: usize,
    /// TCP connect timeout
    pub connect_timeout: Duration,
    /// Per-request timeout
    pub request_timeout: Duration,
    /// Connection lifecycle policy
    pub connection_policy: ConnectionPolicy,
    /// DNS resolution preference. When set, the connector resolves hostnames
    /// manually and filters by address family before connecting.
    #[serde(default)]
    pub dns_mode: Option<DnsMode>,
    /// Maximum concurrent in-flight requests per I/O core. When at capacity,
    /// new fire events are dropped (not queued), creating visible backpressure.
    /// 0 = auto-compute from max_rps, cores, and latency budget.
    #[serde(default)]
    pub max_in_flight_per_core: usize,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            max_connections_per_core: 100,
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            connection_policy: ConnectionPolicy::KeepAlive,
            dns_mode: None,
            max_in_flight_per_core: 0, // auto
        }
    }
}

/// DNS resolution preference for target hostname lookup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DnsMode {
    /// IPv4 only (A records).
    V4,
    /// IPv6 only (AAAA records).
    V6,
    /// Prefer IPv4, fall back to IPv6.
    PreferV4,
    /// Prefer IPv6, fall back to IPv4.
    PreferV6,
}

/// How connections to the target are managed.
///
/// Controls connection lifecycle via the `Connection: close` HTTP header.
/// The actual TCP connections are managed by the HTTP client (cyper); this
/// policy influences client behavior by requesting connection closure on
/// selected requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConnectionPolicy {
    /// Always reuse connections via HTTP keep-alive.
    /// This is the default and matches standard HTTP/1.1 behavior.
    KeepAlive,

    /// Close the connection after every request.
    /// Forces a new TCP connection (+ TLS handshake) for each request.
    /// Useful for testing connection establishment rate.
    AlwaysNew,

    /// Open a new connection for every request but don't close old ones.
    /// Old connections are intentionally leaked (fd stays open) so the
    /// server sees idle persistent connections accumulating.
    /// Does NOT send `Connection: close` — connections appear keep-alive.
    /// Useful for stress-testing server idle connection handling.
    /// Users need high ulimits (each request opens a new fd).
    NoReuse,

    /// Mixed behavior simulating realistic client populations.
    /// A configurable fraction of requests reuse connections; the
    /// remainder force new connections.
    Mixed {
        /// Fraction of requests that reuse existing connections (0.0-1.0).
        /// The remainder send `Connection: close`.
        persistent_ratio: f64,
        /// Connection lifetime in number of requests. When set, each
        /// simulated connection closes after this many requests and a
        /// new one is opened. The count is sampled from the distribution
        /// each time a connection is "opened".
        /// None = unlimited (close only based on persistent_ratio).
        connection_lifetime: Option<CountDistribution>,
    },
}

// `CountDistribution` (= `ValueDistribution<u32>`) is defined in
// `crate::distribution` and re-exported from `crate::lib`.

/// TLS configuration for HTTP executor connections to the target.
///
/// Controls client certificates, server verification, SNI, and cipher
/// selection for load traffic connections. Separate from [`TlsConfig`]
/// (which is for leader↔agent mTLS).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsClientConfig {
    /// Path to client certificate PEM file (leaf + optional chain).
    pub client_cert: Option<String>,
    /// Path to client private key PEM file (PKCS#1 or PKCS#8).
    pub client_key: Option<String>,
    /// Path to CA certificate PEM file for server verification.
    /// When set with `verify_server: true`, only this CA is trusted.
    pub ca_cert: Option<String>,
    /// Whether to verify the server's certificate. Default: true.
    #[serde(default = "default_true")]
    pub verify_server: bool,
    /// Override the TLS SNI server name (independent of Host header).
    pub sni_override: Option<String>,
    /// OpenSSL-style cipher string. Filters rustls cipher suites to
    /// only those matching. Only modern AEAD suites are available;
    /// legacy ciphers (RC4, DES, CBC) will produce an error.
    pub cipher_list: Option<String>,
    /// Disable TLS session resumption. When true, every connection
    /// performs a full handshake (equivalent to legacy `-ctxreset 0`).
    #[serde(default)]
    pub disable_session_resumption: bool,
}

/// Configuration for extracting a numeric signal from response headers.
///
/// The value is parsed as f64 per-request and aggregated per metrics window.
/// The aggregated value is injected into `MetricsSummary::external_signals`
/// and can be targeted by `TargetMetric::External { name }` for PID control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseSignalConfig {
    /// Response header name to extract (case-insensitive match).
    pub header: String,
    /// Signal name as it appears in `MetricsSummary::external_signals`.
    /// Defaults to the header name if not set.
    #[serde(default)]
    pub signal_name: Option<String>,
    /// How to aggregate per-request values across a metrics window.
    #[serde(default)]
    pub aggregation: SignalAggregation,
}

impl ResponseSignalConfig {
    /// The signal name used in MetricsSummary::external_signals.
    pub fn signal_name(&self) -> &str {
        self.signal_name.as_deref().unwrap_or(&self.header)
    }
}

/// How to aggregate per-request numeric values across a metrics window.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum SignalAggregation {
    /// Arithmetic mean of all values in the window.
    #[default]
    Mean,
    /// Maximum value in the window.
    Max,
    /// Most recent value seen in the window.
    Last,
}

// ---------------------------------------------------------------------------
// External signal constraints (for ramp controller)
// ---------------------------------------------------------------------------

/// Configuration for an external signal constraint in the ramp controller.
///
/// Maps a named signal from `MetricsSummary::external_signals` to a
/// backoff constraint. The ramp controller backs off when the signal
/// exceeds (or falls below, depending on `direction`) the threshold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalConstraintConfig {
    /// Signal name matching a key in `MetricsSummary::external_signals`.
    pub signal_name: String,
    /// Threshold at which severity = 1.0 (violation begins).
    pub threshold: f64,
    /// Whether higher or lower signal values indicate worse conditions.
    #[serde(default)]
    pub direction: SignalDirection,
    /// What to do when the signal is absent or stale.
    #[serde(default)]
    pub on_missing: MissingSignalBehavior,
    /// Number of ticks without a signal update before it's considered stale.
    #[serde(default = "default_stale_ticks")]
    pub stale_after_ticks: u32,
    /// Consecutive ticks of violation required before triggering backoff.
    /// Higher values filter noise at the cost of slower response.
    #[serde(default = "default_constraint_persistence")]
    pub persistence: u32,
}

fn default_stale_ticks() -> u32 {
    3
}

fn default_constraint_persistence() -> u32 {
    1
}

/// Whether higher or lower signal values indicate worse conditions.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum SignalDirection {
    /// Higher values are worse (e.g., drop_rate, cpu_pct, queue_depth).
    /// Severity = value / threshold.
    #[default]
    HigherIsWorse,
    /// Lower values are worse (e.g., free_fds, available_memory).
    /// Severity = threshold / value.
    LowerIsWorse,
}

/// Behavior when an expected signal is not present in `MetricsSummary`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum MissingSignalBehavior {
    /// Act as if the constraint doesn't exist (no restriction).
    #[default]
    Ignore,
    /// Suppress rate increases but don't back off.
    Hold,
    /// Treat as worst-case violation (hard backoff).
    Backoff,
}

// ---------------------------------------------------------------------------
// Adaptive controller config types (Step 1 of API rationalization)
// ---------------------------------------------------------------------------

fn default_persistence() -> u32 {
    1
}

fn default_tracking_gain() -> f64 {
    0.5
}

fn default_autotune_duration() -> Duration {
    Duration::from_secs(3)
}

fn default_smoothing() -> f64 {
    0.3
}

/// Reference to a metric source — either a built-in internal metric or an
/// external signal endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetricRef {
    Internal(InternalMetric),
    External(ExternalMetricRef),
}

/// Built-in metrics available from the metrics collector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InternalMetric {
    LatencyP50,
    LatencyP90,
    LatencyP99,
    ErrorRate,
    ThroughputSend,
    ThroughputRecv,
    TimeoutFraction,
    InFlightDropFraction,
}

/// Reference to an external metric signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalMetricRef {
    pub name: String,
    #[serde(default)]
    pub direction: SignalDirection,
    #[serde(default = "default_stale_ticks")]
    pub stale_after_ticks: u32,
    #[serde(default)]
    pub on_missing: MissingSignalBehavior,
}

/// A single constraint in the Adaptive rate controller.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConstraintConfig {
    Threshold(ThresholdConstraintConfig),
    Setpoint(SetpointConstraintConfig),
}

/// Configuration for a threshold-based (AIMD) constraint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdConstraintConfig {
    pub id: String,
    pub metric: MetricRef,
    #[serde(default)]
    pub smoother: Option<SmootherConfig>,
    /// Override the default constraint class. Defaults: TimeoutFraction and
    /// InFlightDropFraction -> Catastrophic; everything else -> OperatingPoint.
    /// Most users should leave this unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_override: Option<ConstraintClassConfig>,
    #[serde(flatten)]
    pub threshold_source: ThresholdSource,
    #[serde(default = "default_persistence")]
    pub persistence: u32,
    #[serde(default)]
    pub self_caused_cap: Option<f64>,
    #[serde(default)]
    pub backoff: Option<BackoffConfig>,
}

/// How the threshold value is determined.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ThresholdSource {
    Absolute {
        threshold: f64,
    },
    FromBaseline {
        threshold_from_baseline: BaselineMultiplier,
    },
}

/// Multiplier applied to the warmup baseline to derive a threshold.
///
/// The effective baseline used is `max(observed_baseline, baseline_floor_ms)`,
/// which prevents sub-millisecond services from getting thresholds too tight
/// to absorb normal OS scheduling jitter (timer ticks, softirq deferral, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineMultiplier {
    pub multiplier: f64,
    /// Floor applied to the observed baseline before multiplying.
    /// Default: 4.0ms — on a 100Hz tick kernel the p99 jitter envelope
    /// reaches 4-5ms due to ksoftirqd scheduling deferral (up to one
    /// 10ms tick) and softirq budget exhaustion. A 4ms floor with a
    /// typical 1.2x multiplier gives a 4.8ms threshold, just above the
    /// observed jitter ceiling while still detecting genuine saturation.
    #[serde(default = "default_baseline_floor_ms")]
    pub baseline_floor_ms: f64,
}

fn default_baseline_floor_ms() -> f64 {
    4.0
}

/// Configuration for a setpoint-tracking (PID) constraint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetpointConstraintConfig {
    pub id: String,
    pub metric: MetricRef,
    #[serde(default)]
    pub smoother: Option<SmootherConfig>,
    pub target: f64,
    #[serde(default)]
    pub gains: GainsConfig,
    #[serde(default = "default_tracking_gain")]
    pub tracking_gain: f64,
}

/// How PID gains are determined for a setpoint constraint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GainsConfig {
    Auto {
        #[serde(default = "default_autotune_duration")]
        autotune_duration: Duration,
        #[serde(default = "default_smoothing")]
        smoothing: f64,
    },
    Manual {
        kp: f64,
        ki: f64,
        kd: f64,
    },
}

impl Default for GainsConfig {
    fn default() -> Self {
        GainsConfig::Auto {
            autotune_duration: default_autotune_duration(),
            smoothing: default_smoothing(),
        }
    }
}

/// Rate bounds for the Adaptive controller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundsConfig {
    pub min_rps: f64,
    pub max_rps: f64,
}

/// Warmup phase configuration for the Adaptive controller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmupConfig {
    pub rps: f64,
    pub duration: Duration,
}

/// Signal smoother configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SmootherConfig {
    None,
    Ema { alpha: f64 },
    Median { size: usize },
}

/// Backoff factor overrides for threshold constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackoffConfig {
    #[serde(default = "default_gentle")]
    pub gentle: f64,
    #[serde(default = "default_moderate")]
    pub moderate: f64,
    #[serde(default = "default_hard")]
    pub hard: f64,
}

fn default_gentle() -> f64 {
    0.90
}
fn default_moderate() -> f64 {
    0.75
}
fn default_hard() -> f64 {
    0.50
}

/// Constraint class override for threshold constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConstraintClassConfig {
    OperatingPoint,
    Catastrophic,
}

// ---------------------------------------------------------------------------
// Controller-level policy configs (optional fields on Adaptive)
// ---------------------------------------------------------------------------

/// Congestion avoidance settings for the increase policy.
///
/// Near the last known failure rate, the controller switches from
/// multiplicative increase to additive increase (like TCP's congestion
/// avoidance phase). These parameters control that transition.
///
/// Omit or set to `null` in JSON to disable congestion avoidance entirely
/// (the controller will always use multiplicative increase).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CongestionAvoidanceConfig {
    /// Switch from multiplicative to additive increase when `current_rate`
    /// exceeds this fraction of the last failure rate. Default: 0.85.
    #[serde(default = "default_ca_trigger_threshold")]
    pub trigger_threshold: f64,
    /// Additive increase = `failure_rate × this`. Default: 0.01.
    #[serde(default = "default_ca_additive_fraction")]
    pub additive_fraction: f64,
    /// EMA smoothing on the failure-rate estimate. Default: 0.3.
    #[serde(default = "default_ca_failure_rate_alpha")]
    pub failure_rate_alpha: f64,
}

fn default_ca_trigger_threshold() -> f64 {
    0.85
}
fn default_ca_additive_fraction() -> f64 {
    0.01
}
fn default_ca_failure_rate_alpha() -> f64 {
    0.3
}

/// How rate increases when no constraints object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IncreasePolicyConfig {
    /// Rate grows by `factor` per clean tick (e.g., 1.10 = +10%/tick).
    /// Optionally, near a known failure rate, switches to additive increase
    /// when congestion avoidance is configured.
    Multiplicative {
        #[serde(default = "default_increase_factor")]
        factor: f64,
        /// Congestion avoidance settings. Omit or set to `null` to disable
        /// (always use multiplicative increase).
        #[serde(default)]
        congestion_avoidance: Option<CongestionAvoidanceConfig>,
    },
    /// Fixed increment per clean tick.
    Additive { increment: f64 },
}

impl Default for IncreasePolicyConfig {
    fn default() -> Self {
        Self::Multiplicative {
            factor: default_increase_factor(),
            congestion_avoidance: None,
        }
    }
}

fn default_increase_factor() -> f64 {
    1.10
}

/// Post-backoff increase suppression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CooldownPolicyConfig {
    /// Minimum cooldown ticks after a backoff.
    #[serde(default = "default_cooldown_min_ticks")]
    pub min_ticks: u32,
    /// Cooldown duration = recovery_estimate × this multiplier.
    #[serde(default = "default_cooldown_multiplier")]
    pub recovery_multiplier: f64,
}

impl Default for CooldownPolicyConfig {
    fn default() -> Self {
        Self {
            min_ticks: default_cooldown_min_ticks(),
            recovery_multiplier: default_cooldown_multiplier(),
        }
    }
}

fn default_cooldown_min_ticks() -> u32 {
    2
}
fn default_cooldown_multiplier() -> f64 {
    1.5
}

/// Known-good rate floor. Prevents deep rate drops on transient spikes.
/// Set to `None` (via `"floor": null`) to disable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloorPolicyConfig {
    /// Floor = best_rate_in_window × fraction (e.g., 0.80 = 80%).
    #[serde(default = "default_floor_fraction")]
    pub fraction: f64,
    /// Window for geometric decay of the known-good rate.
    #[serde(default = "default_floor_window")]
    pub window: Duration,
}

impl Default for FloorPolicyConfig {
    fn default() -> Self {
        Self {
            fraction: default_floor_fraction(),
            window: default_floor_window(),
        }
    }
}

fn default_floor_fraction() -> f64 {
    0.80
}
fn default_floor_window() -> Duration {
    Duration::from_secs(60)
}

/// Per-tick rate-of-change limits. Asymmetric: increases are bounded
/// more tightly than decreases to prevent overshoot while allowing
/// aggressive backoff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateChangeLimitsConfig {
    /// Maximum increase per tick as a fraction (e.g., 0.20 = +20%).
    #[serde(default = "default_max_increase")]
    pub max_increase_pct: f64,
    /// Maximum decrease per tick as a fraction (e.g., 0.50 = -50%).
    #[serde(default = "default_max_decrease")]
    pub max_decrease_pct: f64,
}

impl Default for RateChangeLimitsConfig {
    fn default() -> Self {
        Self {
            max_increase_pct: default_max_increase(),
            max_decrease_pct: default_max_decrease(),
        }
    }
}

fn default_max_increase() -> f64 {
    0.20
}
fn default_max_decrease() -> f64 {
    0.50
}

/// TLS configuration for mTLS between leader and agent nodes.
///
/// When provided, all leader↔agent communication uses mTLS:
/// - The server presents `cert`/`key` and verifies the client's cert against `ca_cert`
/// - The client presents `cert`/`key` and verifies the server's cert against `ca_cert`
///
/// All paths are to PEM-encoded files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// Path to the CA certificate PEM file (used to verify the peer).
    pub ca_cert: String,
    /// Path to this node's certificate PEM file.
    pub cert: String,
    /// Path to this node's private key PEM file.
    pub key: String,
}
