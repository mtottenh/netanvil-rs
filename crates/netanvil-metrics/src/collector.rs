use std::cell::{Cell, RefCell};
use std::time::Instant;

use hdrhistogram::Histogram;
use netanvil_types::{ExecutionResult, MetricsCollector, MetricsSnapshot};

use crate::encoding::encode_histogram;

/// HDR histogram-based metrics collector for a single core.
///
/// Uses `RefCell`/`Cell` for interior mutability — this is `!Send` by design.
/// Each core gets its own collector, shared via `Rc` with spawned tasks.
pub struct HdrMetricsCollector {
    histogram: RefCell<Histogram<u64>>,
    total_requests: Cell<u64>,
    total_errors: Cell<u64>,
    bytes_sent: Cell<u64>,
    bytes_received: Cell<u64>,
    window_start: Cell<Instant>,
    /// HTTP status codes >= this threshold count as errors.
    /// 0 means only transport errors count (error field set).
    error_status_threshold: u16,
    // Scheduling delay tracking for saturation detection.
    // These are per-window, reset on snapshot().
    scheduling_delay_sum_ns: Cell<u64>,
    scheduling_delay_max_ns: Cell<u64>,
    scheduling_delay_count_over_1ms: Cell<u64>,
}

impl HdrMetricsCollector {
    /// Create a new collector.
    ///
    /// `error_status_threshold`: HTTP status codes >= this value are counted
    /// as errors. Use 400 for all client/server errors, 500 for server-only,
    /// or 0 to only count transport errors.
    pub fn new(error_status_threshold: u16) -> Self {
        Self {
            histogram: RefCell::new(
                Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("valid histogram params"),
            ),
            total_requests: Cell::new(0),
            total_errors: Cell::new(0),
            bytes_sent: Cell::new(0),
            bytes_received: Cell::new(0),
            window_start: Cell::new(Instant::now()),
            error_status_threshold,
            scheduling_delay_sum_ns: Cell::new(0),
            scheduling_delay_max_ns: Cell::new(0),
            scheduling_delay_count_over_1ms: Cell::new(0),
        }
    }
}

impl MetricsCollector for HdrMetricsCollector {
    fn record(&self, result: &ExecutionResult) {
        self.total_requests.set(self.total_requests.get() + 1);

        // Count as error if: transport error, OR HTTP status >= threshold
        let is_error = result.error.is_some()
            || (self.error_status_threshold > 0
                && result
                    .status
                    .is_some_and(|s| s >= self.error_status_threshold));

        if is_error {
            self.total_errors.set(self.total_errors.get() + 1);
        }

        self.bytes_received
            .set(self.bytes_received.get() + result.response_size);

        // Record total latency in nanoseconds
        let latency_ns = result.timing.total.as_nanos() as u64;
        let _ = self.histogram.borrow_mut().record(latency_ns.max(1));

        // Track scheduling delay for saturation detection.
        let queue_delay = result
            .actual_time
            .saturating_duration_since(result.intended_time);
        let delay_ns = queue_delay.as_nanos() as u64;

        self.scheduling_delay_sum_ns
            .set(self.scheduling_delay_sum_ns.get() + delay_ns);
        if delay_ns > self.scheduling_delay_max_ns.get() {
            self.scheduling_delay_max_ns.set(delay_ns);
        }
        if delay_ns > 1_000_000 {
            // > 1ms
            self.scheduling_delay_count_over_1ms
                .set(self.scheduling_delay_count_over_1ms.get() + 1);
        }

        // Coordinated omission correction: if the request was delayed
        // (actual_time significantly after intended_time), also record
        // the corrected latency so the histogram reflects what a user
        // at the intended time would have experienced.
        if queue_delay.as_micros() > 100 {
            let corrected_ns = latency_ns + delay_ns;
            let _ = self.histogram.borrow_mut().record(corrected_ns.max(1));
        }
    }

    fn snapshot(&self) -> MetricsSnapshot {
        let now = Instant::now();
        let hist = self.histogram.borrow();
        let bytes = encode_histogram(&hist).unwrap_or_default();

        let snapshot = MetricsSnapshot {
            latency_histogram_bytes: bytes,
            total_requests: self.total_requests.get(),
            total_errors: self.total_errors.get(),
            bytes_sent: self.bytes_sent.get(),
            bytes_received: self.bytes_received.get(),
            window_start: self.window_start.get(),
            window_end: now,
            scheduling_delay_sum_ns: self.scheduling_delay_sum_ns.get(),
            scheduling_delay_max_ns: self.scheduling_delay_max_ns.get(),
            scheduling_delay_count_over_1ms: self.scheduling_delay_count_over_1ms.get(),
        };

        // Reset for next window
        drop(hist);
        self.histogram.borrow_mut().reset();
        self.total_requests.set(0);
        self.total_errors.set(0);
        self.bytes_sent.set(0);
        self.bytes_received.set(0);
        self.window_start.set(now);
        self.scheduling_delay_sum_ns.set(0);
        self.scheduling_delay_max_ns.set(0);
        self.scheduling_delay_count_over_1ms.set(0);

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
            timing: TimingBreakdown {
                total: latency,
                ..Default::default()
            },
            status: if error.is_none() {
                Some(200)
            } else {
                Some(500)
            },
            response_size: 1024,
            error,
        }
    }

    #[test]
    fn record_and_snapshot() {
        let collector = HdrMetricsCollector::new(0);

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
        let collector = HdrMetricsCollector::new(0);

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
        let collector = HdrMetricsCollector::new(0);
        let now = Instant::now();

        // Normal request: intended == actual
        collector.record(&ExecutionResult {
            request_id: 0,
            intended_time: now,
            actual_time: now,
            timing: TimingBreakdown {
                total: Duration::from_millis(5),
                ..Default::default()
            },
            status: Some(200),
            response_size: 0,
            error: None,
        });

        // Delayed request: 50ms queue delay
        let intended = now;
        let actual = now + Duration::from_millis(50);
        collector.record(&ExecutionResult {
            request_id: 1,
            intended_time: intended,
            actual_time: actual,
            timing: TimingBreakdown {
                total: Duration::from_millis(5),
                ..Default::default()
            },
            status: Some(200),
            response_size: 0,
            error: None,
        });

        let snapshot = collector.snapshot();
        // 2 real requests, but the delayed one generates an extra histogram entry
        assert_eq!(snapshot.total_requests, 2);
        // Histogram should have 3 entries (2 real + 1 corrected)
        let hist = crate::encoding::decode_histogram(&snapshot.latency_histogram_bytes).unwrap();
        assert_eq!(hist.len(), 3);
    }
}
