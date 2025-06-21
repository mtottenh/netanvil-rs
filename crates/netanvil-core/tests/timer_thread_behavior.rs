//! Behavioral tests for the timer thread.
//!
//! These tests validate the timer thread in isolation: correct dispatch rate,
//! command forwarding, backpressure detection, and multi-worker distribution.

use std::time::{Duration, Instant};

use netanvil_core::ConstantRateScheduler;
use netanvil_core::PoissonScheduler;
use netanvil_core::timer_thread::{self, FIRE_CHANNEL_CAPACITY};
use netanvil_types::{RequestScheduler, ScheduledRequest, TimerCommand};

/// Helper: run the timer loop in a background thread, return the handle
/// and a vec of fire receivers.
fn spawn_timer(
    schedulers: Vec<Box<dyn RequestScheduler>>,
    num_workers: usize,
    channel_capacity: usize,
) -> (
    flume::Sender<TimerCommand>,
    Vec<flume::Receiver<ScheduledRequest>>,
    std::thread::JoinHandle<()>,
) {
    let mut fire_txs = Vec::with_capacity(num_workers);
    let mut fire_rxs = Vec::with_capacity(num_workers);
    for _ in 0..num_workers {
        let (tx, rx) = flume::bounded(channel_capacity);
        fire_txs.push(tx);
        fire_rxs.push(rx);
    }

    let (cmd_tx, cmd_rx) = flume::unbounded();

    let thread = std::thread::Builder::new()
        .name("test-timer".into())
        .spawn(move || {
            timer_thread::timer_loop(schedulers, fire_txs, cmd_rx);
        })
        .unwrap();

    (cmd_tx, fire_rxs, thread)
}

#[test]
fn timer_dispatches_at_correct_rate() {
    // 1 worker, 1000 RPS for 1 second → ~1000 Fire messages.
    let start = Instant::now();
    let schedulers: Vec<Box<dyn RequestScheduler>> = vec![Box::new(
        ConstantRateScheduler::new(1000.0, start, Some(Duration::from_secs(1))),
    )];

    let (_cmd_tx, fire_rxs, thread) = spawn_timer(schedulers, 1, FIRE_CHANNEL_CAPACITY);

    thread.join().unwrap();

    let mut count = 0u64;
    while let Ok(msg) = fire_rxs[0].try_recv() {
        if matches!(msg, ScheduledRequest::Fire(_)) {
            count += 1;
        }
    }

    // Allow 10% tolerance
    let expected = 1000u64;
    let lower = (expected as f64 * 0.90) as u64;
    let upper = (expected as f64 * 1.10) as u64;
    assert!(
        count >= lower && count <= upper,
        "expected ~{expected} fire events, got {count} (bounds: {lower}..{upper})"
    );
}

#[test]
fn timer_dispatches_at_high_rate() {
    // 1 worker, 5K RPS for 500ms → ~2500 Fire messages.
    // A drain thread reads from the channel to prevent backpressure.
    let start = Instant::now();
    let schedulers: Vec<Box<dyn RequestScheduler>> = vec![Box::new(
        ConstantRateScheduler::new(5_000.0, start, Some(Duration::from_millis(500))),
    )];

    let (_cmd_tx, fire_rxs, thread) = spawn_timer(schedulers, 1, FIRE_CHANNEL_CAPACITY);

    // Drain the channel in a background thread to prevent backpressure stalls
    let rx = fire_rxs.into_iter().next().unwrap();
    let drain_handle = std::thread::spawn(move || {
        let mut count = 0u64;
        loop {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(ScheduledRequest::Fire(_)) => count += 1,
                Ok(ScheduledRequest::Stop) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        count
    });

    thread.join().unwrap();
    let count = drain_handle.join().unwrap();

    let expected = 2500u64;
    let lower = (expected as f64 * 0.85) as u64;
    let upper = (expected as f64 * 1.15) as u64;
    assert!(
        count >= lower && count <= upper,
        "expected ~{expected} fire events at 5K RPS for 500ms, got {count}"
    );
}

#[test]
fn timer_rate_update_propagates_to_schedulers() {
    // Start at 100 RPS, after 500ms update to 400 RPS, run for 1s total.
    // Expected: ~50 (first 500ms at 100) + ~200 (last 500ms at 400) = ~250
    let start = Instant::now();
    let schedulers: Vec<Box<dyn RequestScheduler>> = vec![Box::new(
        ConstantRateScheduler::new(100.0, start, Some(Duration::from_secs(1))),
    )];

    let (cmd_tx, fire_rxs, thread) = spawn_timer(schedulers, 1, FIRE_CHANNEL_CAPACITY);

    // Send rate update after 500ms
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        let _ = cmd_tx.send(TimerCommand::UpdateRate(400.0));
    });

    thread.join().unwrap();

    let mut count = 0u64;
    while let Ok(msg) = fire_rxs[0].try_recv() {
        if matches!(msg, ScheduledRequest::Fire(_)) {
            count += 1;
        }
    }

    // ~50 + ~200 = ~250, allow generous tolerance
    assert!(
        count > 150,
        "expected ~250 with rate change from 100→400, got {count}"
    );
    assert!(
        count < 400,
        "too many requests with rate change, got {count}"
    );
}

#[test]
fn timer_target_update_forwarded_to_all_workers() {
    // 4 workers — send UpdateTargets, verify all workers receive it.
    let start = Instant::now();
    let mut schedulers: Vec<Box<dyn RequestScheduler>> = Vec::new();
    for _ in 0..4 {
        schedulers.push(Box::new(ConstantRateScheduler::new(
            10.0,
            start,
            Some(Duration::from_secs(1)),
        )));
    }

    let (cmd_tx, fire_rxs, thread) = spawn_timer(schedulers, 4, FIRE_CHANNEL_CAPACITY);

    // Send target update, then stop
    std::thread::sleep(Duration::from_millis(100));
    cmd_tx
        .send(TimerCommand::UpdateTargets(vec![
            "http://new.test/".into(),
        ]))
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    cmd_tx.send(TimerCommand::Stop).unwrap();

    thread.join().unwrap();

    // Each worker should have received the UpdateTargets message
    for (i, rx) in fire_rxs.iter().enumerate() {
        let mut found_update = false;
        while let Ok(msg) = rx.try_recv() {
            if matches!(msg, ScheduledRequest::UpdateTargets(_)) {
                found_update = true;
            }
        }
        assert!(
            found_update,
            "worker {i} did not receive UpdateTargets message"
        );
    }
}

#[test]
fn timer_header_update_forwarded_to_all_workers() {
    // 2 workers — send UpdateHeaders, verify both receive it.
    let start = Instant::now();
    let schedulers: Vec<Box<dyn RequestScheduler>> = vec![
        Box::new(ConstantRateScheduler::new(
            10.0,
            start,
            Some(Duration::from_secs(1)),
        )),
        Box::new(ConstantRateScheduler::new(
            10.0,
            start,
            Some(Duration::from_secs(1)),
        )),
    ];

    let (cmd_tx, fire_rxs, thread) = spawn_timer(schedulers, 2, FIRE_CHANNEL_CAPACITY);

    std::thread::sleep(Duration::from_millis(100));
    cmd_tx
        .send(TimerCommand::UpdateHeaders(vec![(
            "X-Test".into(),
            "value".into(),
        )]))
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    cmd_tx.send(TimerCommand::Stop).unwrap();

    thread.join().unwrap();

    for (i, rx) in fire_rxs.iter().enumerate() {
        let mut found_update = false;
        while let Ok(msg) = rx.try_recv() {
            if matches!(msg, ScheduledRequest::UpdateHeaders(_)) {
                found_update = true;
            }
        }
        assert!(
            found_update,
            "worker {i} did not receive UpdateHeaders message"
        );
    }
}

#[test]
fn timer_stop_terminates_thread_and_notifies_workers() {
    // Send Stop, verify thread exits and all workers get Stop.
    let start = Instant::now();
    let schedulers: Vec<Box<dyn RequestScheduler>> = vec![
        Box::new(ConstantRateScheduler::new(
            100.0,
            start,
            Some(Duration::from_secs(60)), // long duration — Stop should pre-empt
        )),
        Box::new(ConstantRateScheduler::new(
            100.0,
            start,
            Some(Duration::from_secs(60)),
        )),
    ];

    let (cmd_tx, fire_rxs, thread) = spawn_timer(schedulers, 2, FIRE_CHANNEL_CAPACITY);

    std::thread::sleep(Duration::from_millis(200));
    cmd_tx.send(TimerCommand::Stop).unwrap();

    let before = Instant::now();
    thread.join().unwrap();
    let join_elapsed = before.elapsed();

    assert!(
        join_elapsed < Duration::from_secs(2),
        "timer thread should exit promptly after Stop, took {:?}",
        join_elapsed
    );

    // Both workers should have received Stop
    for (i, rx) in fire_rxs.iter().enumerate() {
        let mut found_stop = false;
        while let Ok(msg) = rx.try_recv() {
            if matches!(msg, ScheduledRequest::Stop) {
                found_stop = true;
            }
        }
        assert!(
            found_stop,
            "worker {i} did not receive Stop message from timer"
        );
    }
}

#[test]
fn timer_backpressure_detection() {
    // Use a tiny channel capacity, moderate rate → timer should experience
    // backpressure (try_send failures) and continue without blocking.
    let start = Instant::now();
    let schedulers: Vec<Box<dyn RequestScheduler>> = vec![Box::new(
        ConstantRateScheduler::new(500.0, start, Some(Duration::from_millis(200))),
    )];

    // Channel capacity of 2: guaranteed to overflow at 500 RPS
    let (cmd_tx, fire_rxs, thread) = spawn_timer(schedulers, 1, 2);

    // Don't drain the receiver — let it fill up
    thread.join().unwrap();

    // The timer should have completed without blocking.
    // Count received messages — should be far fewer than expected 100
    // because the channel was full most of the time.
    let mut count = 0u64;
    while let Ok(msg) = fire_rxs[0].try_recv() {
        if matches!(msg, ScheduledRequest::Fire(_) | ScheduledRequest::Stop) {
            count += 1;
        }
    }

    // With capacity 2, we should have received very few messages
    // (only what fit + the stop). The key assertion is that the timer
    // thread completed — it didn't deadlock.
    assert!(
        count < 50,
        "with channel capacity 2, most messages should be dropped, got {count}"
    );

    drop(cmd_tx); // keep cmd_tx alive so the command channel doesn't disconnect early
}

#[test]
fn timer_round_robin_distribution() {
    // 4 workers at equal rate — each should get approximately 25% of messages.
    let start = Instant::now();
    let mut schedulers: Vec<Box<dyn RequestScheduler>> = Vec::new();
    for _ in 0..4 {
        schedulers.push(Box::new(ConstantRateScheduler::new(
            250.0, // 250 RPS per scheduler = 1000 total
            start,
            Some(Duration::from_secs(1)),
        )));
    }

    let (_cmd_tx, fire_rxs, thread) = spawn_timer(schedulers, 4, FIRE_CHANNEL_CAPACITY);

    thread.join().unwrap();

    let mut counts = [0u64; 4];
    for (i, rx) in fire_rxs.iter().enumerate() {
        while let Ok(msg) = rx.try_recv() {
            if matches!(msg, ScheduledRequest::Fire(_)) {
                counts[i] += 1;
            }
        }
    }

    let total: u64 = counts.iter().sum();
    assert!(
        total > 800 && total < 1200,
        "total should be ~1000, got {total}"
    );

    // Each worker should have ~250 requests (25%)
    for (i, &count) in counts.iter().enumerate() {
        let expected = total as f64 / 4.0;
        let lower = (expected * 0.60) as u64;
        let upper = (expected * 1.40) as u64;
        assert!(
            count >= lower && count <= upper,
            "worker {i} got {count} requests, expected ~{expected:.0} (bounds: {lower}..{upper})"
        );
    }
}

#[test]
fn timer_precision_intended_times_are_accurate() {
    // Verify that the intended_time in Fire messages matches what the
    // scheduler would produce (monotonically increasing, correctly spaced).
    let start = Instant::now();
    let schedulers: Vec<Box<dyn RequestScheduler>> = vec![Box::new(
        ConstantRateScheduler::new(100.0, start, Some(Duration::from_millis(500))),
    )];

    let (_cmd_tx, fire_rxs, thread) = spawn_timer(schedulers, 1, FIRE_CHANNEL_CAPACITY);

    thread.join().unwrap();

    let mut times = Vec::new();
    while let Ok(msg) = fire_rxs[0].try_recv() {
        if let ScheduledRequest::Fire(t) = msg {
            times.push(t);
        }
    }

    assert!(
        times.len() >= 40,
        "expected ~50 events in 500ms at 100 RPS, got {}",
        times.len()
    );

    // Check monotonicity
    for window in times.windows(2) {
        assert!(
            window[1] >= window[0],
            "intended times must be monotonically increasing"
        );
    }

    // Check spacing is approximately 10ms (1/100 RPS)
    let intervals: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let avg_interval = intervals.iter().sum::<Duration>() / intervals.len() as u32;
    let expected_interval = Duration::from_millis(10);

    // Allow 20% tolerance on average interval
    let lower = expected_interval.mul_f64(0.80);
    let upper = expected_interval.mul_f64(1.20);
    assert!(
        avg_interval >= lower && avg_interval <= upper,
        "average interval should be ~{expected_interval:?}, got {avg_interval:?}"
    );
}

#[test]
fn timer_with_poisson_scheduler_produces_correct_count() {
    // Verify timer thread works with Poisson scheduler (exponential inter-arrivals).
    let start = Instant::now();
    let schedulers: Vec<Box<dyn RequestScheduler>> = vec![Box::new(
        PoissonScheduler::with_seed(500.0, start, Some(Duration::from_secs(1)), 42),
    )];

    let (_cmd_tx, fire_rxs, thread) = spawn_timer(schedulers, 1, FIRE_CHANNEL_CAPACITY);

    thread.join().unwrap();

    let mut count = 0u64;
    while let Ok(msg) = fire_rxs[0].try_recv() {
        if matches!(msg, ScheduledRequest::Fire(_)) {
            count += 1;
        }
    }

    // Poisson with mean 500 for 1 second → ~500 events, allow 20% tolerance
    let expected = 500u64;
    let lower = (expected as f64 * 0.80) as u64;
    let upper = (expected as f64 * 1.20) as u64;
    assert!(
        count >= lower && count <= upper,
        "expected ~{expected} Poisson events, got {count} (bounds: {lower}..{upper})"
    );
}
