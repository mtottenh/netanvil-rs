use std::time::Duration;

use serde::{Deserialize, Serialize};

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
    /// How often workers send metrics snapshots to the coordinator
    pub metrics_interval: Duration,
    /// How often the coordinator runs its control loop
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
        /// Request payload size for RR protocol mode.
        request_size: u16,
        /// Response payload size for RR/SOURCE/BIDIR modes.
        response_size: u32,
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
}

impl TestConfig {
    /// Get the initial RPS from the rate config.
    pub fn initial_rps(&self) -> f64 {
        match &self.rate {
            RateConfig::Static { rps } => *rps,
            RateConfig::Step { steps } => steps.first().map(|(_, rps)| *rps).unwrap_or(100.0),
            RateConfig::Pid { initial_rps, .. } => *initial_rps,
            RateConfig::CompositePid { initial_rps, .. } => *initial_rps,
            RateConfig::Ramp { warmup_rps, .. } => *warmup_rps,
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
            metrics_interval: Duration::from_millis(500),
            control_interval: Duration::from_millis(100),
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
            tls_client: None,
            target_bytes: None,
            tracked_response_headers: Vec::new(),
            md5_check_enabled: false,
            response_signal_headers: Vec::new(),
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
    /// PID controller targeting a single metric.
    Pid { initial_rps: f64, target: PidTarget },
    /// PID controller with multiple simultaneous constraints.
    /// Finds the maximum rate where ALL constraints are satisfied.
    CompositePid {
        initial_rps: f64,
        constraints: Vec<PidConstraint>,
        min_rps: f64,
        max_rps: f64,
    },
    /// Adaptive ramp: learn baseline latency during warmup, then use PID
    /// to find the maximum rate where latency stays within a multiplier
    /// of the baseline. Runs indefinitely if duration is 0.
    Ramp {
        /// RPS during warmup phase (default: 10)
        warmup_rps: f64,
        /// Duration of warmup phase to learn baseline latency
        warmup_duration: Duration,
        /// Acceptable latency multiplier over baseline.
        /// E.g., 3.0 means "p99 can be 3x the warmup baseline before backing off"
        latency_multiplier: f64,
        /// Maximum error rate (%) before forcing rate reduction (default: 5.0)
        max_error_rate: f64,
        /// Minimum RPS floor
        min_rps: f64,
        /// Maximum RPS ceiling
        max_rps: f64,
    },
}

/// PID controller target configuration (single-metric mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidTarget {
    pub metric: TargetMetric,
    /// Target value for the metric (e.g., 200ms for latency)
    pub value: f64,
    /// PID gains — auto-tuned by default, or manually specified.
    pub gains: PidGains,
    /// Min/max RPS bounds
    pub min_rps: f64,
    pub max_rps: f64,
}

/// How PID gains are determined.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PidGains {
    /// Automatically determine gains from system response (default).
    Auto {
        /// Maximum duration for the exploration phase. Default: 3s.
        autotune_duration: Duration,
        /// EMA smoothing factor for metric noise reduction (0.0-1.0). Default: 0.3.
        smoothing: f64,
    },
    /// User-specified fixed gains.
    Manual { kp: f64, ki: f64, kd: f64 },
}

impl Default for PidGains {
    fn default() -> Self {
        PidGains::Auto {
            autotune_duration: Duration::from_secs(3),
            smoothing: 0.3,
        }
    }
}

/// A single constraint in composite PID mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidConstraint {
    /// Which metric to constrain.
    pub metric: TargetMetric,
    /// Upper limit for this metric (e.g., 500.0 for "p99 < 500ms").
    pub limit: f64,
    /// Per-constraint gains. Defaults to auto-tuning.
    pub gains: PidGains,
}

/// Which metric the PID controller targets.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            max_connections_per_core: 100,
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            connection_policy: ConnectionPolicy::KeepAlive,
            dns_mode: None,
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

/// A distribution over positive integer counts.
///
/// Used to model realistic variation in connection lifetimes,
/// request counts per session, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CountDistribution {
    /// Exactly N every time.
    Fixed(u32),
    /// Uniform random between min and max (inclusive).
    Uniform { min: u32, max: u32 },
    /// Normal (Gaussian) distribution, clamped to >= 1.
    Normal { mean: f64, stddev: f64 },
}

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
