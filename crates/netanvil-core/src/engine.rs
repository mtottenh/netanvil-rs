use std::rc::Rc;
use std::time::Instant;

use netanvil_metrics::HdrMetricsCollector;
use netanvil_types::{
    ConnectionPolicy, RateConfig, RequestExecutor, RequestGenerator, RequestScheduler,
    RequestTransformer, SchedulerConfig, TestConfig,
};

use crate::controller::{PidRateController, StaticRateController, StepRateController};
use crate::coordinator::Coordinator;
use crate::handle::IoWorkerHandle;
use crate::io_worker::io_worker_loop;
use crate::result::TestResult;
use crate::scheduler::{ConstantRateScheduler, PoissonScheduler};
use crate::timer_thread::{self, TimerThreadHandle, FIRE_CHANNEL_CAPACITY};
use crate::transformer::{ConnectionPolicyTransformer, HeaderTransformer, NoopTransformer};
use crate::generator::SimpleGenerator;

// ---------------------------------------------------------------------------
// Convenience entry points (use defaults from TestConfig for everything)
// ---------------------------------------------------------------------------

/// Run a load test with the given configuration and executor factory.
///
/// Uses `SimpleGenerator` (round-robin URLs) and `HeaderTransformer` from config.
/// For custom generators or transformers, use [`TestBuilder`].
pub fn run_test<E, F>(config: TestConfig, executor_factory: F) -> netanvil_types::Result<TestResult>
where
    E: RequestExecutor + 'static,
    F: Fn() -> E + Send + 'static,
{
    TestBuilder::new(config, executor_factory).run()
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
    TestBuilder::new(config, executor_factory)
        .on_progress(on_progress)
        .run()
}

/// Full-featured entry point: progress callback + external command channel.
///
/// The `external_command_rx` allows injecting `WorkerCommand`s into the
/// coordinator from outside (HTTP API, distributed leader, etc.).
pub fn run_test_with_api<E, F, P>(
    config: TestConfig,
    executor_factory: F,
    on_progress: P,
    external_command_rx: flume::Receiver<netanvil_types::WorkerCommand>,
) -> netanvil_types::Result<TestResult>
where
    E: RequestExecutor + 'static,
    F: Fn() -> E + Send + 'static,
    P: FnMut(&crate::coordinator::ProgressUpdate) + 'static,
{
    TestBuilder::new(config, executor_factory)
        .on_progress(on_progress)
        .external_commands(external_command_rx)
        .run()
}

// ---------------------------------------------------------------------------
// TestBuilder: full control over all components
// ---------------------------------------------------------------------------

/// Builder for configuring and running a load test with custom components.
///
/// Provides full control over the request generation pipeline. Each factory
/// is called once per I/O worker core to create a core-local instance
/// (the shared-nothing, thread-per-core design).
///
/// # Quick start
///
/// ```ignore
/// use netanvil_core::TestBuilder;
/// use netanvil_http::HttpExecutor;
/// use netanvil_types::TestConfig;
/// use std::time::Duration;
///
/// let result = TestBuilder::new(TestConfig::default())
///     .executor_factory(|| HttpExecutor::with_timeout(Duration::from_secs(30)))
///     .run()
///     .unwrap();
/// ```
///
/// # Custom generator
///
/// ```ignore
/// use netanvil_core::TestBuilder;
/// use netanvil_http::HttpExecutor;
/// use netanvil_types::*;
///
/// struct CacheBustGenerator { base_url: String, counter: u64 }
///
/// impl RequestGenerator for CacheBustGenerator {
///     fn generate(&mut self, _ctx: &RequestContext) -> RequestSpec {
///         self.counter += 1;
///         RequestSpec {
///             method: http::Method::GET,
///             url: format!("{}?_cb={}", self.base_url, self.counter),
///             headers: vec![],
///             body: None,
///         }
///     }
/// }
///
/// let config = TestConfig::default();
/// let url = config.targets[0].clone();
/// let result = TestBuilder::new(config)
///     .generator_factory(move |core_id| {
///         Box::new(CacheBustGenerator {
///             base_url: url.clone(),
///             counter: core_id as u64 * 1_000_000,
///         }) as Box<dyn RequestGenerator>
///     })
///     .executor_factory(|| HttpExecutor::with_timeout(std::time::Duration::from_secs(30)))
///     .run()
///     .unwrap();
/// ```
pub struct TestBuilder<E, F>
where
    E: RequestExecutor + 'static,
    F: Fn() -> E + Send + 'static,
{
    config: TestConfig,
    executor_factory: F,
    generator_factory: Option<Box<dyn Fn(usize) -> Box<dyn RequestGenerator> + Send>>,
    transformer_factory: Option<Box<dyn Fn(usize) -> Box<dyn RequestTransformer> + Send>>,
    on_progress: Option<Box<dyn FnMut(&crate::coordinator::ProgressUpdate)>>,
    external_command_rx: Option<flume::Receiver<netanvil_types::WorkerCommand>>,
}

impl<E, F> TestBuilder<E, F>
where
    E: RequestExecutor + 'static,
    F: Fn() -> E + Send + 'static,
{
    /// Create a new test builder with the given configuration and executor factory.
    pub fn new(config: TestConfig, executor_factory: F) -> Self {
        Self {
            config,
            executor_factory,
            generator_factory: None,
            transformer_factory: None,
            on_progress: None,
            external_command_rx: None,
        }
    }

    /// Set a custom generator factory. Called once per I/O worker core.
    ///
    /// The closure receives the `core_id` (0-based) so generators can
    /// partition their state space (e.g., different URL ranges per core,
    /// disjoint user ID pools, etc.).
    ///
    /// If not set, uses `SimpleGenerator` from the config's `targets` and `method`.
    pub fn generator_factory(mut self, factory: impl Fn(usize) -> Box<dyn RequestGenerator> + Send + 'static) -> Self {
        self.generator_factory = Some(Box::new(factory));
        self
    }

    /// Set a custom transformer factory. Called once per I/O worker core.
    ///
    /// If not set, uses `HeaderTransformer` from config's `headers`,
    /// wrapped with `ConnectionPolicyTransformer` if the connection policy
    /// is not `KeepAlive`.
    pub fn transformer_factory(mut self, factory: impl Fn(usize) -> Box<dyn RequestTransformer> + Send + 'static) -> Self {
        self.transformer_factory = Some(Box::new(factory));
        self
    }

    /// Set a progress callback invoked each coordinator tick (~10-100Hz).
    pub fn on_progress(mut self, callback: impl FnMut(&crate::coordinator::ProgressUpdate) + 'static) -> Self {
        self.on_progress = Some(Box::new(callback));
        self
    }

    /// Set an external command channel for mid-test control (HTTP API, etc.).
    pub fn external_commands(mut self, rx: flume::Receiver<netanvil_types::WorkerCommand>) -> Self {
        self.external_command_rx = Some(rx);
        self
    }

    /// Build and run the test. Blocks until completion.
    pub fn run(self) -> netanvil_types::Result<TestResult> {
        run_test_impl(
            self.config,
            self.executor_factory,
            self.generator_factory,
            self.transformer_factory,
            self.on_progress,
            self.external_command_rx,
        )
    }
}

// ---------------------------------------------------------------------------
// Internal implementation
// ---------------------------------------------------------------------------

fn run_test_impl<E, F>(
    config: TestConfig,
    executor_factory: F,
    generator_factory: Option<Box<dyn Fn(usize) -> Box<dyn RequestGenerator> + Send>>,
    transformer_factory: Option<Box<dyn Fn(usize) -> Box<dyn RequestTransformer> + Send>>,
    on_progress: Option<Box<dyn FnMut(&crate::coordinator::ProgressUpdate)>>,
    external_command_rx: Option<flume::Receiver<netanvil_types::WorkerCommand>>,
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

    // ── Create N schedulers (one per I/O worker) ──
    let initial_per_core = config.initial_rps() / num_cores as f64;
    let mut schedulers: Vec<Box<dyn RequestScheduler>> = Vec::with_capacity(num_cores);
    for _ in 0..num_cores {
        let scheduler: Box<dyn RequestScheduler> = match &config.scheduler {
            SchedulerConfig::ConstantRate => Box::new(ConstantRateScheduler::new(
                initial_per_core,
                start_time,
                Some(config.duration),
            )),
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
        schedulers.push(scheduler);
    }

    // ── Create N bounded fire channels (timer → I/O worker) ──
    let mut fire_txs = Vec::with_capacity(num_cores);
    let mut fire_rxs = Vec::with_capacity(num_cores);
    for _ in 0..num_cores {
        let (tx, rx) = flume::bounded(FIRE_CHANNEL_CAPACITY);
        fire_txs.push(tx);
        fire_rxs.push(rx);
    }

    // ── Create timer command channel (coordinator → timer) ──
    let (timer_cmd_tx, timer_cmd_rx) = flume::unbounded();

    // ── Spawn N I/O worker threads ──
    let mut io_handles = Vec::with_capacity(num_cores);

    // SAFETY: We pass factories as raw pointers to avoid requiring Clone.
    // The pointers are valid for the lifetime of all worker threads because
    // we join them before returning.
    let executor_factory_ptr = &executor_factory as *const F as usize;
    let generator_factory_ptr = &generator_factory
        as *const Option<Box<dyn Fn(usize) -> Box<dyn RequestGenerator> + Send>>
        as usize;
    let transformer_factory_ptr = &transformer_factory
        as *const Option<Box<dyn Fn(usize) -> Box<dyn RequestTransformer> + Send>>
        as usize;

    for core_id in 0..num_cores {
        let (metrics_tx, metrics_rx) = flume::unbounded();
        let fire_rx = fire_rxs.remove(0);
        let config = config.clone();

        let thread = std::thread::Builder::new()
            .name(format!("netanvil-io-{core_id}"))
            .spawn(move || {
                // Pin to core (offset by 1 to leave core 0 for timer thread)
                let pin_core = core_affinity::CoreId { id: core_id + 1 };
                if !core_affinity::set_for_current(pin_core) {
                    tracing::warn!(core_id, "failed to pin I/O worker to core {}", core_id + 1);
                }

                // SAFETY: pointers valid because parent joins all threads before returning.
                let executor_factory = unsafe { &*(executor_factory_ptr as *const F) };
                let generator_factory = unsafe {
                    &*(generator_factory_ptr
                        as *const Option<Box<dyn Fn(usize) -> Box<dyn RequestGenerator> + Send>>)
                };
                let transformer_factory = unsafe {
                    &*(transformer_factory_ptr
                        as *const Option<Box<dyn Fn(usize) -> Box<dyn RequestTransformer> + Send>>)
                };

                let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
                rt.block_on(async {
                    // Generator: user-supplied or default SimpleGenerator
                    let generator: Box<dyn RequestGenerator> = match generator_factory {
                        Some(factory) => factory(core_id),
                        None => {
                            let method: http::Method =
                                config.method.parse().unwrap_or(http::Method::GET);
                            Box::new(SimpleGenerator::new(config.targets.clone(), method))
                        }
                    };

                    // Transformer: user-supplied or default from config
                    let transformer: Box<dyn RequestTransformer> = match transformer_factory {
                        Some(factory) => factory(core_id),
                        None => {
                            let base: Box<dyn RequestTransformer> = if config.headers.is_empty() {
                                Box::new(NoopTransformer)
                            } else {
                                Box::new(HeaderTransformer::new(config.headers.clone()))
                            };

                            match &config.connections.connection_policy {
                                ConnectionPolicy::KeepAlive => base,
                                policy => Box::new(ConnectionPolicyTransformer::new(
                                    base,
                                    policy.clone(),
                                )),
                            }
                        }
                    };

                    let executor = executor_factory();
                    let collector = HdrMetricsCollector::new(config.error_status_threshold);

                    io_worker_loop(
                        fire_rx,
                        generator,
                        Rc::new(transformer),
                        Rc::new(executor),
                        Rc::new(collector),
                        metrics_tx,
                        core_id,
                        config.metrics_interval,
                    )
                    .await;
                });
            })
            .map_err(|e| netanvil_types::NetAnvilError::Other(format!("spawn io worker: {e}")))?;

        io_handles.push(IoWorkerHandle {
            metrics_rx,
            thread: Some(thread),
            core_id,
        });
    }

    // ── Spawn timer thread ──
    let timer_thread = std::thread::Builder::new()
        .name("netanvil-timer".into())
        .spawn(move || {
            let core = core_affinity::CoreId { id: 0 };
            if !core_affinity::set_for_current(core) {
                tracing::warn!("failed to pin timer thread to core 0");
            }

            timer_thread::timer_loop(schedulers, fire_txs, timer_cmd_rx);
        })
        .map_err(|e| netanvil_types::NetAnvilError::Other(format!("spawn timer thread: {e}")))?;

    let timer_handle = TimerThreadHandle {
        command_tx: timer_cmd_tx,
        thread: Some(timer_thread),
    };

    // ── Create coordinator ──
    let rate_controller: Box<dyn netanvil_types::RateController> = match &config.rate {
        RateConfig::Static { rps } => Box::new(StaticRateController::new(*rps)),
        RateConfig::Step { steps } => {
            Box::new(StepRateController::with_start_time(steps.clone(), start_time))
        }
        RateConfig::Pid { initial_rps, target } => Box::new(PidRateController::new(
            target.metric.clone(),
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
        io_handles,
        timer_handle,
        config.duration,
        config.control_interval,
    );

    if let Some(callback) = on_progress {
        coordinator.on_progress(callback);
    }

    if let Some(rx) = external_command_rx {
        coordinator.set_external_commands(rx);
    }

    // Wire external signal source from config (e.g. server load metric)
    if let Some(source) = crate::signal::make_signal_source(
        config.external_metrics_url.as_deref(),
        config.external_metrics_field.as_deref(),
    ) {
        coordinator.set_external_signal_source(source);
    }

    Ok(coordinator.run())
}
