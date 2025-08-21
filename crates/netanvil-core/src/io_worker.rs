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
    MetricsCollector, MetricsSnapshot, RequestContext, RequestExecutor, RequestGenerator,
    RequestTransformer, ScheduledRequest,
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

    tracing::info!(core_id, "io worker starting, waiting for fire events");

    loop {
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
            if !handle_message(
                msg,
                &mut generator,
                &transformer,
                &executor,
                &metrics,
                &mut request_seq,
                core_id,
            ) {
                tracing::info!(
                    core_id,
                    request_seq,
                    drained,
                    "received stop during drain, exiting"
                );
                // Send final snapshot before exiting
                let snapshot = metrics.snapshot();
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

        // Periodic metrics reporting
        if last_report.elapsed() >= metrics_interval {
            let snapshot = metrics.snapshot();
            tracing::debug!(
                core_id,
                total_requests = snapshot.total_requests,
                "sending periodic metrics snapshot"
            );
            let _ = metrics_tx.try_send(snapshot);
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

    // Send final snapshot
    let snapshot = metrics.snapshot();
    tracing::info!(
        core_id,
        request_seq,
        final_requests = snapshot.total_requests,
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
            compio::runtime::spawn(async move {
                let result = exec.execute(&spec, &context).await;
                met.record(&result);
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
