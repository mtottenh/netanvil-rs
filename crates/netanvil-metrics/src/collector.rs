use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::Instant;

use hdrhistogram::Histogram;
use netanvil_types::config::ResponseSignalConfig;
use netanvil_types::metrics::ResponseSignalAccumulator;
use netanvil_types::{
    ExecutionResult, MetricsCollector, MetricsSnapshot, NoopPacketSource, PacketCounterSource,
};

/// HDR histogram-based metrics collector for a single core.
///
/// Uses `RefCell`/`Cell` for interior mutability — this is `!Send` by design.
/// Each core gets its own collector, shared via `Rc` with spawned tasks.
///
/// Generic over `P: PacketCounterSource` so protocol-specific executors can
/// inject packet-level counters (UDP loss bitmap, TCP_INFO, etc.) that get
/// populated inside `snapshot()`.  Defaults to [`NoopPacketSource`] (zero cost).
pub struct HdrMetricsCollector<P: PacketCounterSource = NoopPacketSource> {
    histogram: RefCell<Histogram<u64>>,
    size_histogram: RefCell<Histogram<u64>>,
    total_requests: Cell<u64>,
    total_errors: Cell<u64>,
    bytes_sent: Cell<u64>,
    bytes_received: Cell<u64>,
    window_start: Cell<Instant>,
    /// HTTP status codes >= this threshold count as errors.
    /// 0 means only transport errors count (error field set).
    error_status_threshold: u16,
    // Decomposed scheduling delay tracking (per-window, reset on snapshot).
    timer_lag_sum_ns: Cell<u64>,
    timer_lag_max_ns: Cell<u64>,
    channel_transit_sum_ns: Cell<u64>,
    channel_transit_max_ns: Cell<u64>,
    dispatch_gap_sum_ns: Cell<u64>,
    dispatch_gap_max_ns: Cell<u64>,
    total_delay_sum_ns: Cell<u64>,
    total_delay_max_ns: Cell<u64>,
    total_delay_count_over_1ms: Cell<u64>,
    /// Response header names to track value distributions for.
    tracked_headers: Vec<String>,
    /// Per-header value counts: header_name -> (header_value -> count).
    header_counts: RefCell<HashMap<String, HashMap<String, u64>>>,
    /// Whether MD5 body checking is enabled.
    md5_check_enabled: bool,
    /// Number of MD5 mismatches in this window.
    md5_mismatches: Cell<u64>,
    /// Response signal extraction configs.
    response_signal_configs: Vec<ResponseSignalConfig>,
    /// Per-signal accumulators for the current window.
    response_signal_accumulators: RefCell<HashMap<String, ResponseSignalAccumulator>>,
    /// Shared CPU affinity counters. Written by ObservedTcpStream, drained
    /// by snapshot(). None when health sampling is disabled.
    affinity_counters: Option<std::rc::Rc<netanvil_types::CpuAffinityCounters>>,
    /// Shared TCP health counters. Written by ObservedTcpStream, drained
    /// by snapshot(). None when health sampling is disabled.
    tcp_health_counters: Option<std::rc::Rc<netanvil_types::TcpHealthCounters>>,
    /// Protocol-level packet counter source (UDP loss, TCP_INFO, etc.).
    packet_source: P,
    /// Requests that completed due to timeout. Tracked separately because
    /// timeout "latency" (= timeout duration) is not real server response
    /// time and should not contaminate the latency histogram.
    timeout_count: Cell<u64>,
}

impl HdrMetricsCollector {
    /// Create a new collector with no packet counter source.
    pub fn new(
        error_status_threshold: u16,
        tracked_headers: Vec<String>,
        md5_check_enabled: bool,
    ) -> Self {
        Self::with_signal_configs(
            error_status_threshold,
            tracked_headers,
            md5_check_enabled,
            vec![],
        )
    }

    /// Create a collector with response signal extraction (no packet source).
    pub fn with_signal_configs(
        error_status_threshold: u16,
        tracked_headers: Vec<String>,
        md5_check_enabled: bool,
        response_signal_configs: Vec<ResponseSignalConfig>,
    ) -> Self {
        Self::with_packet_source(
            error_status_threshold,
            tracked_headers,
            md5_check_enabled,
            response_signal_configs,
            NoopPacketSource,
        )
    }
}

impl<P: PacketCounterSource> HdrMetricsCollector<P> {
    /// Create a collector with a protocol-specific packet counter source.
    pub fn with_packet_source(
        error_status_threshold: u16,
        tracked_headers: Vec<String>,
        md5_check_enabled: bool,
        response_signal_configs: Vec<ResponseSignalConfig>,
        packet_source: P,
    ) -> Self {
        Self {
            histogram: RefCell::new(
                Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("valid histogram params"),
            ),
            size_histogram: RefCell::new(
                Histogram::<u64>::new_with_bounds(1, 1_073_741_824, 3)
                    .expect("valid histogram params"),
            ),
            total_requests: Cell::new(0),
            total_errors: Cell::new(0),
            bytes_sent: Cell::new(0),
            bytes_received: Cell::new(0),
            window_start: Cell::new(Instant::now()),
            error_status_threshold,
            timer_lag_sum_ns: Cell::new(0),
            timer_lag_max_ns: Cell::new(0),
            channel_transit_sum_ns: Cell::new(0),
            channel_transit_max_ns: Cell::new(0),
            dispatch_gap_sum_ns: Cell::new(0),
            dispatch_gap_max_ns: Cell::new(0),
            total_delay_sum_ns: Cell::new(0),
            total_delay_max_ns: Cell::new(0),
            total_delay_count_over_1ms: Cell::new(0),
            tracked_headers,
            header_counts: RefCell::new(HashMap::new()),
            md5_check_enabled,
            md5_mismatches: Cell::new(0),
            response_signal_configs,
            response_signal_accumulators: RefCell::new(HashMap::new()),
            affinity_counters: None,
            tcp_health_counters: None,
            packet_source,
            timeout_count: Cell::new(0),
        }
    }
}

impl<P: PacketCounterSource> HdrMetricsCollector<P> {
    /// Set the shared CPU affinity counters (written by ObservedTcpStream,
    /// drained by snapshot()). Call once during per-core setup.
    pub fn set_affinity_counters(
        &mut self,
        counters: std::rc::Rc<netanvil_types::CpuAffinityCounters>,
    ) {
        self.affinity_counters = Some(counters);
    }

    /// Set the shared TCP health counters.
    pub fn set_tcp_health_counters(
        &mut self,
        counters: std::rc::Rc<netanvil_types::TcpHealthCounters>,
    ) {
        self.tcp_health_counters = Some(counters);
    }
}

impl<P: PacketCounterSource> MetricsCollector for HdrMetricsCollector<P> {
    fn record(&self, result: &ExecutionResult) {
        self.total_requests.set(self.total_requests.get() + 1);

        // Timeouts are tracked separately. A timeout's "latency" is the
        // timeout duration, not the server's response time — recording it
        // in the histogram would corrupt p99 with 30-second sentinel values.
        let is_timeout = matches!(result.error, Some(netanvil_types::ExecutionError::Timeout));
        if is_timeout {
            self.timeout_count.set(self.timeout_count.get() + 1);
            self.total_errors.set(self.total_errors.get() + 1);
            self.bytes_sent
                .set(self.bytes_sent.get() + result.bytes_sent);
            // Skip histogram, size histogram, and scheduling delay recording.
            return;
        }

        // Count as error if: transport error, OR HTTP status >= threshold
        let is_error = result.error.is_some()
            || (self.error_status_threshold > 0
                && result
                    .status
                    .is_some_and(|s| s >= self.error_status_threshold));

        if is_error {
            self.total_errors.set(self.total_errors.get() + 1);
        }

        self.bytes_sent
            .set(self.bytes_sent.get() + result.bytes_sent);
        self.bytes_received
            .set(self.bytes_received.get() + result.response_size);

        // Record total latency in nanoseconds
        let latency_ns = result.timing.total.as_nanos() as u64;
        let _ = self.histogram.borrow_mut().record(latency_ns.max(1));

        // Track decomposed scheduling delay for saturation detection.
        let timer_lag_ns = result.sent_time.saturating_duration_since(result.intended_time).as_nanos() as u64;
        let channel_transit_ns = result.actual_time.saturating_duration_since(result.sent_time).as_nanos() as u64;
        let dispatch_gap_ns = result.dispatch_time.saturating_duration_since(result.actual_time).as_nanos() as u64;
        let total_delay_ns = result.dispatch_time.saturating_duration_since(result.intended_time).as_nanos() as u64;

        self.timer_lag_sum_ns.set(self.timer_lag_sum_ns.get() + timer_lag_ns);
        if timer_lag_ns > self.timer_lag_max_ns.get() {
            self.timer_lag_max_ns.set(timer_lag_ns);
        }
        self.channel_transit_sum_ns.set(self.channel_transit_sum_ns.get() + channel_transit_ns);
        if channel_transit_ns > self.channel_transit_max_ns.get() {
            self.channel_transit_max_ns.set(channel_transit_ns);
        }
        self.dispatch_gap_sum_ns.set(self.dispatch_gap_sum_ns.get() + dispatch_gap_ns);
        if dispatch_gap_ns > self.dispatch_gap_max_ns.get() {
            self.dispatch_gap_max_ns.set(dispatch_gap_ns);
        }
        self.total_delay_sum_ns.set(self.total_delay_sum_ns.get() + total_delay_ns);
        if total_delay_ns > self.total_delay_max_ns.get() {
            self.total_delay_max_ns.set(total_delay_ns);
        }
        if total_delay_ns > 1_000_000 {
            // > 1ms
            self.total_delay_count_over_1ms
                .set(self.total_delay_count_over_1ms.get() + 1);
        }

        // Coordinated omission correction: if the request was delayed
        // (dispatch_time significantly after intended_time), also record
        // the corrected latency so the histogram reflects what a user
        // at the intended time would have experienced.
        // Uses total_delay (intent-to-dispatch) not partial (intent-to-dequeue).
        if total_delay_ns > 100_000 {
            // > 100μs
            let corrected_ns = latency_ns + total_delay_ns;
            let _ = self.histogram.borrow_mut().record(corrected_ns.max(1));
        }

        // Record response size into size histogram
        if result.response_size > 0 {
            let _ = self
                .size_histogram
                .borrow_mut()
                .record(result.response_size.max(1));
        }

        // Track response header value distributions
        if !self.tracked_headers.is_empty() {
            if let Some(ref headers) = result.response_headers {
                let mut counts = self.header_counts.borrow_mut();
                for tracked in &self.tracked_headers {
                    let tracked_lower = tracked.to_lowercase();
                    for (name, value) in headers {
                        if name.to_lowercase() == tracked_lower {
                            let entry = counts.entry(tracked.clone()).or_default();
                            *entry.entry(value.clone()).or_insert(0) += 1;
                        }
                    }
                }
            }
        }

        // Extract numeric signals from response headers
        if !self.response_signal_configs.is_empty() {
            if let Some(ref headers) = result.response_headers {
                let mut accumulators = self.response_signal_accumulators.borrow_mut();
                for config in &self.response_signal_configs {
                    let header_lower = config.header.to_lowercase();
                    for (name, value) in headers {
                        if name.to_lowercase() == header_lower {
                            if let Ok(v) = value.parse::<f64>() {
                                let acc = accumulators
                                    .entry(config.signal_name().to_string())
                                    .or_default();
                                acc.sum += v;
                                acc.count += 1;
                                if v > acc.max {
                                    acc.max = v;
                                }
                                acc.last = v;
                            }
                        }
                    }
                }
            }
        }

        // MD5 body verification
        if self.md5_check_enabled {
            if let Some(ref body) = result.response_body {
                if let Some(ref headers) = result.response_headers {
                    // Look for Content-MD5 header
                    let expected_md5 = headers.iter().find_map(|(k, v)| {
                        if k.eq_ignore_ascii_case("content-md5") {
                            Some(v.as_str())
                        } else {
                            None
                        }
                    });
                    if let Some(expected) = expected_md5 {
                        let computed = format!("{:x}", md5::compute(body));
                        // Content-MD5 can be hex or base64; try both
                        let matches =
                            computed == expected || computed.eq_ignore_ascii_case(expected);
                        if !matches {
                            self.md5_mismatches.set(self.md5_mismatches.get() + 1);
                        }
                    }
                }
            }
        }
    }

    fn snapshot(&self) -> MetricsSnapshot {
        let now = Instant::now();
        let pkt_deltas = self.packet_source.take_packet_deltas();

        // Clone histograms (memcpy of counts Vec), then reset originals
        // in-place so the allocation is reused for the next window.
        let latency_hist = self.histogram.borrow().clone();
        let size_hist = self.size_histogram.borrow().clone();

        // Clone and reset header counts
        let header_counts = {
            let mut counts = self.header_counts.borrow_mut();
            let cloned = counts.clone();
            counts.clear();
            cloned
        };

        // Drain shared CPU affinity counters (atomically read and reset).
        let (aff_hits, aff_misses, aff_unknown) = self
            .affinity_counters
            .as_ref()
            .map(|c| c.drain())
            .unwrap_or((0, 0, 0));

        // Drain shared TCP health counters.
        let tcp_snap = self
            .tcp_health_counters
            .as_ref()
            .map(|c| c.drain())
            .unwrap_or_default();

        let snapshot = MetricsSnapshot {
            latency_histogram: latency_hist,
            total_requests: self.total_requests.get(),
            total_errors: self.total_errors.get(),
            bytes_sent: self.bytes_sent.get(),
            bytes_received: self.bytes_received.get(),
            window_start: self.window_start.get(),
            window_end: now,
            timer_lag_sum_ns: self.timer_lag_sum_ns.get(),
            timer_lag_max_ns: self.timer_lag_max_ns.get(),
            channel_transit_sum_ns: self.channel_transit_sum_ns.get(),
            channel_transit_max_ns: self.channel_transit_max_ns.get(),
            dispatch_gap_sum_ns: self.dispatch_gap_sum_ns.get(),
            dispatch_gap_max_ns: self.dispatch_gap_max_ns.get(),
            total_delay_sum_ns: self.total_delay_sum_ns.get(),
            total_delay_max_ns: self.total_delay_max_ns.get(),
            total_delay_count_over_1ms: self.total_delay_count_over_1ms.get(),
            header_value_counts: header_counts,
            response_size_histogram: size_hist,
            md5_mismatches: self.md5_mismatches.get(),
            response_signals: {
                let mut acc = self.response_signal_accumulators.borrow_mut();
                let cloned = acc.clone();
                acc.clear();
                cloned
            },
            cpu_affinity_hits: aff_hits,
            cpu_affinity_misses: aff_misses,
            cpu_affinity_unknown: aff_unknown,
            tcp_rtt_sum_us: tcp_snap.rtt_sum_us,
            tcp_rtt_count: tcp_snap.rtt_count,
            tcp_rtt_max_us: tcp_snap.rtt_max_us,
            tcp_retransmits: tcp_snap.retransmits,
            tcp_lost: tcp_snap.lost,
            packets_sent: pkt_deltas.sent,
            packets_received: pkt_deltas.received,
            packets_lost: pkt_deltas.lost,
            timeout_count: self.timeout_count.get(),
            in_flight_drops: 0,    // stamped by io_worker before sending
            in_flight_count: 0,    // stamped by io_worker before sending
            in_flight_capacity: 0, // stamped by io_worker before sending
        };

        // Reset for next window (keeps existing allocations)
        self.histogram.borrow_mut().reset();
        self.size_histogram.borrow_mut().reset();
        self.total_requests.set(0);
        self.total_errors.set(0);
        self.bytes_sent.set(0);
        self.bytes_received.set(0);
        self.window_start.set(now);
        self.timer_lag_sum_ns.set(0);
        self.timer_lag_max_ns.set(0);
        self.channel_transit_sum_ns.set(0);
        self.channel_transit_max_ns.set(0);
        self.dispatch_gap_sum_ns.set(0);
        self.dispatch_gap_max_ns.set(0);
        self.total_delay_sum_ns.set(0);
        self.total_delay_max_ns.set(0);
        self.total_delay_count_over_1ms.set(0);
        self.md5_mismatches.set(0);
        self.timeout_count.set(0);

        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use netanvil_types::{ExecutionError, TimingBreakdown};
    use std::time::Duration;

    fn make_result(latency: Duration, error: Option<ExecutionError>) -> ExecutionResult {
        let now = Instant::now();
        ExecutionResult {
            request_id: 0,
            intended_time: now,
            actual_time: now,
            sent_time: now,
            dispatch_time: now,
            timing: TimingBreakdown {
                total: latency,
                ..Default::default()
            },
            status: if error.is_none() {
                Some(200)
            } else {
                Some(500)
            },
            bytes_sent: 0,
            response_size: 1024,
            error,
            response_headers: None,
            response_body: None,
        }
    }

    #[test]
    fn record_and_snapshot() {
        let collector = HdrMetricsCollector::new(0, vec![], false);

        // Record 100 requests with 10ms latency
        for _ in 0..100 {
            collector.record(&make_result(Duration::from_millis(10), None));
        }

        let snapshot = collector.snapshot();
        assert_eq!(snapshot.total_requests, 100);
        assert_eq!(snapshot.total_errors, 0);
        assert_eq!(snapshot.bytes_received, 100 * 1024);

        // After snapshot, counters are reset
        let snapshot2 = collector.snapshot();
        assert_eq!(snapshot2.total_requests, 0);
    }

    #[test]
    fn error_counting() {
        let collector = HdrMetricsCollector::new(0, vec![], false);

        collector.record(&make_result(Duration::from_millis(10), None));
        collector.record(&make_result(
            Duration::from_millis(10),
            Some(ExecutionError::Timeout),
        ));
        collector.record(&make_result(Duration::from_millis(10), None));

        let snapshot = collector.snapshot();
        assert_eq!(snapshot.total_requests, 3);
        assert_eq!(snapshot.total_errors, 1);
    }

    #[test]
    fn coordinated_omission_correction() {
        let collector = HdrMetricsCollector::new(0, vec![], false);
        let now = Instant::now();

        // Normal request: intended == actual
        collector.record(&ExecutionResult {
            request_id: 0,
            intended_time: now,
            actual_time: now,
            sent_time: now,
            dispatch_time: now,
            timing: TimingBreakdown {
                total: Duration::from_millis(5),
                ..Default::default()
            },
            status: Some(200),
            bytes_sent: 0,
            response_size: 0,
            error: None,
            response_headers: None,
            response_body: None,
        });

        // Delayed request: 50ms queue delay
        let intended = now;
        let actual = now + Duration::from_millis(50);
        collector.record(&ExecutionResult {
            request_id: 1,
            intended_time: intended,
            actual_time: actual,
            sent_time: actual,
            dispatch_time: actual,
            timing: TimingBreakdown {
                total: Duration::from_millis(5),
                ..Default::default()
            },
            status: Some(200),
            bytes_sent: 0,
            response_size: 0,
            error: None,
            response_headers: None,
            response_body: None,
        });

        let snapshot = collector.snapshot();
        // 2 real requests, but the delayed one generates an extra histogram entry
        assert_eq!(snapshot.total_requests, 2);
        // Histogram should have 3 entries (2 real + 1 corrected)
        assert_eq!(snapshot.latency_histogram.len(), 3);
    }
}
