//! Dedicated timer thread for high-precision request scheduling.
//!
//! The timer thread owns all per-worker schedulers and dispatches fire events
//! to I/O workers via bounded channels. It runs as a plain `std::thread` with
//! no async runtime, allowing synchronous spin-loop precision (~1μs).
//!
//! See `docs/design/timer-thread.md` for the full design.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use netanvil_types::{RequestScheduler, ScheduledRequest, TimerCommand};

/// Shared counters for timer thread activity.
///
/// Uses `Arc<AtomicU64>` so the coordinator can read these without any
/// channel overhead — critical because under saturation (when these
/// counters matter most) channels may themselves be full.
#[derive(Debug, Clone)]
pub struct TimerStats {
    /// Total fire events successfully dispatched to workers.
    pub dispatched: Arc<AtomicU64>,
    /// Total fire events dropped due to backpressure (channel full).
    pub dropped: Arc<AtomicU64>,
}

impl Default for TimerStats {
    fn default() -> Self {
        Self::new()
    }
}

impl TimerStats {
    pub fn new() -> Self {
        Self {
            dispatched: Arc::new(AtomicU64::new(0)),
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// Handle for the coordinator to communicate with the timer thread.
pub struct TimerThreadHandle {
    pub command_tx: flume::Sender<TimerCommand>,
    pub thread: Option<std::thread::JoinHandle<()>>,
    /// Shared atomic counters — coordinator reads these each tick.
    pub stats: TimerStats,
}

/// Default capacity for bounded fire channels (timer → I/O worker).
///
/// At 100K RPS per worker this provides ~10ms of buffering, enough to
/// absorb I/O jitter without masking real backpressure.
pub const FIRE_CHANNEL_CAPACITY: usize = 1024;

/// Run the timer thread loop.
///
/// Owns all schedulers, uses a min-heap to select the next event across
/// all workers, dispatches `ScheduledRequest::Fire` via bounded channels.
///
/// This function blocks until all schedulers are exhausted or a `Stop`
/// command is received.
pub fn timer_loop(
    mut schedulers: Vec<Box<dyn RequestScheduler>>,
    fire_txs: Vec<flume::Sender<ScheduledRequest>>,
    cmd_rx: flume::Receiver<TimerCommand>,
    stats: TimerStats,
) {
    let num_workers = schedulers.len();
    let mut heap: BinaryHeap<Reverse<(Instant, usize)>> = BinaryHeap::new();

    tracing::info!(num_workers, "timer thread starting, seeding heap");

    // Seed the heap with the first event from each scheduler
    for (i, sched) in schedulers.iter_mut().enumerate() {
        if let Some(t) = sched.next_request_time() {
            heap.push(Reverse((t, i)));
            tracing::trace!(worker = i, "seeded heap with first event");
        } else {
            tracing::debug!(
                worker = i,
                "scheduler returned None on seed — already exhausted"
            );
        }
    }

    tracing::info!(heap_size = heap.len(), "heap seeded, entering main loop");

    loop {
        // Drain commands (non-blocking)
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                TimerCommand::UpdateRate(rps) => {
                    let per_worker = rps / num_workers as f64;
                    tracing::debug!(total_rps = rps, per_worker, "rate update received");
                    for s in &mut schedulers {
                        s.update_rate(per_worker);
                    }
                }
                TimerCommand::UpdateTargets(targets) => {
                    tracing::debug!(count = targets.len(), "forwarding target update to workers");
                    for ch in &fire_txs {
                        // Use blocking send for control messages — these are rare
                        // and must not be dropped. If the channel is full, this blocks
                        // briefly until the worker drains a message.
                        let _ = ch.send(ScheduledRequest::UpdateTargets(targets.clone()));
                    }
                }
                TimerCommand::UpdateMetadata(headers) => {
                    tracing::debug!(count = headers.len(), "forwarding header update to workers");
                    for ch in &fire_txs {
                        let _ = ch.send(ScheduledRequest::UpdateMetadata(headers.clone()));
                    }
                }
                TimerCommand::Stop => {
                    let total_dispatched = stats.dispatched.load(Ordering::Relaxed);
                    let total_dropped = stats.dropped.load(Ordering::Relaxed);
                    tracing::info!(
                        total_dispatched,
                        total_dropped,
                        "stop command received, shutting down"
                    );
                    // Use try_send for Stop to avoid deadlock if channel is full
                    // (e.g., backpressure scenario). Workers also exit when the
                    // sender is dropped, so dropping fire_txs guarantees shutdown.
                    for ch in &fire_txs {
                        let _ = ch.try_send(ScheduledRequest::Stop);
                    }
                    return;
                }
            }
        }

        // Get next event
        let Some(Reverse((intended_time, worker_id))) = heap.pop() else {
            let total_dispatched = stats.dispatched.load(Ordering::Relaxed);
            let total_dropped = stats.dropped.load(Ordering::Relaxed);
            tracing::info!(
                total_dispatched,
                total_dropped,
                "heap empty — all schedulers exhausted"
            );
            break;
        };

        // High-precision sleep
        precision_sleep_sync(intended_time);

        // Dispatch to worker via bounded channel
        match fire_txs[worker_id].try_send(ScheduledRequest::Fire(intended_time)) {
            Ok(()) => {
                let count = stats.dispatched.fetch_add(1, Ordering::Relaxed) + 1;
                if count % 1000 == 0 {
                    tracing::debug!(
                        total_dispatched = count,
                        total_dropped = stats.dropped.load(Ordering::Relaxed),
                        heap_size = heap.len(),
                        "dispatch progress"
                    );
                }
            }
            Err(flume::TrySendError::Full(_)) => {
                let dropped = stats.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                // Log first drop, then at power-of-two intervals to avoid
                // spamming the log under sustained backpressure.
                if dropped.is_power_of_two() {
                    tracing::warn!(
                        worker = worker_id,
                        total_dropped = dropped,
                        "backpressure: requests being dropped"
                    );
                }
            }
            Err(flume::TrySendError::Disconnected(_)) => {
                tracing::warn!(
                    worker = worker_id,
                    "worker channel disconnected, skipping scheduler"
                );
                continue;
            }
        }

        // Advance this worker's scheduler
        if let Some(next) = schedulers[worker_id].next_request_time() {
            heap.push(Reverse((next, worker_id)));
        } else {
            tracing::debug!(worker = worker_id, "scheduler exhausted");
        }
    }

    // Schedulers exhausted — tell all workers to stop.
    // Use try_send to avoid blocking if the channel is full (backpressure scenario).
    // Workers will also exit when the channel sender is dropped (recv returns Err).
    tracing::info!("sending stop to all workers");
    for ch in &fire_txs {
        let _ = ch.try_send(ScheduledRequest::Stop);
    }
}

/// Synchronous high-precision sleep.
///
/// Uses `std::thread::sleep` for the coarse portion (>50μs remaining),
/// then spin-loops for the final microseconds. This is safe because the
/// timer thread owns its core exclusively — no other tasks to starve.
///
/// Precision: ~1μs on Linux with a dedicated core.
pub fn precision_sleep_sync(target: Instant) {
    let now = Instant::now();
    if target <= now {
        return;
    }
    let remaining = target - now;

    // Use nanosleep for the coarse portion (>50μs remaining)
    if remaining > Duration::from_micros(50) {
        std::thread::sleep(remaining - Duration::from_micros(50));
    }

    // Spin for the final microseconds
    while Instant::now() < target {
        std::hint::spin_loop();
    }
}
