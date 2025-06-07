use std::time::Instant;

use netanvil_metrics::HdrMetricsCollector;
use netanvil_types::{RateConfig, RequestExecutor, TestConfig};

use crate::controller::{PidRateController, StaticRateController, StepRateController};
use crate::coordinator::Coordinator;
use crate::generator::SimpleGenerator;
use crate::handle::WorkerHandle;
use crate::result::TestResult;
use crate::scheduler::ConstantRateScheduler;
use crate::transformer::NoopTransformer;
use crate::worker::Worker;

/// Run a load test with the given configuration and executor factory.
///
/// The executor factory is called once per core to create a per-core
/// executor instance. This allows each core to have its own connection
/// pool (the `!Send` shared-nothing design).
///
/// This function blocks until the test completes.
pub fn run_test<E, F>(config: TestConfig, executor_factory: F) -> netanvil_types::Result<TestResult>
where
    E: RequestExecutor + 'static,
    F: Fn() -> E + Send + 'static,
{
    let num_cores = if config.num_cores == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    } else {
        config.num_cores
    };

    let start_time = Instant::now();

    // Create worker handles (channels + threads)
    let mut handles = Vec::with_capacity(num_cores);

    for core_id in 0..num_cores {
        let (cmd_tx, cmd_rx) = flume::unbounded();
        let (metrics_tx, metrics_rx) = flume::unbounded();

        let config = config.clone();
        let executor_factory = &executor_factory as *const F as usize;

        // SAFETY: The executor_factory reference is valid for the lifetime of
        // this function, and we join all threads before returning. We pass it
        // as a raw pointer to avoid requiring F: Clone.
        let thread = std::thread::Builder::new()
            .name(format!("netanvil-worker-{core_id}"))
            .spawn(move || {
                let executor_factory =
                    unsafe { &*(executor_factory as *const F) };

                let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
                rt.block_on(async {
                    let initial_per_core = config.initial_rps() / num_cores as f64;
                    let scheduler = ConstantRateScheduler::new(
                        initial_per_core,
                        start_time,
                        Some(config.duration),
                    );
                    let generator = SimpleGenerator::get(config.targets.clone());
                    let transformer = NoopTransformer;
                    let executor = executor_factory();
                    let collector = HdrMetricsCollector::new();

                    let worker = Worker::new(
                        scheduler,
                        generator,
                        transformer,
                        executor,
                        collector,
                        cmd_rx,
                        metrics_tx,
                        core_id,
                        config.metrics_interval,
                    );

                    worker.run().await;
                });
            })
            .map_err(|e| netanvil_types::NetAnvilError::Other(format!("spawn worker: {e}")))?;

        handles.push(WorkerHandle {
            command_tx: cmd_tx,
            metrics_rx,
            thread: Some(thread),
            core_id,
        });
    }

    // Build rate controller
    let rate_controller: Box<dyn netanvil_types::RateController> = match &config.rate {
        RateConfig::Static { rps } => Box::new(StaticRateController::new(*rps)),
        RateConfig::Step { steps } => {
            Box::new(StepRateController::with_start_time(steps.clone(), start_time))
        }
        RateConfig::Pid { initial_rps, target } => Box::new(PidRateController::new(
            target.metric,
            target.value,
            *initial_rps,
            target.min_rps,
            target.max_rps,
            target.kp,
            target.ki,
            target.kd,
        )),
    };

    // Run coordinator on this thread (blocking)
    let mut coordinator = Coordinator::new(
        rate_controller,
        handles,
        config.duration,
        config.control_interval,
    );

    Ok(coordinator.run())
}
