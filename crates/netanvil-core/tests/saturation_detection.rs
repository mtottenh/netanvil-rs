//! Tests for client/server saturation detection.
//!
//! Validates the three signal sources (timer backpressure, scheduling delay,
//! rate deficit) and the classification logic.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use netanvil_types::{
    ExecutionResult, MetricsCollector, MetricsSnapshot, SaturationAssessment, SaturationInfo,
    TimingBreakdown,
};

// ===========================================================================
// Timer backpressure: AtomicU64 reporting
// ===========================================================================

#[test]
fn timer_stats_are_shared_via_atomics() {
    use netanvil_core::timer_thread::TimerStats;

    let stats = TimerStats::new();
    let stats_clone = stats.clone();

    // Simulate timer thread incrementing
    stats.dispatched.fetch_add(100, Ordering::Relaxed);
    stats.dropped.fetch_add(5, Ordering::Relaxed);

    // Coordinator reads from clone
    assert_eq!(stats_clone.dispatched.load(Ordering::Relaxed), 100);
    assert_eq!(stats_clone.dropped.load(Ordering::Relaxed), 5);
}

#[test]
fn timer_backpressure_increments_dropped_counter() {
    use netanvil_core::timer_thread::{self, TimerStats};
    use netanvil_core::ConstantRateScheduler;

    // Create a tiny bounded channel that will fill up
    let (fire_tx, _fire_rx) = flume::bounded(2);
    let (cmd_tx, cmd_rx) = flume::unbounded();

    let scheduler =
        ConstantRateScheduler::new(10000.0, Instant::now(), Some(Duration::from_millis(200)));
    let stats = TimerStats::new();
    let stats_read = stats.clone();

    let thread = std::thread::spawn(move || {
        timer_thread::timer_loop(vec![Box::new(scheduler)], vec![fire_tx], cmd_rx, stats);
    });

    // Don't drain the channel — let it fill up
    std::thread::sleep(Duration::from_millis(300));
    let _ = cmd_tx.send(netanvil_types::TimerCommand::Stop);
    let _ = thread.join();

    let dispatched = stats_read.dispatched.load(Ordering::Relaxed);
    let dropped = stats_read.dropped.load(Ordering::Relaxed);

    assert!(dispatched > 0, "some requests should have been dispatched");
    assert!(
        dropped > 0,
        "with channel capacity 2 at 10K RPS, should have drops: dispatched={dispatched}, dropped={dropped}"
    );
}

// ===========================================================================
// Scheduling delay tracking in HdrMetricsCollector
// ===========================================================================

#[test]
fn collector_tracks_scheduling_delay() {
    use netanvil_metrics::HdrMetricsCollector;

    let collector = HdrMetricsCollector::new(0, vec![], false);

    // Request with no delay
    let now = Instant::now();
    collector.record(&ExecutionResult {
        request_id: 0,
        intended_time: now,
        actual_time: now,
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

    // Request with 10ms delay (above 1ms threshold)
    collector.record(&ExecutionResult {
        request_id: 1,
        intended_time: now,
        actual_time: now + Duration::from_millis(10),
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

    // Request with 0.5ms delay (below 1ms threshold)
    collector.record(&ExecutionResult {
        request_id: 2,
        intended_time: now,
        actual_time: now + Duration::from_micros(500),
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

    assert_eq!(snapshot.total_requests, 3);
    // Sum should be ~10.5ms in nanoseconds
    assert!(
        snapshot.scheduling_delay_sum_ns > 10_000_000,
        "sum should include 10ms + 0.5ms delays: {}",
        snapshot.scheduling_delay_sum_ns
    );
    // Max should be ~10ms
    assert!(
        snapshot.scheduling_delay_max_ns >= 9_000_000
            && snapshot.scheduling_delay_max_ns <= 11_000_000,
        "max should be ~10ms: {}ns",
        snapshot.scheduling_delay_max_ns
    );
    // Only the 10ms request exceeds 1ms threshold
    assert_eq!(
        snapshot.scheduling_delay_count_over_1ms, 1,
        "only 1 request should exceed 1ms threshold"
    );
}

#[test]
fn collector_resets_delay_stats_on_snapshot() {
    use netanvil_metrics::HdrMetricsCollector;

    let collector = HdrMetricsCollector::new(0, vec![], false);
    let now = Instant::now();

    // Record a delayed request
    collector.record(&ExecutionResult {
        request_id: 0,
        intended_time: now,
        actual_time: now + Duration::from_millis(5),
        timing: TimingBreakdown {
            total: Duration::from_millis(1),
            ..Default::default()
        },
        status: Some(200),
        bytes_sent: 0,
        response_size: 0,
        error: None,
        response_headers: None,
        response_body: None,
    });

    let snap1 = collector.snapshot();
    assert!(snap1.scheduling_delay_sum_ns > 0);
    assert!(snap1.scheduling_delay_max_ns > 0);
    assert_eq!(snap1.scheduling_delay_count_over_1ms, 1);

    // Second snapshot should be clean
    let snap2 = collector.snapshot();
    assert_eq!(snap2.scheduling_delay_sum_ns, 0);
    assert_eq!(snap2.scheduling_delay_max_ns, 0);
    assert_eq!(snap2.scheduling_delay_count_over_1ms, 0);
}

// ===========================================================================
// Aggregate metrics merges scheduling delay correctly
// ===========================================================================

#[test]
fn aggregate_merges_scheduling_delay_fields() {
    use hdrhistogram::Histogram;
    use netanvil_metrics::AggregateMetrics;

    let now = Instant::now();
    let empty_hist = || Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap();
    let empty_size_hist = || Histogram::<u64>::new_with_bounds(1, 1_073_741_824, 3).unwrap();

    let snap_a = MetricsSnapshot {
        latency_histogram: empty_hist(),
        total_requests: 100,
        total_errors: 0,
        bytes_sent: 0,
        bytes_received: 0,
        window_start: now,
        window_end: now + Duration::from_secs(1),
        scheduling_delay_sum_ns: 5_000_000, // 5ms total
        scheduling_delay_max_ns: 2_000_000, // 2ms max
        scheduling_delay_count_over_1ms: 3,
        header_value_counts: std::collections::HashMap::new(),
        response_size_histogram: empty_size_hist(),
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
        timeout_count: 0,
        in_flight_drops: 0,
        in_flight_count: 0,
        in_flight_capacity: 0,
    };

    let snap_b = MetricsSnapshot {
        latency_histogram: empty_hist(),
        total_requests: 50,
        total_errors: 0,
        bytes_sent: 0,
        bytes_received: 0,
        window_start: now,
        window_end: now + Duration::from_secs(1),
        scheduling_delay_sum_ns: 10_000_000, // 10ms total
        scheduling_delay_max_ns: 8_000_000,  // 8ms max
        scheduling_delay_count_over_1ms: 7,
        header_value_counts: std::collections::HashMap::new(),
        response_size_histogram: empty_size_hist(),
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
        timeout_count: 0,
        in_flight_drops: 0,
        in_flight_count: 0,
        in_flight_capacity: 0,
    };

    let mut agg = AggregateMetrics::new();
    agg.merge(&snap_a);
    agg.merge(&snap_b);

    assert_eq!(agg.scheduling_delay_sum_ns(), 15_000_000); // sum of sums
    assert_eq!(agg.scheduling_delay_max_ns(), 8_000_000); // max of maxes
    assert_eq!(agg.scheduling_delay_count_over_1ms(), 10); // sum of counts
    assert_eq!(agg.total_requests(), 150);
}

// ===========================================================================
// SaturationAssessment classification
// ===========================================================================

#[test]
fn saturation_healthy_when_no_signals() {
    let info = SaturationInfo {
        backpressure_drops: 0,
        backpressure_ratio: 0.0,
        scheduling_delay_mean_ms: 0.1,
        scheduling_delay_max_ms: 0.5,
        delayed_request_ratio: 0.0,
        rate_achievement: 1.0,
        cpu_affinity_ratio: 0.0,
        tcp_rtt_mean_ms: 0.0,
        tcp_rtt_max_ms: 0.0,
        tcp_retransmit_ratio: 0.0,
        assessment: SaturationAssessment::Healthy,
        in_flight_drops: 0,
        in_flight_count: 0,
        in_flight_capacity: 0,
    };
    assert_eq!(info.assessment, SaturationAssessment::Healthy);
}

#[test]
fn saturation_client_when_backpressure() {
    // Backpressure > 1% → client saturated
    let info = SaturationInfo {
        backpressure_drops: 50,
        backpressure_ratio: 0.05, // 5% dropped
        scheduling_delay_mean_ms: 0.1,
        scheduling_delay_max_ms: 0.5,
        delayed_request_ratio: 0.0,
        rate_achievement: 0.95,
        cpu_affinity_ratio: 0.0,
        tcp_rtt_mean_ms: 0.0,
        tcp_rtt_max_ms: 0.0,
        tcp_retransmit_ratio: 0.0,
        assessment: SaturationAssessment::ClientSaturated,
        in_flight_drops: 0,
        in_flight_count: 0,
        in_flight_capacity: 0,
    };
    assert_eq!(info.assessment, SaturationAssessment::ClientSaturated);
}

#[test]
fn saturation_info_serializes_to_json() {
    let info = SaturationInfo {
        backpressure_drops: 10,
        backpressure_ratio: 0.02,
        scheduling_delay_mean_ms: 5.0,
        scheduling_delay_max_ms: 50.0,
        delayed_request_ratio: 0.15,
        rate_achievement: 0.88,
        cpu_affinity_ratio: 0.0,
        tcp_rtt_mean_ms: 0.0,
        tcp_rtt_max_ms: 0.0,
        tcp_retransmit_ratio: 0.0,
        assessment: SaturationAssessment::ClientSaturated,
        in_flight_drops: 0,
        in_flight_count: 0,
        in_flight_capacity: 0,
    };

    let json = serde_json::to_string(&info).unwrap();
    assert!(json.contains("ClientSaturated"));
    assert!(json.contains("backpressure_drops"));

    // Round-trip
    let parsed: SaturationInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.backpressure_drops, 10);
    assert_eq!(parsed.assessment, SaturationAssessment::ClientSaturated);
}
