use std::time::{Duration, Instant};

use netanvil_core::{ConstantRateScheduler, PoissonScheduler};
use netanvil_types::RequestScheduler;

// ---------------------------------------------------------------------------
// ConstantRateScheduler behavioral tests
// ---------------------------------------------------------------------------

#[test]
fn constant_rate_produces_correct_number_of_requests_over_time() {
    let start = Instant::now();
    let mut sched = ConstantRateScheduler::new(1000.0, start, Some(Duration::from_secs(1)));

    let mut count = 0;
    while sched.next_request_time().is_some() {
        count += 1;
    }

    // 1000 RPS for 1 second should produce exactly 1000 requests
    assert_eq!(count, 1000);
}

#[test]
fn constant_rate_spacing_is_uniform() {
    let start = Instant::now();
    let mut sched = ConstantRateScheduler::new(100.0, start, Some(Duration::from_secs(1)));

    let mut times = Vec::new();
    while let Some(t) = sched.next_request_time() {
        times.push(t);
    }

    // Check inter-arrival times are all equal
    let expected_interval = Duration::from_millis(10); // 1/100 = 10ms
    for pair in times.windows(2) {
        let gap = pair[1].duration_since(pair[0]);
        assert_eq!(gap, expected_interval, "spacing should be exactly 10ms");
    }
}

#[test]
fn constant_rate_update_changes_spacing_for_subsequent_requests() {
    let start = Instant::now();
    let mut sched = ConstantRateScheduler::new(100.0, start, None);

    // Consume 10 requests at 100 RPS.
    // After each call, next_time advances by 10ms (old interval).
    for _ in 0..10 {
        sched.next_request_time().unwrap();
    }

    // Double the rate BEFORE the next call
    sched.update_rate(200.0);

    // The next call returns the already-computed next_time and advances
    // by the NEW interval (5ms). So from this point, gaps are 5ms.
    let t0 = sched.next_request_time().unwrap();
    let t1 = sched.next_request_time().unwrap();
    let t2 = sched.next_request_time().unwrap();

    assert_eq!(t1.duration_since(t0), Duration::from_millis(5));
    assert_eq!(t2.duration_since(t1), Duration::from_millis(5));
}

#[test]
fn constant_rate_terminates_at_duration() {
    let start = Instant::now();
    let mut sched = ConstantRateScheduler::new(10.0, start, Some(Duration::from_millis(500)));

    let mut count = 0;
    while sched.next_request_time().is_some() {
        count += 1;
    }

    // 10 RPS for 0.5s = 5 requests
    assert_eq!(count, 5);
}

#[test]
fn constant_rate_zero_rps_does_not_panic() {
    let start = Instant::now();
    let mut sched = ConstantRateScheduler::new(0.0, start, Some(Duration::from_millis(100)));

    // Should still produce some requests (at 1 RPS fallback),
    // or at least not panic. With 1 RPS and 100ms window, we get 0 requests
    // after the first one (first at t=0, next at t=1000ms which is past end).
    let first = sched.next_request_time();
    assert!(first.is_some());
}

// ---------------------------------------------------------------------------
// PoissonScheduler behavioral tests
// ---------------------------------------------------------------------------

#[test]
fn poisson_produces_approximately_correct_number_of_requests() {
    let start = Instant::now();
    let target_rps = 500.0;
    let duration = Duration::from_secs(10);
    let mut sched = PoissonScheduler::with_seed(target_rps, start, Some(duration), 42);

    let mut count = 0;
    while sched.next_request_time().is_some() {
        count += 1;
    }

    let expected = (target_rps * duration.as_secs_f64()) as i64;
    let deviation = ((count as i64 - expected).abs() as f64) / expected as f64;

    // Poisson: for 5000 expected events, standard deviation is sqrt(5000) ≈ 70.
    // Allow 10% tolerance (very generous for this sample size).
    assert!(
        deviation < 0.10,
        "expected ~{expected} requests, got {count} (deviation {:.1}%)",
        deviation * 100.0
    );
}

#[test]
fn poisson_inter_arrival_times_are_exponentially_distributed() {
    let start = Instant::now();
    let target_rps = 1000.0;
    let duration = Duration::from_secs(5);
    let mut sched = PoissonScheduler::with_seed(target_rps, start, Some(duration), 123);

    let mut times = Vec::new();
    while let Some(t) = sched.next_request_time() {
        times.push(t);
    }

    // Compute inter-arrival times
    let intervals: Vec<f64> = times
        .windows(2)
        .map(|w| w[1].duration_since(w[0]).as_secs_f64())
        .collect();

    // Mean should be approximately 1/rate = 1ms = 0.001s
    let mean: f64 = intervals.iter().sum::<f64>() / intervals.len() as f64;
    let expected_mean = 1.0 / target_rps;
    let mean_deviation = (mean - expected_mean).abs() / expected_mean;
    assert!(
        mean_deviation < 0.05,
        "mean inter-arrival {mean:.6}s, expected {expected_mean:.6}s (deviation {:.1}%)",
        mean_deviation * 100.0
    );

    // Coefficient of variation for exponential distribution should be ~1.0
    let variance: f64 =
        intervals.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / intervals.len() as f64;
    let cv = variance.sqrt() / mean;
    assert!(
        (cv - 1.0).abs() < 0.10,
        "CV should be ~1.0 for exponential, got {cv:.3}"
    );
}

#[test]
fn poisson_times_are_monotonically_increasing() {
    let start = Instant::now();
    let mut sched = PoissonScheduler::with_seed(500.0, start, Some(Duration::from_secs(2)), 99);

    let mut prev = start;
    while let Some(t) = sched.next_request_time() {
        assert!(t >= prev, "times must be monotonically increasing");
        prev = t;
    }
}

#[test]
fn poisson_rate_update_changes_average_interval() {
    let start = Instant::now();
    let mut sched = PoissonScheduler::with_seed(100.0, start, None, 77);

    // Sample 500 intervals at 100 RPS
    let mut intervals_before = Vec::new();
    let mut prev = sched.next_request_time().unwrap();
    for _ in 0..500 {
        let t = sched.next_request_time().unwrap();
        intervals_before.push(t.duration_since(prev).as_secs_f64());
        prev = t;
    }

    // Change to 400 RPS
    sched.update_rate(400.0);

    // Sample 500 more intervals
    let mut intervals_after = Vec::new();
    prev = sched.next_request_time().unwrap();
    for _ in 0..500 {
        let t = sched.next_request_time().unwrap();
        intervals_after.push(t.duration_since(prev).as_secs_f64());
        prev = t;
    }

    let mean_before: f64 = intervals_before.iter().sum::<f64>() / intervals_before.len() as f64;
    let mean_after: f64 = intervals_after.iter().sum::<f64>() / intervals_after.len() as f64;

    // After 4x rate increase, mean interval should be ~4x smaller
    let ratio = mean_before / mean_after;
    assert!(
        (ratio - 4.0).abs() < 1.0,
        "expected ~4x ratio, got {ratio:.2} (before={mean_before:.6}, after={mean_after:.6})"
    );
}

#[test]
fn poisson_deterministic_with_same_seed() {
    let start = Instant::now();
    let duration = Some(Duration::from_secs(1));

    let mut sched1 = PoissonScheduler::with_seed(200.0, start, duration, 42);
    let mut sched2 = PoissonScheduler::with_seed(200.0, start, duration, 42);

    let times1: Vec<_> = std::iter::from_fn(|| sched1.next_request_time()).collect();
    let times2: Vec<_> = std::iter::from_fn(|| sched2.next_request_time()).collect();

    assert_eq!(times1.len(), times2.len());
    for (a, b) in times1.iter().zip(times2.iter()) {
        assert_eq!(a, b);
    }
}
