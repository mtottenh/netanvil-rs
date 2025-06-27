//! Load test engine: worker, coordinator, schedulers, rate controllers.

pub mod controller;
pub mod coordinator;
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

/// A factory closure that creates a [`netanvil_types::RequestGenerator`] per core.
pub type GeneratorFactory = Box<dyn Fn(usize) -> Box<dyn netanvil_types::RequestGenerator> + Send>;

/// A factory closure that creates a [`netanvil_types::RequestTransformer`] per core.
pub type TransformerFactory =
    Box<dyn Fn(usize) -> Box<dyn netanvil_types::RequestTransformer> + Send>;

pub use controller::{
    AutotuneParams, AutotuningPidController, CompositePidController, PidGainValues,
    PidRateController, PidStepInput, StaticRateController, StepRateController,
};
pub use coordinator::Coordinator;
pub use coordinator::ProgressUpdate;
pub use engine::{run_test, run_test_with_api, run_test_with_progress, TestBuilder};
pub use generator::SimpleGenerator;
pub use handle::IoWorkerHandle;
pub use io_worker::{io_worker_loop, IoWorkerConfig};
pub use report::{ProgressLine, Report};
pub use result::TestResult;
pub use scheduler::{ConstantRateScheduler, PoissonScheduler};
pub use timer_thread::TimerThreadHandle;
pub use transformer::{ConnectionPolicyTransformer, HeaderTransformer, NoopTransformer};
