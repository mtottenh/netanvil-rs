use std::time::{Duration, Instant};

/// Context for a single request, created by the worker scheduling loop.
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// Unique request ID (partitioned by core: core_id * MAX + sequence)
    pub request_id: u64,
    /// When this request SHOULD have been sent (for coordinated omission tracking)
    pub intended_time: Instant,
    /// When it was actually dispatched
    pub actual_time: Instant,
    /// Which core is executing this request
    pub core_id: usize,
    /// Whether this request is selected for detailed sampling
    pub is_sampled: bool,
    /// Session ID, if session simulation is active
    pub session_id: Option<u64>,
}

/// What to send. Produced by RequestGenerator, modified by RequestTransformer.
#[derive(Debug, Clone)]
pub struct RequestSpec {
    pub method: http::Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// What happened. Produced by RequestExecutor, consumed by MetricsCollector.
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub request_id: u64,
    pub intended_time: Instant,
    pub actual_time: Instant,
    pub timing: TimingBreakdown,
    pub status: Option<u16>,
    pub response_size: u64,
    pub error: Option<ExecutionError>,
}

/// Latency breakdown for a single request.
#[derive(Debug, Clone, Default)]
pub struct TimingBreakdown {
    pub dns_lookup: Duration,
    pub tcp_connect: Duration,
    pub tls_handshake: Duration,
    pub time_to_first_byte: Duration,
    pub content_transfer: Duration,
    pub total: Duration,
}

/// Errors that can occur during request execution.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ExecutionError {
    #[error("connection failed: {0}")]
    Connect(String),

    #[error("request timed out")]
    Timeout,

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("{0}")]
    Other(String),
}
