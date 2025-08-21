use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use netanvil_types::{MetricsSnapshot, MetricsSummary};

use crate::encoding::decode_histogram;

/// Aggregated metrics from multiple sources (cores or nodes).
///
/// Merge is associative and commutative for counters. The histogram
/// merge is also commutative. This property is what makes the coordinator
/// pattern work at both local and distributed levels.
pub struct AggregateMetrics {
    histogram: Histogram<u64>,
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
}

impl AggregateMetrics {
    pub fn new() -> Self {
        Self {
            histogram: Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
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
        }
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
        if let Ok(hist) = decode_histogram(&snapshot.latency_histogram_bytes) {
            let _ = self.histogram.add(&hist);
        }

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
    }

    /// Reset for the next aggregation window.
    pub fn reset(&mut self) {
        self.histogram.reset();
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
            external_signals: Vec::new(), // populated by the coordinator if external source configured
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::encode_histogram;

    fn make_snapshot(latencies_ms: &[u64], errors: u64) -> MetricsSnapshot {
        let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap();
        for &lat_ms in latencies_ms {
            hist.record(lat_ms * 1_000_000).unwrap();
        }

        let now = Instant::now();
        MetricsSnapshot {
            latency_histogram_bytes: encode_histogram(&hist).unwrap(),
            total_requests: latencies_ms.len() as u64,
            total_errors: errors,
            bytes_sent: 0,
            bytes_received: latencies_ms.len() as u64 * 1024,
            window_start: now,
            window_end: now + Duration::from_secs(1),
            scheduling_delay_sum_ns: 0,
            scheduling_delay_max_ns: 0,
            scheduling_delay_count_over_1ms: 0,
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
