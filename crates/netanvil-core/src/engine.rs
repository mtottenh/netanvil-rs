use std::time::Instant;

use netanvil_metrics::HdrMetricsCollector;
use netanvil_types::{
    RateConfig, RequestExecutor, RequestGenerator, RequestScheduler, RequestTransformer,
    SchedulerConfig, TestConfig,
};

use crate::controller::{PidRateController, StaticRateController, StepRateController};
use crate::coordinator::Coordinator;
use crate::generator::SimpleGenerator;
use crate::handle::WorkerHandle;
use crate::result::TestResult;
use crate::scheduler::{ConstantRateScheduler, PoissonScheduler};
use crate::transformer::{HeaderTransformer, NoopTransformer};
use crate::worker::Worker;

/// Run a load test with the given configuration and executor factory.
///
/// The executor factory is called once per core to create a per-core
/// executor instance. This allows each core to have its own connection
/// pool (the `!Send` shared-nothing design).
///
/// Scheduler, generator, and transformer are selected from `TestConfig`.
/// This function blocks until the test completes.
pub fn run_test<E, F>(config: TestConfig, executor_factory: F) -> netanvil_types::Result<TestResult>
where
    E: RequestExecutor + 'static,
    F: Fn() -> E + Send + 'static,
{
    run_test_inner(config, executor_factory, None)
}

/// Like `run_test`, but with a progress callback invoked each coordinator tick.
pub fn run_test_with_progress<E, F, P>(
    config: TestConfig,
    executor_factory: F,
    on_progress: P,
) -> netanvil_types::Result<TestResult>
where
    E: RequestExecutor + 'static,
    F: Fn() -> E + Send + 'static,
    P: FnMut(&crate::coordinator::ProgressUpdate) + 'static,
{
    // Reuse run_test's worker setup by factoring it out would be ideal,
    // but for now we duplicate minimally by calling run_test_inner.
    run_test_inner(config, executor_factory, Some(Box::new(on_progress)))
}

fn run_test_inner<E, F>(
    config: TestConfig,
    executor_factory: F,
    on_progress: Option<Box<dyn FnMut(&crate::coordinator::ProgressUpdate)>>,
) -> netanvil_types::Result<TestResult>
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
    let mut handles = Vec::with_capacity(num_cores);

    for core_id in 0..num_cores {
        let (cmd_tx, cmd_rx) = flume::unbounded();
        let (metrics_tx, metrics_rx) = flume::unbounded();

        let config = config.clone();
        let executor_factory = &executor_factory as *const F as usize;

        let thread = std::thread::Builder::new()
            .name(format!("netanvil-worker-{core_id}"))
            .spawn(move || {
                let core = core_affinity::CoreId { id: core_id };
                if !core_affinity::set_for_current(core) {
                    tracing::warn!(core_id, "failed to pin worker thread to core");
                }

                let executor_factory = unsafe { &*(executor_factory as *const F) };

                let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
                rt.block_on(async {
                    let initial_per_core = config.initial_rps() / num_cores as f64;

                    let scheduler: Box<dyn RequestScheduler> = match &config.scheduler {
                        SchedulerConfig::ConstantRate => Box::new(
                            ConstantRateScheduler::new(
                                initial_per_core,
                                start_time,
                                Some(config.duration),
                            ),
                        ),
                        SchedulerConfig::Poisson { seed } => match seed {
                            Some(s) => Box::new(PoissonScheduler::with_seed(
                                initial_per_core,
                                start_time,
                                Some(config.duration),
                                *s,
                            )),
                            None => Box::new(PoissonScheduler::new(
                                initial_per_core,
                                start_time,
                                Some(config.duration),
                            )),
                        },
                    };

                    let method: http::Method = config
                        .method
                        .parse()
                        .unwrap_or(http::Method::GET);
                    let generator: Box<dyn RequestGenerator> =
                        Box::new(SimpleGenerator::new(config.targets.clone(), method));

                    let transformer: Box<dyn RequestTransformer> = if config.headers.is_empty() {
                        Box::new(NoopTransformer)
                    } else {
                        Box::new(HeaderTransformer::new(config.headers.clone()))
                    };

                    let executor = executor_factory();
                    let collector = HdrMetricsCollector::new(config.error_status_threshold);

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

    let mut coordinator = Coordinator::new(
        rate_controller,
        handles,
        config.duration,
        config.control_interval,
    );

    if let Some(callback) = on_progress {
        coordinator.on_progress(callback);
    }

    Ok(coordinator.run())
}
