use std::collections::HashMap;
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use netanvil_types::config::SignalAggregation;
use netanvil_types::metrics::ResponseSignalAccumulator;
use netanvil_types::{MetricsSnapshot, MetricsSummary};

/// Aggregated metrics from multiple sources (cores or nodes).
///
/// Merge is associative and commutative for counters. The histogram
/// merge is also commutative. This property is what makes the coordinator
/// pattern work at both local and distributed levels.
pub struct AggregateMetrics {
    histogram: Histogram<u64>,
    size_histogram: Histogram<u64>,
    total_requests: u64,
    total_errors: u64,
    bytes_sent: u64,
    bytes_received: u64,
    window_start: Option<Instant>,
    window_end: Option<Instant>,
    source_count: usize,
    // Scheduling delay aggregates (composable: sum/max/sum)
    scheduling_delay_sum_ns: u64,
    scheduling_delay_max_ns: u64,
    scheduling_delay_count_over_1ms: u64,
    // Response header value distribution (composable: sum per key/value pair)
    header_value_counts: HashMap<String, HashMap<String, u64>>,
    // MD5 mismatch count (composable: sum)
    md5_mismatches: u64,
    // Response signal accumulators (composable: sum sums, max maxes, last = last seen)
    response_signals: HashMap<String, ResponseSignalAccumulator>,
    // Signal aggregation configs (set once, used in to_summary)
    signal_configs: Vec<netanvil_types::config::ResponseSignalConfig>,
    // CPU affinity tracking (composable: sum)
    cpu_affinity_hits: u64,
    cpu_affinity_misses: u64,
    cpu_affinity_unknown: u64,
    // TCP health tracking (composable: sum/max)
    tcp_rtt_sum_us: u64,
    tcp_rtt_count: u64,
    tcp_rtt_max_us: u64,
    tcp_retransmits: u64,
    tcp_lost: u64,
    // Protocol-level packet tracking (composable: sum)
    packets_sent: u64,
    packets_received: u64,
    packets_lost: u64,
    // Timeout tracking (composable: sum)
    timeout_count: u64,
    // In-flight tracking (composable: sum for drops, sum for count, max for capacity)
    in_flight_drops: u64,
    in_flight_count: u64,
    in_flight_capacity: u64,
}

impl AggregateMetrics {
    pub fn new() -> Self {
        Self {
            histogram: Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                .expect("valid histogram params"),
            size_histogram: Histogram::<u64>::new_with_bounds(1, 1_073_741_824, 3)
                .expect("valid histogram params"),
            total_requests: 0,
            total_errors: 0,
            bytes_sent: 0,
            bytes_received: 0,
            window_start: None,
            window_end: None,
            source_count: 0,
            scheduling_delay_sum_ns: 0,
            scheduling_delay_max_ns: 0,
            scheduling_delay_count_over_1ms: 0,
            header_value_counts: HashMap::new(),
            md5_mismatches: 0,
            response_signals: HashMap::new(),
            signal_configs: Vec::new(),
            cpu_affinity_hits: 0,
            cpu_affinity_misses: 0,
            cpu_affinity_unknown: 0,
            tcp_rtt_sum_us: 0,
            tcp_rtt_count: 0,
            tcp_rtt_max_us: 0,
            tcp_retransmits: 0,
            tcp_lost: 0,
            packets_sent: 0,
            packets_received: 0,
            packets_lost: 0,
            timeout_count: 0,
            in_flight_drops: 0,
            in_flight_count: 0,
            in_flight_capacity: 0,
        }
    }

    /// Set response signal aggregation configs (called once at setup).
    pub fn set_signal_configs(
        &mut self,
        configs: Vec<netanvil_types::config::ResponseSignalConfig>,
    ) {
        self.signal_configs = configs;
    }
}

impl Default for AggregateMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl AggregateMetrics {
    /// Merge a per-core snapshot into this aggregate.
    pub fn merge(&mut self, snapshot: &MetricsSnapshot) {
        let _ = self.histogram.add(&snapshot.latency_histogram);

        self.total_requests += snapshot.total_requests;
        self.total_errors += snapshot.total_errors;
        self.bytes_sent += snapshot.bytes_sent;
        self.bytes_received += snapshot.bytes_received;

        self.window_start = Some(match self.window_start {
            Some(existing) => existing.min(snapshot.window_start),
            None => snapshot.window_start,
        });
        self.window_end = Some(match self.window_end {
            Some(existing) => existing.max(snapshot.window_end),
            None => snapshot.window_end,
        });

        self.source_count += 1;

        // Scheduling delay: sum sums, max maxes, count sums
        self.scheduling_delay_sum_ns += snapshot.scheduling_delay_sum_ns;
        self.scheduling_delay_max_ns = self
            .scheduling_delay_max_ns
            .max(snapshot.scheduling_delay_max_ns);
        self.scheduling_delay_count_over_1ms += snapshot.scheduling_delay_count_over_1ms;

        // Merge response size histogram
        let _ = self.size_histogram.add(&snapshot.response_size_histogram);

        // Merge header value counts by summing
        for (header, values) in &snapshot.header_value_counts {
            let entry = self.header_value_counts.entry(header.clone()).or_default();
            for (value, count) in values {
                *entry.entry(value.clone()).or_insert(0) += count;
            }
        }

        // Merge MD5 mismatches by summing
        self.md5_mismatches += snapshot.md5_mismatches;

        // Merge response signal accumulators
        for (name, acc) in &snapshot.response_signals {
            let entry = self.response_signals.entry(name.clone()).or_default();
            entry.sum += acc.sum;
            entry.count += acc.count;
            if acc.max > entry.max {
                entry.max = acc.max;
            }
            entry.last = acc.last;
        }

        // Merge CPU affinity counters
        self.cpu_affinity_hits += snapshot.cpu_affinity_hits;
        self.cpu_affinity_misses += snapshot.cpu_affinity_misses;
        self.cpu_affinity_unknown += snapshot.cpu_affinity_unknown;

        // Merge TCP health counters
        self.tcp_rtt_sum_us += snapshot.tcp_rtt_sum_us;
        self.tcp_rtt_count += snapshot.tcp_rtt_count;
        self.tcp_rtt_max_us = self.tcp_rtt_max_us.max(snapshot.tcp_rtt_max_us);
        self.tcp_retransmits += snapshot.tcp_retransmits;
        self.tcp_lost += snapshot.tcp_lost;

        // Merge protocol-level packet counters
        self.packets_sent += snapshot.packets_sent;
        self.packets_received += snapshot.packets_received;
        self.packets_lost += snapshot.packets_lost;

        // Merge timeout counter
        self.timeout_count += snapshot.timeout_count;

        // Merge in-flight tracking
        self.in_flight_drops += snapshot.in_flight_drops;
        self.in_flight_count += snapshot.in_flight_count; // sum across cores
        self.in_flight_capacity += snapshot.in_flight_capacity; // sum across cores
    }

    /// Reset for the next aggregation window.
    pub fn reset(&mut self) {
        self.histogram.reset();
        self.size_histogram.reset();
        self.total_requests = 0;
        self.total_errors = 0;
        self.bytes_sent = 0;
        self.bytes_received = 0;
        self.window_start = None;
        self.window_end = None;
        self.source_count = 0;
        self.scheduling_delay_sum_ns = 0;
        self.scheduling_delay_max_ns = 0;
        self.scheduling_delay_count_over_1ms = 0;
        self.header_value_counts.clear();
        self.md5_mismatches = 0;
        self.response_signals.clear();
        self.cpu_affinity_hits = 0;
        self.cpu_affinity_misses = 0;
        self.cpu_affinity_unknown = 0;
        self.tcp_rtt_sum_us = 0;
        self.tcp_rtt_count = 0;
        self.tcp_rtt_max_us = 0;
        self.tcp_retransmits = 0;
        self.tcp_lost = 0;
        self.packets_sent = 0;
        self.packets_received = 0;
        self.packets_lost = 0;
        self.timeout_count = 0;
        self.in_flight_drops = 0;
        self.in_flight_count = 0;
        self.in_flight_capacity = 0;
    }

    /// Protocol-level packet counters: (sent, received, lost).
    pub fn packet_counters(&self) -> (u64, u64, u64) {
        (self.packets_sent, self.packets_received, self.packets_lost)
    }

    /// Compute derived metrics for the RateController.
    pub fn to_summary(&self) -> MetricsSummary {
        let window_duration = match (self.window_start, self.window_end) {
            (Some(start), Some(end)) => end.saturating_duration_since(start),
            _ => Duration::from_secs(1),
        };

        let secs = window_duration.as_secs_f64().max(0.001);

        MetricsSummary {
            total_requests: self.total_requests,
            total_errors: self.total_errors,
            error_rate: if self.total_requests > 0 {
                self.total_errors as f64 / self.total_requests as f64
            } else {
                0.0
            },
            request_rate: self.total_requests as f64 / secs,
            latency_p50_ns: self.histogram.value_at_quantile(0.50),
            latency_p90_ns: self.histogram.value_at_quantile(0.90),
            latency_p99_ns: self.histogram.value_at_quantile(0.99),
            window_duration,
            bytes_sent: self.bytes_sent,
            bytes_received: self.bytes_received,
            throughput_send_bps: self.bytes_sent as f64 / secs,
            throughput_recv_bps: self.bytes_received as f64 / secs,
            external_signals: self.compute_response_signals(),
            timeout_count: self.timeout_count,
            in_flight_drops: self.in_flight_drops,
        }
    }

    /// Compute aggregated response signal values from per-window accumulators.
    fn compute_response_signals(&self) -> Vec<(String, f64)> {
        let mut signals = Vec::new();
        for config in &self.signal_configs {
            let name = config.signal_name();
            if let Some(acc) = self.response_signals.get(name) {
                if acc.count == 0 {
                    continue;
                }
                let value = match config.aggregation {
                    SignalAggregation::Mean => acc.sum / acc.count as f64,
                    SignalAggregation::Max => acc.max,
                    SignalAggregation::Last => acc.last,
                };
                signals.push((name.to_string(), value));
            }
        }
        signals
    }

    /// Accessors for final reporting.
    pub fn total_requests(&self) -> u64 {
        self.total_requests
    }

    pub fn total_errors(&self) -> u64 {
        self.total_errors
    }

    pub fn histogram(&self) -> &Histogram<u64> {
        &self.histogram
    }

    pub fn source_count(&self) -> usize {
        self.source_count
    }

    pub fn scheduling_delay_sum_ns(&self) -> u64 {
        self.scheduling_delay_sum_ns
    }

    pub fn scheduling_delay_max_ns(&self) -> u64 {
        self.scheduling_delay_max_ns
    }

    pub fn scheduling_delay_count_over_1ms(&self) -> u64 {
        self.scheduling_delay_count_over_1ms
    }

    pub fn bytes_sent(&self) -> u64 {
        self.bytes_sent
    }

    pub fn bytes_received(&self) -> u64 {
        self.bytes_received
    }

    pub fn size_histogram(&self) -> &Histogram<u64> {
        &self.size_histogram
    }

    pub fn header_value_counts(&self) -> &HashMap<String, HashMap<String, u64>> {
        &self.header_value_counts
    }

    pub fn md5_mismatches(&self) -> u64 {
        self.md5_mismatches
    }

    pub fn response_signals(&self) -> &HashMap<String, ResponseSignalAccumulator> {
        &self.response_signals
    }

    pub fn cpu_affinity_hits(&self) -> u64 {
        self.cpu_affinity_hits
    }

    pub fn cpu_affinity_misses(&self) -> u64 {
        self.cpu_affinity_misses
    }

    pub fn cpu_affinity_unknown(&self) -> u64 {
        self.cpu_affinity_unknown
    }

    pub fn cpu_affinity_ratio(&self) -> f64 {
        let total = self.cpu_affinity_hits + self.cpu_affinity_misses;
        if total > 0 {
            self.cpu_affinity_hits as f64 / total as f64
        } else {
            0.0
        }
    }

    pub fn tcp_rtt_mean_us(&self) -> f64 {
        if self.tcp_rtt_count > 0 {
            self.tcp_rtt_sum_us as f64 / self.tcp_rtt_count as f64
        } else {
            0.0
        }
    }

    pub fn tcp_rtt_max_us(&self) -> u64 {
        self.tcp_rtt_max_us
    }

    pub fn tcp_retransmit_ratio(&self) -> f64 {
        let total = self.tcp_rtt_count; // one observation per sampled read
        if total > 0 {
            self.tcp_retransmits as f64 / total as f64
        } else {
            0.0
        }
    }

    pub fn timeout_count(&self) -> u64 {
        self.timeout_count
    }

    pub fn in_flight_drops(&self) -> u64 {
        self.in_flight_drops
    }

    pub fn in_flight_count(&self) -> u64 {
        self.in_flight_count
    }

    pub fn in_flight_capacity(&self) -> u64 {
        self.in_flight_capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snapshot(latencies_ms: &[u64], errors: u64) -> MetricsSnapshot {
        let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap();
        for &lat_ms in latencies_ms {
            hist.record(lat_ms * 1_000_000).unwrap();
        }

        let now = Instant::now();
        MetricsSnapshot {
            latency_histogram: hist,
            total_requests: latencies_ms.len() as u64,
            total_errors: errors,
            bytes_sent: 0,
            bytes_received: latencies_ms.len() as u64 * 1024,
            window_start: now,
            window_end: now + Duration::from_secs(1),
            scheduling_delay_sum_ns: 0,
            scheduling_delay_max_ns: 0,
            scheduling_delay_count_over_1ms: 0,
            header_value_counts: std::collections::HashMap::new(),
            response_size_histogram: Histogram::<u64>::new_with_bounds(1, 1_073_741_824, 3)
                .unwrap(),
            md5_mismatches: 0,
            response_signals: std::collections::HashMap::new(),
            cpu_affinity_hits: 0,
            cpu_affinity_misses: 0,
            cpu_affinity_unknown: 0,
            tcp_rtt_sum_us: 0,
            tcp_rtt_count: 0,
            tcp_rtt_max_us: 0,
            tcp_retransmits: 0,
            tcp_lost: 0,
            packets_sent: 0,
            packets_received: 0,
            packets_lost: 0,
        }
    }

    #[test]
    fn merge_two_snapshots() {
        let mut agg = AggregateMetrics::new();

        // Core 0: 50 requests, 10ms each
        let snap0 = make_snapshot(&vec![10; 50], 0);
        // Core 1: 50 requests, 20ms each, 2 errors
        let snap1 = make_snapshot(&vec![20; 50], 2);

        agg.merge(&snap0);
        agg.merge(&snap1);

        assert_eq!(agg.total_requests(), 100);
        assert_eq!(agg.total_errors(), 2);
        assert_eq!(agg.source_count(), 2);
        assert_eq!(agg.histogram().len(), 100);
    }

    #[test]
    fn to_summary_computes_percentiles() {
        let mut agg = AggregateMetrics::new();

        // Create a spread of latencies
        let mut latencies: Vec<u64> = Vec::new();
        for i in 1..=100 {
            latencies.push(i); // 1ms to 100ms
        }
        let snap = make_snapshot(&latencies, 5);
        agg.merge(&snap);

        let summary = agg.to_summary();
        assert_eq!(summary.total_requests, 100);
        assert_eq!(summary.total_errors, 5);
        assert!((summary.error_rate - 0.05).abs() < 0.001);
        // p50 should be around 50ms = 50_000_000 ns
        assert!(summary.latency_p50_ns > 40_000_000);
        assert!(summary.latency_p50_ns < 60_000_000);
        // p99 should be around 99ms
        assert!(summary.latency_p99_ns > 90_000_000);
    }

    #[test]
    fn merge_is_commutative_for_counters() {
        let snap_a = make_snapshot(&[10, 20, 30], 1);
        let snap_b = make_snapshot(&[40, 50], 0);

        // Order A, B
        let mut agg1 = AggregateMetrics::new();
        agg1.merge(&snap_a);
        agg1.merge(&snap_b);

        // Order B, A
        let mut agg2 = AggregateMetrics::new();
        agg2.merge(&snap_b);
        agg2.merge(&snap_a);

        assert_eq!(agg1.total_requests(), agg2.total_requests());
        assert_eq!(agg1.total_errors(), agg2.total_errors());
        assert_eq!(agg1.histogram().len(), agg2.histogram().len());
    }

    #[test]
    fn reset_clears_everything() {
        let mut agg = AggregateMetrics::new();
        agg.merge(&make_snapshot(&[10, 20], 1));
        assert_eq!(agg.total_requests(), 2);

        agg.reset();
        assert_eq!(agg.total_requests(), 0);
        assert_eq!(agg.total_errors(), 0);
        assert_eq!(agg.histogram().len(), 0);
        assert_eq!(agg.source_count(), 0);
    }
}
