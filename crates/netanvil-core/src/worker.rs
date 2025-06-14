use std::rc::Rc;
use std::time::{Duration, Instant};

use netanvil_types::{
    MetricsCollector, MetricsSnapshot, RequestContext, RequestExecutor, RequestGenerator,
    RequestScheduler, RequestTransformer, WorkerCommand,
};

use crate::precision::precision_sleep_until;

/// Maximum requests to fire in a burst before yielding to the runtime.
/// This prevents starvation of I/O tasks when catching up after a delay.
const YIELD_AFTER_BURST: u32 = 64;

/// Per-core worker that runs the request scheduling loop.
///
/// Fully monomorphized — all component types are known at compile time,
/// giving zero virtual dispatch on the hot path.
///
/// The worker owns the scheduler and generator (exclusive `&mut self` access).
/// The transformer, executor, and metrics collector are `Rc`-shared with
/// spawned tasks on the same core.
pub struct Worker<S, G, T, E, M>
where
    S: RequestScheduler,
    G: RequestGenerator,
    T: RequestTransformer + 'static,
    E: RequestExecutor + 'static,
    M: MetricsCollector + 'static,
{
    scheduler: S,
    generator: G,
    transformer: Rc<T>,
    executor: Rc<E>,
    metrics: Rc<M>,

    command_rx: flume::Receiver<WorkerCommand>,
    metrics_tx: flume::Sender<MetricsSnapshot>,

    core_id: usize,
    metrics_interval: Duration,
}

impl<S, G, T, E, M> Worker<S, G, T, E, M>
where
    S: RequestScheduler,
    G: RequestGenerator,
    T: RequestTransformer + 'static,
    E: RequestExecutor + 'static,
    M: MetricsCollector + 'static,
{
    pub fn new(
        scheduler: S,
        generator: G,
        transformer: T,
        executor: E,
        metrics: M,
        command_rx: flume::Receiver<WorkerCommand>,
        metrics_tx: flume::Sender<MetricsSnapshot>,
        core_id: usize,
        metrics_interval: Duration,
    ) -> Self {
        Self {
            scheduler,
            generator,
            transformer: Rc::new(transformer),
            executor: Rc::new(executor),
            metrics: Rc::new(metrics),
            command_rx,
            metrics_tx,
            core_id,
            metrics_interval,
        }
    }

    /// Run the scheduling loop until the test completes or a Stop command
    /// is received.
    pub async fn run(mut self) {
        let mut request_seq: u64 = 0;
        let mut last_report = Instant::now();
        let mut consecutive_immediate: u32 = 0;

        loop {
            // 1. Check for coordinator commands (non-blocking)
            match self.command_rx.try_recv() {
                Ok(WorkerCommand::UpdateRate(rps)) => {
                    self.scheduler.update_rate(rps);
                }
                Ok(WorkerCommand::UpdateTargets(targets)) => {
                    self.generator.update_targets(targets);
                }
                Ok(WorkerCommand::UpdateHeaders(headers)) => {
                    self.transformer.update_headers(headers);
                }
                Ok(WorkerCommand::Stop) => break,
                Err(_) => {}
            }

            // 2. Get next scheduled time
            let Some(intended_time) = self.scheduler.next_request_time() else {
                break; // Test duration complete
            };

            // 3. Sleep until intended send time, or yield if catching up
            let now = Instant::now();
            if intended_time > now {
                // Future request — sleep via compio timer (yields to runtime)
                precision_sleep_until(intended_time).await;
                consecutive_immediate = 0;
            } else {
                // Request is due now (or overdue) — fire immediately
                consecutive_immediate += 1;
                if consecutive_immediate >= YIELD_AFTER_BURST {
                    // We've fired a burst without yielding — give the
                    // runtime a chance to process I/O completions
                    compio::time::sleep(Duration::from_micros(100)).await;
                    consecutive_immediate = 0;
                }
            }

            // 4. Build context capturing both intended and actual time
            let context = RequestContext {
                request_id: self.core_id as u64 * 1_000_000_000 + request_seq,
                intended_time,
                actual_time: Instant::now(),
                core_id: self.core_id,
                is_sampled: false,
                session_id: None,
            };
            request_seq += 1;

            // 5. Generate and transform (synchronous, on the scheduling thread)
            let spec = self.generator.generate(&context);
            let spec = self.transformer.transform(spec, &context);

            // 6. Spawn execution as a concurrent task on this core
            let executor = self.executor.clone();
            let metrics = self.metrics.clone();
            compio::runtime::spawn(async move {
                let result = executor.execute(&spec, &context).await;
                metrics.record(&result);
            })
            .detach();

            // 7. Periodic metrics reporting
            if last_report.elapsed() >= self.metrics_interval {
                let snapshot = self.metrics.snapshot();
                let _ = self.metrics_tx.try_send(snapshot);
                last_report = Instant::now();
            }
        }

        // Send final snapshot
        let snapshot = self.metrics.snapshot();
        let _ = self.metrics_tx.try_send(snapshot);
    }
}
