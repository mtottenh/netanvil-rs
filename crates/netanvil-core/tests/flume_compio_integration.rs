//! Validates that flume's async receive works correctly with compio's runtime.
//! This is a prerequisite for the N:M timer thread architecture.

use std::time::{Duration, Instant};

#[test]
fn flume_recv_async_wakes_compio_task() {
    // Verify: a compio task blocked on recv_async() wakes promptly
    // when another thread pushes to the channel.
    let (tx, rx) = flume::unbounded::<Instant>();

    // Push from a std::thread after 100ms
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        tx.send(Instant::now()).unwrap();
    });

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    let result = rt.block_on(async {
        let before = Instant::now();
        let sent_at = rx.recv_async().await.unwrap();
        let received_at = Instant::now();
        (before, sent_at, received_at)
    });

    let (before, sent_at, received_at) = result;
    let wait_time = received_at - before;
    let wake_latency = received_at - sent_at;

    // Should have waited ~100ms (the sender's sleep)
    assert!(
        wait_time >= Duration::from_millis(80) && wait_time < Duration::from_millis(300),
        "should have waited ~100ms, waited {:?}",
        wait_time
    );

    // Wake latency should be small (< 10ms)
    assert!(
        wake_latency < Duration::from_millis(10),
        "wake latency from send to recv should be small, was {:?}",
        wake_latency
    );
}

#[test]
fn flume_recv_async_returns_immediately_when_channel_has_data() {
    // Verify: when the channel already has messages, recv_async()
    // returns immediately without yielding.
    let (tx, rx) = flume::unbounded::<u64>();

    // Pre-fill channel
    for i in 0..100 {
        tx.send(i).unwrap();
    }

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    let (count, elapsed) = rt.block_on(async {
        let start = Instant::now();
        let mut count = 0u64;
        for _ in 0..100 {
            let _ = rx.recv_async().await.unwrap();
            count += 1;
        }
        (count, start.elapsed())
    });

    assert_eq!(count, 100);
    // Processing 100 pre-filled messages should be near-instant (< 10ms)
    assert!(
        elapsed < Duration::from_millis(10),
        "draining 100 pre-filled messages should be fast, took {:?}",
        elapsed
    );
}

#[test]
fn flume_recv_async_interleaves_with_compio_io() {
    // Verify: while a task is blocked on recv_async(), other compio tasks
    // (I/O operations) continue to make progress.

    // We test this by spawning a timer task alongside the channel receive.
    // If recv_async() blocks the runtime, the timer task won't fire.
    let (tx, rx) = flume::unbounded::<String>();

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    let result = rt.block_on(async {
        let timer_fired = std::rc::Rc::new(std::cell::Cell::new(false));
        let timer_flag = timer_fired.clone();

        // Spawn a timer task — should fire while we're waiting on recv_async
        compio::runtime::spawn(async move {
            compio::time::sleep(Duration::from_millis(50)).await;
            timer_flag.set(true);
        })
        .detach();

        // Push from another thread after 200ms
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            tx.send("hello".into()).unwrap();
        });

        // This should yield to runtime, allowing the timer task to fire
        let msg = rx.recv_async().await.unwrap();
        (msg, timer_fired.get())
    });

    assert_eq!(result.0, "hello");
    assert!(
        result.1,
        "timer task should have fired while waiting on recv_async"
    );
}

#[test]
fn flume_bounded_channel_provides_backpressure() {
    // Verify: bounded channel with try_send detects when the worker
    // can't keep up (the basis for CO detection in the timer thread).
    let (tx, rx) = flume::bounded::<u64>(16);

    // Fill the channel
    for i in 0..16 {
        assert!(tx.try_send(i).is_ok());
    }

    // Next send should fail (full)
    assert!(tx.try_send(99).is_err(), "bounded channel should reject when full");

    // Drain one
    let _ = rx.try_recv().unwrap();

    // Now one more should succeed
    assert!(tx.try_send(100).is_ok());
}

#[test]
fn cross_thread_send_throughput() {
    // Measure: how fast can a std::thread push to a flume channel
    // that a compio task consumes? This bounds the timer thread's
    // dispatch rate.
    let (tx, rx) = flume::bounded::<Instant>(1024);
    let n = 50_000u64;

    let sender = std::thread::spawn(move || {
        let start = Instant::now();
        for _ in 0..n {
            let _ = tx.send(Instant::now());
        }
        start.elapsed()
    });

    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
    let receive_elapsed = rt.block_on(async {
        let start = Instant::now();
        let mut count = 0u64;
        while count < n {
            let _ = rx.recv_async().await;
            count += 1;
        }
        start.elapsed()
    });

    let send_elapsed = sender.join().unwrap();
    let throughput = n as f64 / receive_elapsed.as_secs_f64();

    eprintln!(
        "Cross-thread channel: {n} messages in {receive_elapsed:?} ({throughput:.0} msg/sec), sender took {send_elapsed:?}"
    );

    // Should handle at least 100K msg/sec (we need ~50K for high-rate testing)
    assert!(
        throughput > 100_000.0,
        "channel throughput too low: {throughput:.0} msg/sec"
    );
}
