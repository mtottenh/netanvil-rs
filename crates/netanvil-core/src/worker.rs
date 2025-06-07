use std::rc::Rc;
use std::time::Instant;

use netanvil_types::{
    MetricsCollector, MetricsSnapshot, RequestContext, RequestExecutor, RequestGenerator,
    RequestScheduler, RequestTransformer, WorkerCommand,
};

use crate::precision::precision_sleep_until;

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
    metrics_interval: std::time::Duration,
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
        metrics_interval: std::time::Duration,
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

        loop {
            // 1. Check for coordinator commands (non-blocking)
            match self.command_rx.try_recv() {
                Ok(WorkerCommand::UpdateRate(rps)) => {
                    self.scheduler.update_rate(rps);
                }
                Ok(WorkerCommand::Stop) => break,
                Err(_) => {}
            }

            // 2. Get next scheduled time
            let Some(intended_time) = self.scheduler.next_request_time() else {
                break; // Test duration complete
            };

            // 3. Precision sleep until intended send time
            precision_sleep_until(intended_time).await;

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
