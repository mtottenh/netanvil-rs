use std::time::{Duration, Instant};

use hdrhistogram::Histogram;

/// Per-core metrics snapshot. Sent from worker to coordinator.
/// Contains an HDR histogram and counters for a time window.
///
/// `Histogram<u64>` is `Send`, so snapshots cross flume channels
/// without serialisation.  V2 encoding is only needed when histograms
/// must travel over the network (distributed coordinator).
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    /// HDR histogram of latencies (nanoseconds)
    pub latency_histogram: Histogram<u64>,
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
    /// Sum of scheduling delays (intended→actual gap) in nanoseconds.
    /// Used to compute mean delay for saturation detection.
    pub scheduling_delay_sum_ns: u64,
    /// Maximum scheduling delay in this window (nanoseconds).
    pub scheduling_delay_max_ns: u64,
    /// Number of requests with scheduling delay > 1ms.
    /// Indicates systematic client-side backlog.
    pub scheduling_delay_count_over_1ms: u64,
    /// Response header value distribution. Maps (header_name, header_value) -> count.
    /// Used for tracking response header distributions (e.g., X-Cache hit/miss).
    pub header_value_counts:
        std::collections::HashMap<String, std::collections::HashMap<String, u64>>,
    /// HDR histogram of response sizes (bytes).
    pub response_size_histogram: Histogram<u64>,
    /// Number of MD5 mismatches detected in this window.
    pub md5_mismatches: u64,
    /// Numeric signal values extracted from response headers this window.
    /// Maps signal_name → (sum, count, max, last) for aggregation.
    pub response_signals: std::collections::HashMap<String, ResponseSignalAccumulator>,
}

/// Accumulator for a single numeric signal extracted from response headers.
#[derive(Debug, Clone, Default)]
pub struct ResponseSignalAccumulator {
    pub sum: f64,
    pub count: u64,
    pub max: f64,
    pub last: f64,
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
    /// Total bytes sent during this window.
    pub bytes_sent: u64,
    /// Total bytes received during this window.
    pub bytes_received: u64,
    /// Send throughput in bytes per second.
    pub throughput_send_bps: f64,
    /// Receive throughput in bytes per second.
    pub throughput_recv_bps: f64,
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

/// Client-side saturation assessment.
///
/// Distinguishes between client bottleneck (can't generate fast enough),
/// server bottleneck (can't handle the load), or both.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SaturationInfo {
    /// Requests the timer thread dropped this window (fire channel full).
    pub backpressure_drops: u64,
    /// Fraction of total attempted dispatches that were dropped (0.0-1.0).
    pub backpressure_ratio: f64,
    /// Mean scheduling delay in milliseconds (intended_time → actual_time gap).
    pub scheduling_delay_mean_ms: f64,
    /// Max scheduling delay in milliseconds this window.
    pub scheduling_delay_max_ms: f64,
    /// Fraction of requests with scheduling delay > 1ms.
    pub delayed_request_ratio: f64,
    /// Ratio of achieved RPS to target RPS (1.0 = hitting target exactly).
    pub rate_achievement: f64,
    /// Overall assessment.
    pub assessment: SaturationAssessment,
}

/// Classification of where the bottleneck is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SaturationAssessment {
    /// All signals healthy — generating at target rate, no backpressure.
    #[default]
    Healthy,
    /// Client can't generate fast enough — add more cores or nodes.
    ClientSaturated,
    /// Server can't handle the load — latency/errors rising.
    ServerSaturated,
    /// Both client and server are struggling.
    BothSaturated,
}
