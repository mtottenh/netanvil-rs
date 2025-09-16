use std::rc::Rc;
use std::time::Instant;

use crate::coordinator::Coordinator;
use crate::generator::SimpleGenerator;
use crate::handle::IoWorkerHandle;
use crate::io_worker::io_worker_loop;
use crate::result::TestResult;
use crate::scheduler::{ConstantRateScheduler, PoissonScheduler};
use crate::timer_thread::{self, TimerThreadHandle, FIRE_CHANNEL_CAPACITY};
use crate::transformer::{ConnectionPolicyTransformer, HeaderTransformer, NoopTransformer};
use netanvil_metrics::HdrMetricsCollector;
use netanvil_types::{
    ConnectionPolicy, RequestExecutor, RequestGenerator, RequestScheduler, RequestTransformer,
    SchedulerConfig, TestConfig,
};

// ---------------------------------------------------------------------------
// Convenience entry points (use defaults from TestConfig for everything)
// ---------------------------------------------------------------------------

/// Run a load test with the given configuration and executor factory.
///
/// Uses `SimpleGenerator` (round-robin URLs) and `HeaderTransformer` from config.
/// For custom generators or transformers, use [`TestBuilder`].
pub fn run_test<E, F>(config: TestConfig, executor_factory: F) -> netanvil_types::Result<TestResult>
where
    E: RequestExecutor<Spec = netanvil_types::HttpRequestSpec> + 'static,
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
    E: RequestExecutor<Spec = netanvil_types::HttpRequestSpec> + 'static,
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
    E: RequestExecutor<Spec = netanvil_types::HttpRequestSpec> + 'static,
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
///     fn generate(&mut self, _ctx: &RequestContext) -> HttpRequestSpec {
///         self.counter += 1;
///         HttpRequestSpec {
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
    E: RequestExecutor<Spec = netanvil_types::HttpRequestSpec> + 'static,
    F: Fn() -> E + Send + 'static,
{
    config: TestConfig,
    executor_factory: F,
    generator_factory: Option<crate::GeneratorFactory>,
    transformer_factory: Option<crate::TransformerFactory>,
    on_progress: Option<crate::ProgressCallback>,
    external_command_rx: Option<flume::Receiver<netanvil_types::WorkerCommand>>,
    pushed_signal_source: Option<crate::SignalSourceFn>,
}

impl<E, F> TestBuilder<E, F>
where
    E: RequestExecutor<Spec = netanvil_types::HttpRequestSpec> + 'static,
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
            pushed_signal_source: None,
        }
    }

    /// Set a custom generator factory. Called once per I/O worker core.
    ///
    /// The closure receives the `core_id` (0-based) so generators can
    /// partition their state space (e.g., different URL ranges per core,
    /// disjoint user ID pools, etc.).
    ///
    /// If not set, uses `SimpleGenerator` from the config's `targets` and `method`.
    pub fn generator_factory(
        mut self,
        factory: impl Fn(usize) -> Box<dyn RequestGenerator<Spec = netanvil_types::HttpRequestSpec>>
            + Send
            + 'static,
    ) -> Self {
        self.generator_factory = Some(Box::new(factory));
        self
    }

    /// Set a custom transformer factory. Called once per I/O worker core.
    ///
    /// If not set, uses `HeaderTransformer` from config's `headers`,
    /// wrapped with `ConnectionPolicyTransformer` if the connection policy
    /// is not `KeepAlive`.
    pub fn transformer_factory(
        mut self,
        factory: impl Fn(usize) -> Box<dyn RequestTransformer<Spec = netanvil_types::HttpRequestSpec>>
            + Send
            + 'static,
    ) -> Self {
        self.transformer_factory = Some(Box::new(factory));
        self
    }

    /// Set a progress callback invoked each coordinator tick (~10-100Hz).
    pub fn on_progress(
        mut self,
        callback: impl FnMut(&crate::coordinator::ProgressUpdate) + 'static,
    ) -> Self {
        self.on_progress = Some(Box::new(callback));
        self
    }

    /// Set an external command channel for mid-test control (HTTP API, etc.).
    pub fn external_commands(mut self, rx: flume::Receiver<netanvil_types::WorkerCommand>) -> Self {
        self.external_command_rx = Some(rx);
        self
    }

    /// Set a push-based signal source. Read each coordinator tick.
    /// Pushed signals override polled signals (from config) with the same key.
    /// Typically wired to `SharedState::drain_pushed_signals()` for `PUT /signal`.
    pub fn pushed_signal_source(mut self, f: impl FnMut() -> Vec<(String, f64)> + 'static) -> Self {
        self.pushed_signal_source = Some(Box::new(f));
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
            self.pushed_signal_source,
        )
    }
}

// ---------------------------------------------------------------------------
// GenericTestBuilder: for non-HTTP protocols (TCP, UDP, QUIC, etc.)
// ---------------------------------------------------------------------------

/// Builder for configuring and running a load test with any protocol.
///
/// Unlike [`TestBuilder`] (which provides HTTP-specific defaults), this builder
/// **requires** generator and transformer factories in the constructor.
/// Use this for non-HTTP protocols where no default generator exists.
///
/// # Example
///
/// ```ignore
/// use netanvil_core::GenericTestBuilder;
/// use netanvil_tcp::{TcpExecutor, SimpleTcpGenerator, TcpNoopTransformer, TcpFraming};
///
/// let gen_factory = Box::new(move |_core_id| {
///     Box::new(SimpleTcpGenerator::new(targets.clone(), payload.clone(), TcpFraming::Raw, true))
///         as Box<dyn RequestGenerator<Spec = TcpRequestSpec>>
/// });
/// let trans_factory = Box::new(|_| {
///     Box::new(TcpNoopTransformer) as Box<dyn RequestTransformer<Spec = TcpRequestSpec>>
/// });
///
/// let result = GenericTestBuilder::new(config, || TcpExecutor::new(), gen_factory, trans_factory)
///     .run()
///     .unwrap();
/// ```
pub struct GenericTestBuilder<E, F>
where
    E: RequestExecutor + 'static,
    F: Fn() -> E + Send + 'static,
{
    config: TestConfig,
    executor_factory: F,
    generator_factory: crate::GenericGeneratorFactory<E::Spec>,
    transformer_factory: crate::GenericTransformerFactory<E::Spec>,
    on_progress: Option<crate::ProgressCallback>,
    external_command_rx: Option<flume::Receiver<netanvil_types::WorkerCommand>>,
    pushed_signal_source: Option<crate::SignalSourceFn>,
}

impl<E, F> GenericTestBuilder<E, F>
where
    E: RequestExecutor + 'static,
    F: Fn() -> E + Send + 'static,
{
    /// Create a new generic test builder. Generator and transformer factories are required.
    pub fn new(
        config: TestConfig,
        executor_factory: F,
        generator_factory: crate::GenericGeneratorFactory<E::Spec>,
        transformer_factory: crate::GenericTransformerFactory<E::Spec>,
    ) -> Self {
        Self {
            config,
            executor_factory,
            generator_factory,
            transformer_factory,
            on_progress: None,
            external_command_rx: None,
            pushed_signal_source: None,
        }
    }

    /// Set a progress callback invoked each coordinator tick (~10-100Hz).
    pub fn on_progress(
        mut self,
        callback: impl FnMut(&crate::coordinator::ProgressUpdate) + 'static,
    ) -> Self {
        self.on_progress = Some(Box::new(callback));
        self
    }

    /// Set an external command channel for mid-test control (HTTP API, etc.).
    pub fn external_commands(mut self, rx: flume::Receiver<netanvil_types::WorkerCommand>) -> Self {
        self.external_command_rx = Some(rx);
        self
    }

    /// Set a push-based signal source. Read each coordinator tick.
    pub fn pushed_signal_source(mut self, f: impl FnMut() -> Vec<(String, f64)> + 'static) -> Self {
        self.pushed_signal_source = Some(Box::new(f));
        self
    }

    /// Build and run the test. Blocks until completion.
    pub fn run(self) -> netanvil_types::Result<TestResult> {
        run_test_core(
            self.config,
            self.executor_factory,
            self.generator_factory,
            self.transformer_factory,
            self.on_progress,
            self.external_command_rx,
            self.pushed_signal_source,
        )
    }
}

// ---------------------------------------------------------------------------
// Internal implementation
// ---------------------------------------------------------------------------

/// HTTP-specific entry point — fills in default generator/transformer, then delegates.
fn run_test_impl<E, F>(
    config: TestConfig,
    executor_factory: F,
    generator_factory: Option<crate::GeneratorFactory>,
    transformer_factory: Option<crate::TransformerFactory>,
    on_progress: Option<crate::ProgressCallback>,
    external_command_rx: Option<flume::Receiver<netanvil_types::WorkerCommand>>,
    pushed_signal_source: Option<crate::SignalSourceFn>,
) -> netanvil_types::Result<TestResult>
where
    E: RequestExecutor<Spec = netanvil_types::HttpRequestSpec> + 'static,
    F: Fn() -> E + Send + 'static,
{
    // Fill in HTTP-specific defaults for generator
    let gen_factory: crate::GeneratorFactory = match generator_factory {
        Some(f) => f,
        None => {
            let targets = config.targets.clone();
            let method_str = config.method.clone();
            Box::new(move |_core_id| {
                let method: http::Method = method_str.parse().unwrap_or(http::Method::GET);
                Box::new(SimpleGenerator::new(targets.clone(), method))
                    as Box<dyn RequestGenerator<Spec = netanvil_types::HttpRequestSpec>>
            })
        }
    };

    // Fill in HTTP-specific defaults for transformer
    let trans_factory: crate::TransformerFactory = match transformer_factory {
        Some(f) => f,
        None => {
            let headers = config.headers.clone();
            let conn_policy = config.connections.connection_policy.clone();
            Box::new(move |_core_id| {
                let base: Box<dyn RequestTransformer<Spec = netanvil_types::HttpRequestSpec>> =
                    if headers.is_empty() {
                        Box::new(NoopTransformer)
                    } else {
                        Box::new(HeaderTransformer::new(headers.clone()))
                    };

                match &conn_policy {
                    ConnectionPolicy::KeepAlive => base,
                    policy => Box::new(ConnectionPolicyTransformer::new(base, policy.clone())),
                }
            })
        }
    };

    run_test_core(
        config,
        executor_factory,
        gen_factory,
        trans_factory,
        on_progress,
        external_command_rx,
        pushed_signal_source,
    )
}

/// Protocol-agnostic engine core. Orchestrates workers, timer, and coordinator.
///
/// All factories are required (non-optional). The HTTP-specific `run_test_impl`
/// fills in defaults before calling this. Non-HTTP protocols (TCP, UDP, QUIC)
/// call this directly via `GenericTestBuilder`.
fn run_test_core<E, F>(
    config: TestConfig,
    executor_factory: F,
    generator_factory: crate::GenericGeneratorFactory<E::Spec>,
    transformer_factory: crate::GenericTransformerFactory<E::Spec>,
    on_progress: Option<crate::ProgressCallback>,
    external_command_rx: Option<flume::Receiver<netanvil_types::WorkerCommand>>,
    pushed_signal_source: Option<crate::SignalSourceFn>,
) -> netanvil_types::Result<TestResult>
where
    E: RequestExecutor + 'static,
    F: Fn() -> E + Send + 'static,
{
    // ── Core budget: reserve 1 for timer, 1 for coordinator/API ──
    let available_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let num_cores = if config.num_cores == 0 {
        // Auto-detect: subtract 2 reserved cores (timer + misc)
        available_cores.saturating_sub(2).max(1)
    } else {
        config.num_cores
    };

    let timer_core = 0usize;
    let io_core_start = 1usize;
    // Coordinator/API/misc get the last core (or share core 0 on small machines)
    let misc_core = if available_cores > num_cores + 1 {
        num_cores + 1
    } else {
        0 // share with timer on small machines
    };

    tracing::info!(
        available_cores,
        io_workers = num_cores,
        timer_core,
        io_cores = ?(io_core_start..io_core_start + num_cores),
        misc_core,
        "thread pinning layout"
    );

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
    let generator_factory_ptr =
        &generator_factory as *const crate::GenericGeneratorFactory<E::Spec> as usize;
    let transformer_factory_ptr =
        &transformer_factory as *const crate::GenericTransformerFactory<E::Spec> as usize;

    for core_id in 0..num_cores {
        let (metrics_tx, metrics_rx) = flume::unbounded();
        let fire_rx = fire_rxs.remove(0);
        let config = config.clone();

        let thread = std::thread::Builder::new()
            .name(format!("netanvil-io-{core_id}"))
            .spawn(move || {
                // Pin to dedicated core (offset by io_core_start)
                let pin_id = io_core_start + core_id;
                let pin_core = core_affinity::CoreId { id: pin_id };
                if !core_affinity::set_for_current(pin_core) {
                    tracing::warn!(core_id, pin_id, "failed to pin I/O worker");
                }

                // Elevate I/O worker priority (nice -10)
                #[cfg(target_os = "linux")]
                unsafe {
                    let rc = libc::setpriority(libc::PRIO_PROCESS, 0, -10);
                    if rc != 0 {
                        tracing::debug!(core_id, "I/O worker: nice -10 failed (not privileged)");
                    }
                }

                // SAFETY: pointers valid because parent joins all threads before returning.
                let executor_factory = unsafe { &*(executor_factory_ptr as *const F) };
                let generator_factory = unsafe {
                    &*(generator_factory_ptr as *const crate::GenericGeneratorFactory<E::Spec>)
                };
                let transformer_factory = unsafe {
                    &*(transformer_factory_ptr as *const crate::GenericTransformerFactory<E::Spec>)
                };

                let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
                rt.block_on(async {
                    let generator = generator_factory(core_id);
                    let transformer = transformer_factory(core_id);
                    let executor = executor_factory();
                    let collector = HdrMetricsCollector::with_signal_configs(
                        config.error_status_threshold,
                        config.tracked_response_headers.clone(),
                        config.md5_check_enabled,
                        config.response_signal_headers.clone(),
                    );

                    io_worker_loop(
                        crate::io_worker::IoWorkerConfig {
                            fire_rx,
                            metrics_tx,
                            core_id,
                            metrics_interval: config.metrics_interval,
                            graceful_shutdown: config.graceful_shutdown,
                        },
                        generator,
                        Rc::new(transformer),
                        Rc::new(executor),
                        Rc::new(collector),
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
    let timer_stats = timer_thread::TimerStats::new();
    let timer_stats_clone = timer_stats.clone();
    let timer_thread = std::thread::Builder::new()
        .name("netanvil-timer".into())
        .spawn(move || {
            // Pin timer to dedicated core
            let core = core_affinity::CoreId { id: timer_core };
            if !core_affinity::set_for_current(core) {
                tracing::warn!(timer_core, "failed to pin timer thread");
            }

            // Elevate timer priority: try SCHED_FIFO first, fall back to nice -20
            #[cfg(target_os = "linux")]
            unsafe {
                let param = libc::sched_param { sched_priority: 50 };
                if libc::sched_setscheduler(0, libc::SCHED_FIFO, &param) == 0 {
                    tracing::info!("timer thread: SCHED_FIFO priority 50");
                } else {
                    // SCHED_FIFO requires CAP_SYS_NICE — fall back to nice
                    if libc::setpriority(libc::PRIO_PROCESS, 0, -20) == 0 {
                        tracing::info!("timer thread: nice -20 (SCHED_FIFO unavailable)");
                    } else {
                        tracing::warn!("timer thread: failed to elevate priority");
                    }
                }
            }

            timer_thread::timer_loop(schedulers, fire_txs, timer_cmd_rx, timer_stats_clone);
        })
        .map_err(|e| netanvil_types::NetAnvilError::Other(format!("spawn timer thread: {e}")))?;

    let timer_handle = TimerThreadHandle {
        command_tx: timer_cmd_tx,
        thread: Some(timer_thread),
        stats: timer_stats,
    };

    // ── Create coordinator ──
    let rate_controller =
        crate::build_rate_controller(&config.rate, config.control_interval, start_time);

    // Pin coordinator (main thread) to misc core
    if misc_core > 0 && misc_core != timer_core {
        let core = core_affinity::CoreId { id: misc_core };
        if !core_affinity::set_for_current(core) {
            tracing::debug!(misc_core, "failed to pin coordinator to misc core");
        }
    }

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

    // Wire pull-based external signal source from config (e.g. server load metric)
    if let Some(source) = crate::signal::make_signal_source(
        config.external_metrics_url.as_deref(),
        config.external_metrics_field.as_deref(),
    ) {
        coordinator.set_external_signal_source(source);
    }

    // Wire push-based signal source (e.g. from PUT /signal API endpoint)
    if let Some(source) = pushed_signal_source {
        coordinator.set_pushed_signal_source(source);
    }

    // Wire response signal extraction configs
    if !config.response_signal_headers.is_empty() {
        coordinator.set_response_signal_configs(config.response_signal_headers.clone());
    }

    // Wire termination controls from config
    if let Some(n) = config.max_requests {
        coordinator.max_requests(n);
    }
    if let Some(n) = config.autostop_threshold {
        coordinator.autostop_threshold(n);
    }
    if let Some(n) = config.refusestop_threshold {
        coordinator.refusestop_threshold(n);
    }
    if let Some(d) = config.warmup_duration {
        coordinator.warmup_duration(d);
    }
    if let Some(n) = config.target_bytes {
        coordinator.target_bytes(n);
    }

    Ok(coordinator.run())
}
