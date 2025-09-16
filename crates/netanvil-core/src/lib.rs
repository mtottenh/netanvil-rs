//! Load test engine: worker, coordinator, schedulers, rate controllers.

pub mod capture;
pub mod controller;
pub mod coordinator;
pub mod dropping;
pub mod engine;
pub mod generator;
pub mod handle;
pub mod io_worker;
pub mod report;
pub mod result;
pub mod scheduler;
pub mod signal;
pub mod timer_thread;
pub mod transformer;

/// A closure that produces `(signal_name, value)` pairs when polled.
pub type SignalSourceFn = Box<dyn FnMut() -> Vec<(String, f64)>>;

/// A closure invoked each coordinator tick with live progress.
pub type ProgressCallback = Box<dyn FnMut(&coordinator::ProgressUpdate)>;

/// A factory closure that creates a [`netanvil_types::RequestGenerator`] per core (generic).
pub type GenericGeneratorFactory<S> =
    Box<dyn Fn(usize) -> Box<dyn netanvil_types::RequestGenerator<Spec = S>> + Send>;

/// A factory closure that creates a [`netanvil_types::RequestTransformer`] per core (generic).
pub type GenericTransformerFactory<S> =
    Box<dyn Fn(usize) -> Box<dyn netanvil_types::RequestTransformer<Spec = S>> + Send>;

/// HTTP-specific generator factory (backward compatible alias).
pub type GeneratorFactory = GenericGeneratorFactory<netanvil_types::HttpRequestSpec>;

/// HTTP-specific transformer factory (backward compatible alias).
pub type TransformerFactory = GenericTransformerFactory<netanvil_types::HttpRequestSpec>;

pub use controller::builder::build_rate_controller;
pub use controller::{
    AutotuneParams, AutotuningPidController, CompositePidController, PidGainValues,
    PidRateController, PidStepInput, RampConfig, RampRateController, StaticRateController,
    StepRateController,
};
pub use coordinator::Coordinator;
pub use coordinator::ProgressUpdate;
pub use engine::{
    run_test, run_test_with_api, run_test_with_progress, GenericTestBuilder, TestBuilder,
};
pub use generator::SimpleGenerator;
pub use handle::IoWorkerHandle;
pub use io_worker::{io_worker_loop, IoWorkerConfig};
pub use report::{ProgressLine, Report};
pub use result::TestResult;
pub use scheduler::{ConstantRateScheduler, PoissonScheduler};
pub use timer_thread::TimerThreadHandle;
pub use transformer::{ConnectionPolicyTransformer, HeaderTransformer, NoopTransformer};
