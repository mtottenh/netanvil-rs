use std::time::{Duration, Instant};

/// Per-core metrics snapshot. Sent from worker to coordinator.
/// Contains a serialized HDR histogram and counters for a time window.
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    /// V2-serialized HDR histogram of latencies (nanoseconds)
    pub latency_histogram_bytes: Vec<u8>,
    /// Total requests completed in this window
    pub total_requests: u64,
    /// Total errors in this window
    pub total_errors: u64,
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Total bytes received
    pub bytes_received: u64,
    /// Window start time
    pub window_start: Instant,
    /// Window end time
    pub window_end: Instant,
}

/// Derived metrics summary for the RateController.
///
/// This type lives in netanvil-types so RateController doesn't depend on
/// hdrhistogram. The coordinator computes this from AggregateMetrics
/// (which lives in netanvil-metrics).
#[derive(Debug, Clone, Default)]
pub struct MetricsSummary {
    pub total_requests: u64,
    pub total_errors: u64,
    pub error_rate: f64,
    pub request_rate: f64,
    pub latency_p50_ns: u64,
    pub latency_p90_ns: u64,
    pub latency_p99_ns: u64,
    pub window_duration: Duration,
    /// External signals from the system under test.
    /// E.g. `[("load", 82.5)]` from a server-reported load metric.
    /// Injected by the coordinator from an external source (HTTP poll, push, etc.).
    pub external_signals: Vec<(String, f64)>,
}

/// Output of a RateController: new target rate and when to check again.
#[derive(Debug, Clone)]
pub struct RateDecision {
    pub target_rps: f64,
    pub next_update_interval: Duration,
}
