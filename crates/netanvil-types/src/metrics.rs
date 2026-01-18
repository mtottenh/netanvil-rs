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
    /// CPU affinity hits: sampled reads where RX CPU matches worker CPU.
    pub cpu_affinity_hits: u64,
    /// CPU affinity misses: sampled reads where RX CPU differs from worker CPU.
    pub cpu_affinity_misses: u64,
    /// CPU affinity unknown: sampled reads where SO_INCOMING_CPU was unavailable.
    pub cpu_affinity_unknown: u64,
    /// TCP RTT sum in microseconds (for mean calculation).
    pub tcp_rtt_sum_us: u64,
    /// Number of TCP RTT observations.
    pub tcp_rtt_count: u64,
    /// Maximum TCP RTT observed in microseconds.
    pub tcp_rtt_max_us: u64,
    /// Total TCP retransmits observed (summed across samples).
    pub tcp_retransmits: u64,
    /// Total TCP lost segments observed.
    pub tcp_lost: u64,
    /// Protocol-level messages sent this window (datagrams, transactions, queries).
    /// Distinct from `total_requests` which counts completed request/response cycles.
    pub packets_sent: u64,
    /// Protocol-level responses received this window.
    pub packets_received: u64,
    /// Protocol-level messages declared lost (sent but no response observed
    /// within the tracking window).
    pub packets_lost: u64,
    /// Requests that completed due to timeout. Tracked separately from
    /// total_errors because timeout "latency" should not be in the histogram.
    pub timeout_count: u64,
    /// Requests declined by the per-core in-flight limit this window.
    pub in_flight_drops: u64,
    /// Current in-flight request count at snapshot time.
    pub in_flight_count: u64,
    /// Per-core in-flight capacity (0 = unlimited).
    pub in_flight_capacity: u64,
}

/// Protocol-level packet counter deltas for a metrics window.
///
/// Returned by [`PacketCounterSource::take_packet_deltas()`] for protocols
/// that track packet-level statistics (UDP loss, TCP retransmits, etc.).
#[derive(Debug, Clone, Copy, Default)]
pub struct PacketCounterDeltas {
    pub sent: u64,
    pub received: u64,
    pub lost: u64,
}

/// Accumulator for a single numeric signal extracted from response headers.
#[derive(Debug, Clone, Default)]
pub struct ResponseSignalAccumulator {
    pub sum: f64,
    pub count: u64,
    pub max: f64,
    pub last: f64,
}

/// Shared CPU affinity observation counters.
///
/// Produced by connection health sampling (`ObservedTcpStream` writes),
/// consumed by the metrics collector (`snapshot()` reads and resets).
/// Both live on the same core — `Cell` is sufficient.
pub struct CpuAffinityCounters {
    pub hits: std::cell::Cell<u64>,
    pub misses: std::cell::Cell<u64>,
    pub unknown: std::cell::Cell<u64>,
}

impl CpuAffinityCounters {
    pub fn new() -> Self {
        Self {
            hits: std::cell::Cell::new(0),
            misses: std::cell::Cell::new(0),
            unknown: std::cell::Cell::new(0),
        }
    }

    /// Record a single observation.
    #[inline]
    pub fn record(&self, incoming_cpu: i32, worker_cpu: usize) {
        if incoming_cpu < 0 {
            self.unknown.set(self.unknown.get() + 1);
        } else if incoming_cpu as usize == worker_cpu {
            self.hits.set(self.hits.get() + 1);
        } else {
            self.misses.set(self.misses.get() + 1);
        }
    }

    /// Read and reset all counters. Returns (hits, misses, unknown).
    pub fn drain(&self) -> (u64, u64, u64) {
        let result = (self.hits.get(), self.misses.get(), self.unknown.get());
        self.hits.set(0);
        self.misses.set(0);
        self.unknown.set(0);
        result
    }
}

impl Default for CpuAffinityCounters {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared TCP health observation counters from sampled getsockopt(TCP_INFO).
///
/// Same thread-per-core model as `CpuAffinityCounters` — `Cell` is sufficient.
pub struct TcpHealthCounters {
    pub rtt_sum_us: std::cell::Cell<u64>,
    pub rtt_count: std::cell::Cell<u64>,
    pub rtt_max_us: std::cell::Cell<u64>,
    pub retransmits: std::cell::Cell<u64>,
    pub lost: std::cell::Cell<u64>,
}

impl TcpHealthCounters {
    pub fn new() -> Self {
        Self {
            rtt_sum_us: std::cell::Cell::new(0),
            rtt_count: std::cell::Cell::new(0),
            rtt_max_us: std::cell::Cell::new(0),
            retransmits: std::cell::Cell::new(0),
            lost: std::cell::Cell::new(0),
        }
    }

    /// Record a TCP_INFO observation.
    #[inline]
    pub fn record(&self, info: &libc::tcp_info) {
        let rtt = info.tcpi_rtt as u64;
        self.rtt_sum_us.set(self.rtt_sum_us.get() + rtt);
        self.rtt_count.set(self.rtt_count.get() + 1);
        if rtt > self.rtt_max_us.get() {
            self.rtt_max_us.set(rtt);
        }
        self.retransmits
            .set(self.retransmits.get() + info.tcpi_total_retrans as u64);
        self.lost.set(self.lost.get() + info.tcpi_lost as u64);
    }

    /// Read and reset all counters.
    pub fn drain(&self) -> TcpHealthSnapshot {
        let snap = TcpHealthSnapshot {
            rtt_sum_us: self.rtt_sum_us.get(),
            rtt_count: self.rtt_count.get(),
            rtt_max_us: self.rtt_max_us.get(),
            retransmits: self.retransmits.get(),
            lost: self.lost.get(),
        };
        self.rtt_sum_us.set(0);
        self.rtt_count.set(0);
        self.rtt_max_us.set(0);
        self.retransmits.set(0);
        self.lost.set(0);
        snap
    }
}

impl Default for TcpHealthCounters {
    fn default() -> Self {
        Self::new()
    }
}

/// Snapshot of TCP health counters for a metrics window.
#[derive(Debug, Clone, Copy, Default)]
pub struct TcpHealthSnapshot {
    pub rtt_sum_us: u64,
    pub rtt_count: u64,
    pub rtt_max_us: u64,
    pub retransmits: u64,
    pub lost: u64,
}

/// Bundle of per-core health counter references.
///
/// Passed from the engine to the executor factory so the executor
/// and the metrics collector share the same counters.
#[derive(Clone)]
pub struct HealthCounters {
    pub affinity: std::rc::Rc<CpuAffinityCounters>,
    pub tcp_health: std::rc::Rc<TcpHealthCounters>,
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
    /// Requests that completed due to timeout in this window.
    /// Timeouts are excluded from the latency histogram.
    pub timeout_count: u64,
    /// Requests declined by the per-core in-flight limit this window.
    pub in_flight_drops: u64,
}

/// Output of a RateController: new target rate.
#[derive(Debug, Clone)]
pub struct RateDecision {
    pub target_rps: f64,
}

/// Client-side saturation assessment.
///
/// Distinguishes between client bottleneck (can't generate fast enough),
/// server bottleneck (can't handle the load), or both.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
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
    /// Fraction of sampled reads where RX CPU matches worker CPU.
    /// Only meaningful when --health-sample-rate > 0.
    pub cpu_affinity_ratio: f64,
    /// Mean TCP RTT in milliseconds from sampled TCP_INFO.
    pub tcp_rtt_mean_ms: f64,
    /// Max TCP RTT in milliseconds from sampled TCP_INFO.
    pub tcp_rtt_max_ms: f64,
    /// Ratio of retransmitted to total segments observed.
    pub tcp_retransmit_ratio: f64,
    /// Overall assessment.
    pub assessment: SaturationAssessment,
    /// Requests declined by the per-core in-flight limit this window.
    /// Semantically distinct from `backpressure_drops`: in-flight drops mean
    /// "target is too slow" (saturation signal), while backpressure drops mean
    /// "worker CPU-bound" (different failure mode).
    #[serde(default)]
    pub in_flight_drops: u64,
    /// Current number of in-flight requests across all cores.
    #[serde(default)]
    pub in_flight_count: u64,
    /// Per-core in-flight capacity (0 = unlimited).
    #[serde(default)]
    pub in_flight_capacity: u64,
}

/// Classification of where the bottleneck is.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
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
