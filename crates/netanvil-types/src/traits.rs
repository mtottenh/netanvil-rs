use std::future::Future;
use std::time::Instant;

use crate::metrics::{MetricsSummary, MetricsSnapshot, RateDecision};
use crate::request::{ExecutionResult, RequestContext, RequestSpec};

// ---------------------------------------------------------------------------
// Hot-path traits: per-core, !Send, called at high frequency.
// No Send/Sync bounds. No supertrait requirements.
// ---------------------------------------------------------------------------

/// Computes when to fire the next request.
///
/// Implementations: ConstantRateScheduler, PoissonScheduler
pub trait RequestScheduler {
    /// Returns the next intended send time, or None if the test is done.
    fn next_request_time(&mut self) -> Option<Instant>;

    /// Update this core's target request rate.
    fn update_rate(&mut self, rps: f64);
}

/// Creates request specifications from context.
///
/// Implementations: SimpleGenerator, TemplatedGenerator
pub trait RequestGenerator {
    fn generate(&mut self, context: &RequestContext) -> RequestSpec;
}

/// Modifies requests before execution.
///
/// Implementations: NoopTransformer, HeaderTransformer
pub trait RequestTransformer {
    fn transform(&self, spec: RequestSpec, context: &RequestContext) -> RequestSpec;
}

/// Executes requests against the target system.
///
/// Takes `&self` because it is `Rc`-shared across concurrent spawned tasks
/// on a single core. Uses interior mutability for connection pool state.
///
/// Implementations: HttpExecutor (netanvil-http)
pub trait RequestExecutor {
    fn execute(
        &self,
        spec: &RequestSpec,
        context: &RequestContext,
    ) -> impl Future<Output = ExecutionResult>;
}

/// Records per-request metrics. `Rc`-shared across spawned tasks.
/// Uses interior mutability (`RefCell<Histogram>`, `Cell<u64>` counters).
///
/// Implementations: HdrMetricsCollector (netanvil-metrics)
pub trait MetricsCollector {
    fn record(&self, result: &ExecutionResult);
    fn snapshot(&self) -> MetricsSnapshot;
}

// ---------------------------------------------------------------------------
// Blanket impls for boxed traits.
// These allow the engine to construct components from config at runtime
// while the Worker remains generic (monomorphized over Box<dyn Trait>).
// The vtable dispatch cost (~1ns) is negligible at per-request frequency.
// ---------------------------------------------------------------------------

impl RequestScheduler for Box<dyn RequestScheduler> {
    fn next_request_time(&mut self) -> Option<Instant> {
        (**self).next_request_time()
    }
    fn update_rate(&mut self, rps: f64) {
        (**self).update_rate(rps)
    }
}

impl RequestGenerator for Box<dyn RequestGenerator> {
    fn generate(&mut self, context: &RequestContext) -> RequestSpec {
        (**self).generate(context)
    }
}

impl RequestTransformer for Box<dyn RequestTransformer> {
    fn transform(&self, spec: RequestSpec, context: &RequestContext) -> RequestSpec {
        (**self).transform(spec, context)
    }
}

// ---------------------------------------------------------------------------
// Control-plane trait: runs in coordinator, called at ~10-100Hz.
// ---------------------------------------------------------------------------

/// Pure computation: metrics summary in, rate decision out.
///
/// The coordinator owns the controller exclusively — no sharing, no Send needed.
///
/// Implementations: StaticRateController, StepRateController, PidRateController,
///                  ExternalRateController
pub trait RateController {
    fn update(&mut self, summary: &MetricsSummary) -> RateDecision;
    fn current_rate(&self) -> f64;
}
