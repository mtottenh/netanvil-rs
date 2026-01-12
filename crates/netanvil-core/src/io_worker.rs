//! I/O worker loop for the N:M timer thread architecture.
//!
//! Each I/O worker runs on its own compio runtime thread. It receives
//! `ScheduledRequest` messages from the timer thread via a bounded channel
//! and executes HTTP requests. No scheduling logic — pure I/O execution.
//!
//! See `docs/design/timer-thread.md` for the full design.

use std::rc::Rc;
use std::time::{Duration, Instant};

use netanvil_types::{
    EventRecorder, ExecutionResult, MetricsCollector, MetricsSnapshot, RequestContext,
    RequestExecutor, RequestGenerator, RequestTransformer, ScheduledRequest,
};

/// Infrastructure channels and config for an I/O worker.
pub struct IoWorkerConfig {
    pub fire_rx: flume::Receiver<ScheduledRequest>,
    pub metrics_tx: flume::Sender<MetricsSnapshot>,
    pub core_id: usize,
    pub metrics_interval: Duration,
    /// Whether to yield for in-flight requests on shutdown.
    pub graceful_shutdown: bool,
}

/// Run the I/O worker loop.
///
/// Receives fire events from the timer thread, generates/transforms/executes
/// requests, and reports metrics. The worker has no scheduler — timing is
/// entirely managed by the timer thread.
///
/// While blocked on `recv_async()`, the compio runtime processes io_uring
/// completions (TCP connects, HTTP responses, etc.). After receiving a
/// message, the worker drains any accumulated messages, yielding to the
/// runtime every ~50μs to ensure I/O completions are processed.
pub async fn io_worker_loop<G, T, E, M>(
    config: IoWorkerConfig,
    mut generator: G,
    transformer: Rc<T>,
    executor: Rc<E>,
    metrics: Rc<M>,
    event_recorder: Rc<dyn EventRecorder>,
    in_flight_limit: Rc<crate::in_flight::InFlightLimit>,
) where
    G: RequestGenerator,
    T: RequestTransformer<Spec = G::Spec> + 'static,
    E: RequestExecutor<Spec = G::Spec> + 'static,
    M: MetricsCollector + 'static,
{
    let IoWorkerConfig {
        fire_rx,
        metrics_tx,
        core_id,
        metrics_interval,
        graceful_shutdown,
    } = config;
    let mut request_seq: u64 = 0;
    let mut last_report = Instant::now();
    let in_flight_drops = std::sync::atomic::AtomicU64::new(0);
    let mut last_in_flight_drops: u64 = 0; // for per-window delta

    // Create response feedback channel only if the generator opts in.
    // When disabled (default), zero overhead — no channel, no result cloning.
    // Bounded to 1024 entries; try_send drops on backpressure to avoid
    // blocking the executor runtime or accumulating unbounded memory.
    let (response_tx, response_rx) = if generator.wants_responses() {
        let (tx, rx) = flume::bounded(1024);
        tracing::info!(
            core_id,
            "generator requested response callbacks, channel created"
        );
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    tracing::info!(core_id, "io worker starting, waiting for fire events");

    loop {
        // Deliver completed responses to generator before next generate() call.
        if let Some(ref rx) = response_rx {
            while let Ok(result) = rx.try_recv() {
                generator.on_response(&result);
            }
        }

        // Wait for next message from timer thread.
        // While suspended: compio processes io_uring completions.
        // Wakes immediately when timer thread pushes a message.
        let msg = match fire_rx.recv_async().await {
            Ok(msg) => msg,
            Err(_) => {
                tracing::info!(core_id, request_seq, "fire channel disconnected, exiting");
                break;
            }
        };

        tracing::trace!(core_id, ?msg, "received message from timer thread");

        if !handle_message(
            msg,
            &mut generator,
            &transformer,
            &executor,
            &metrics,
            &event_recorder,
            &response_tx,
            &in_flight_limit,
            &in_flight_drops,
            &mut request_seq,
            core_id,
        ) {
            tracing::info!(core_id, request_seq, "received stop, exiting main loop");
            break;
        }

        // Drain any messages that accumulated while we were yielding.
        // Yield to the runtime every ~50μs of wall-clock time so
        // io_uring completions are processed at a regular cadence.
        let mut drained = 0u64;
        let mut last_yield = Instant::now();
        while let Ok(msg) = fire_rx.try_recv() {
            drained += 1;

            // Deliver any responses that completed during the burst.
            if let Some(ref rx) = response_rx {
                while let Ok(result) = rx.try_recv() {
                    generator.on_response(&result);
                }
            }

            if !handle_message(
                msg,
                &mut generator,
                &transformer,
                &executor,
                &metrics,
                &event_recorder,
                &response_tx,
                &in_flight_limit,
                &in_flight_drops,
                &mut request_seq,
                core_id,
            ) {
                tracing::info!(
                    core_id,
                    request_seq,
                    drained,
                    "received stop during drain, exiting"
                );
                // Flush events and send final snapshot before exiting
                event_recorder.flush();
                let mut snapshot = metrics.snapshot();
                let current_drops = in_flight_drops.load(std::sync::atomic::Ordering::Relaxed);
                snapshot.in_flight_drops = current_drops - last_in_flight_drops;
                snapshot.in_flight_count = in_flight_limit.in_flight() as u64;
                snapshot.in_flight_capacity = in_flight_limit.capacity() as u64;
                let _ = metrics_tx.try_send(snapshot);
                return;
            }

            if last_yield.elapsed() > Duration::from_micros(50) {
                compio::time::sleep(Duration::ZERO).await;
                last_yield = Instant::now();
            }
        }

        if drained > 0 {
            tracing::trace!(core_id, drained, request_seq, "drained burst");
        }

        // Periodic metrics reporting + event flush
        if last_report.elapsed() >= metrics_interval {
            let mut snapshot = metrics.snapshot();
            // Stamp in-flight data onto snapshot (per-window delta for drops)
            let current_drops = in_flight_drops.load(std::sync::atomic::Ordering::Relaxed);
            snapshot.in_flight_drops = current_drops - last_in_flight_drops;
            last_in_flight_drops = current_drops;
            snapshot.in_flight_count = in_flight_limit.in_flight() as u64;
            snapshot.in_flight_capacity = in_flight_limit.capacity() as u64;
            tracing::debug!(
                core_id,
                total_requests = snapshot.total_requests,
                in_flight = snapshot.in_flight_count,
                in_flight_drops = snapshot.in_flight_drops,
                "sending periodic metrics snapshot"
            );
            let _ = metrics_tx.try_send(snapshot);
            event_recorder.flush();
            last_report = Instant::now();
        }
    }

    // Give spawned tasks a chance to complete before taking the final snapshot.
    // Without this yield, the metrics won't include requests still in-flight.
    // Skip this when graceful_shutdown is false (-nowait mode).
    if graceful_shutdown {
        tracing::debug!(core_id, "yielding to let spawned tasks complete");
        compio::time::sleep(Duration::from_millis(50)).await;
    }

    // Final drain of responses from in-flight tasks that completed during shutdown.
    if let Some(ref rx) = response_rx {
        while let Ok(result) = rx.try_recv() {
            generator.on_response(&result);
        }
    }

    // Flush remaining event records
    event_recorder.flush();

    // Send final snapshot
    let mut snapshot = metrics.snapshot();
    let current_drops = in_flight_drops.load(std::sync::atomic::Ordering::Relaxed);
    snapshot.in_flight_drops = current_drops - last_in_flight_drops;
    snapshot.in_flight_count = in_flight_limit.in_flight() as u64;
    snapshot.in_flight_capacity = in_flight_limit.capacity() as u64;
    tracing::info!(
        core_id,
        request_seq,
        final_requests = snapshot.total_requests,
        total_in_flight_drops = current_drops,
        "sending final snapshot and exiting"
    );
    let _ = metrics_tx.try_send(snapshot);
}

/// Handle a single ScheduledRequest message.
///
/// Returns `true` to continue, `false` to stop the worker.
fn handle_message<G, T, E, M>(
    msg: ScheduledRequest,
    generator: &mut G,
    transformer: &Rc<T>,
    executor: &Rc<E>,
    metrics: &Rc<M>,
    event_recorder: &Rc<dyn EventRecorder>,
    response_tx: &Option<flume::Sender<ExecutionResult>>,
    in_flight_limit: &Rc<crate::in_flight::InFlightLimit>,
    in_flight_drops: &std::sync::atomic::AtomicU64,
    seq: &mut u64,
    core_id: usize,
) -> bool
where
    G: RequestGenerator,
    T: RequestTransformer<Spec = G::Spec> + 'static,
    E: RequestExecutor<Spec = G::Spec> + 'static,
    M: MetricsCollector + 'static,
{
    match msg {
        ScheduledRequest::Fire(intended_time) => {
            // Check in-flight limit before spawning. If at capacity, count
            // as an in_flight_drop (distinct from fire_channel_drops).
            let permit = match in_flight_limit.try_acquire() {
                Some(p) => p,
                None => {
                    in_flight_drops.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return true; // continue processing, just skip this request
                }
            };

            let context = RequestContext {
                request_id: core_id as u64 * 1_000_000_000 + *seq,
                intended_time,
                actual_time: Instant::now(),
                core_id,
                is_sampled: false,
                session_id: None,
            };
            *seq += 1;

            let spec = generator.generate(&context);
            let spec = transformer.transform(spec, &context);

            let exec = executor.clone();
            let met = metrics.clone();
            let evt = event_recorder.clone();
            let resp_tx = response_tx.clone();
            compio::runtime::spawn(async move {
                let result = exec.execute(&spec, &context).await;
                met.record(&result);
                evt.record(&result);
                if let Some(tx) = resp_tx {
                    let _ = tx.try_send(result); // drop on backpressure
                }
                drop(permit); // release in-flight slot
            })
            .detach();

            true
        }
        ScheduledRequest::UpdateTargets(targets) => {
            generator.update_targets(targets);
            true
        }
        ScheduledRequest::UpdateMetadata(headers) => {
            transformer.update_metadata(headers);
            true
        }
        ScheduledRequest::Stop => false,
    }
}
