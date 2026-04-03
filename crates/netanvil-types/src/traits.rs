use std::future::Future;
use std::time::Instant;

use crate::controller::{ControllerInfo, ControllerType};
use crate::metrics::{MetricsSnapshot, MetricsSummary, PacketCounterDeltas, RateDecision};
use crate::request::{ExecutionResult, HttpRequestSpec, ProtocolSpec, RequestContext};

// ---------------------------------------------------------------------------
// Hot-path traits: per-core, !Send, called at high frequency.
// No Send/Sync bounds. No supertrait requirements.
//
// Generator, Transformer, and Executor are generic over a protocol-specific
// Spec type via an associated type. For HTTP, Spec = HttpRequestSpec.
// Future protocols (TCP, UDP, gRPC) define their own spec types.
// ---------------------------------------------------------------------------

/// Computes when to fire the next request.
///
/// Implementations: ConstantRateScheduler, PoissonScheduler
pub trait RequestScheduler: Send {
    /// Returns the next intended send time, or None if the test is done.
    fn next_request_time(&mut self) -> Option<Instant>;

    /// Update this core's target request rate.
    fn update_rate(&mut self, rps: f64);
}

/// Creates protocol-specific request specifications from context.
///
/// Implementations: SimpleGenerator, HybridGenerator, WasmGenerator, LuaJitGenerator
pub trait RequestGenerator {
    type Spec: ProtocolSpec;

    fn generate(&mut self, context: &RequestContext) -> Self::Spec;

    /// Called with the result of a completed request.
    ///
    /// Invoked on the I/O worker main loop BEFORE the next `generate()` call,
    /// so `&mut self` is safe. Only called when `wants_responses()` returns true.
    /// Enables stateful session simulation, custom assertions, and adaptive payloads.
    fn on_response(&mut self, _result: &ExecutionResult) {}

    /// Whether this generator wants response callbacks via `on_response`.
    ///
    /// When false (default), zero overhead — no channel created, no results cloned.
    /// When true, the I/O worker channels completed `ExecutionResult`s back to
    /// the main loop and delivers them before each `generate()` call.
    fn wants_responses(&self) -> bool {
        false
    }

    /// Replace the target URLs mid-test. Default: no-op.
    fn update_targets(&mut self, _targets: Vec<String>) {}
}

/// Modifies requests before execution.
///
/// Uses `&self` for `transform` (called from Rc-shared context).
/// Uses `&self` for `update_metadata` (interior mutability via RefCell).
///
/// Implementations: NoopTransformer, HeaderTransformer, ConnectionPolicyTransformer
pub trait RequestTransformer {
    type Spec: ProtocolSpec;

    fn transform(&self, spec: Self::Spec, context: &RequestContext) -> Self::Spec;

    /// Replace protocol-specific metadata mid-test.
    /// For HTTP: headers as key-value pairs. Default: no-op.
    fn update_metadata(&self, _metadata: Vec<(String, String)>) {}
}

/// Source of protocol-level packet counters (UDP loss bitmap, TCP_INFO, etc.).
///
/// Shared between the executor (which populates it) and the metrics collector
/// (which reads it during `snapshot()`).  Implementations use interior
/// mutability and return deltas since the last call.
pub trait PacketCounterSource: Default {
    fn take_packet_deltas(&self) -> PacketCounterDeltas;
}

/// No-op packet counter source for protocols that don't track packet loss.
#[derive(Debug, Default)]
pub struct NoopPacketSource;

impl PacketCounterSource for NoopPacketSource {
    #[inline]
    fn take_packet_deltas(&self) -> PacketCounterDeltas {
        PacketCounterDeltas::default()
    }
}

/// Executes requests against the target system.
///
/// Takes `&self` because it is `Rc`-shared across concurrent spawned tasks
/// on a single core. Uses interior mutability for connection pool state.
///
/// Implementations: HttpExecutor (netanvil-http)
pub trait RequestExecutor {
    type Spec: ProtocolSpec;
    type PacketSource: PacketCounterSource;

    fn execute(
        &self,
        spec: &Self::Spec,
        context: &RequestContext,
    ) -> impl Future<Output = ExecutionResult>;

    /// Return the packet counter source shared with this executor.
    /// Called once per I/O worker at setup time, not on the hot path.
    fn packet_counter_source(&self) -> Option<Self::PacketSource> {
        None
    }
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

impl<G: RequestGenerator + ?Sized> RequestGenerator for Box<G> {
    type Spec = G::Spec;
    fn generate(&mut self, context: &RequestContext) -> Self::Spec {
        (**self).generate(context)
    }
    fn on_response(&mut self, result: &ExecutionResult) {
        (**self).on_response(result)
    }
    fn wants_responses(&self) -> bool {
        (**self).wants_responses()
    }
    fn update_targets(&mut self, targets: Vec<String>) {
        (**self).update_targets(targets)
    }
}

impl EventRecorder for Box<dyn EventRecorder> {
    fn record(&self, result: &ExecutionResult) {
        (**self).record(result)
    }
    fn flush(&self) {
        (**self).flush()
    }
}

impl<T: RequestTransformer + ?Sized> RequestTransformer for Box<T> {
    type Spec = T::Spec;
    fn transform(&self, spec: Self::Spec, context: &RequestContext) -> Self::Spec {
        (**self).transform(spec, context)
    }
    fn update_metadata(&self, metadata: Vec<(String, String)>) {
        (**self).update_metadata(metadata)
    }
}

// ---------------------------------------------------------------------------
// Per-request event recording trait: streams individual request records.
// Separate from MetricsCollector (which produces aggregates).
// ---------------------------------------------------------------------------

/// Records per-request events to a streaming sink (Arrow IPC file, etc.).
///
/// One instance per I/O worker core. Uses `&self` with interior mutability
/// (same contract as `MetricsCollector`). Called on the hot path — keep fast.
///
/// Implementations: `NoopEventRecorder` (netanvil-types), `ArrowEventRecorder` (netanvil-events)
pub trait EventRecorder {
    /// Record a completed request event.
    fn record(&self, result: &ExecutionResult);
    /// Flush buffered records to the underlying sink.
    fn flush(&self);
}

/// No-op event recorder. Zero overhead when event logging is disabled.
pub struct NoopEventRecorder;

impl EventRecorder for NoopEventRecorder {
    #[inline(always)]
    fn record(&self, _result: &ExecutionResult) {}
    #[inline(always)]
    fn flush(&self) {}
}

// ---------------------------------------------------------------------------
// Convenience type aliases for HTTP-specific trait objects.
// ---------------------------------------------------------------------------

/// HTTP request generator (trait object).
pub type HttpGenerator = dyn RequestGenerator<Spec = HttpRequestSpec>;

/// HTTP request transformer (trait object).
pub type HttpTransformer = dyn RequestTransformer<Spec = HttpRequestSpec>;

// ---------------------------------------------------------------------------
// Control-plane trait: runs in coordinator, called at ~10-100Hz.
// ---------------------------------------------------------------------------

/// Pure computation: metrics summary in, rate decision out.
///
/// The coordinator owns the controller exclusively — no sharing, no Send needed.
///
/// Implementations: StaticRateController, StepRateController, Arbiter
pub trait RateController {
    fn update(&mut self, summary: &MetricsSummary) -> RateDecision;
    fn current_rate(&self) -> f64;

    /// Override the target rate externally (e.g. from the control API).
    /// The controller should adopt this as its new baseline.
    /// Default: no-op (controller ignores external overrides).
    fn set_rate(&mut self, _rps: f64) {}

    /// Dynamically adjust the upper RPS bound.
    /// Used by the Arbiter's progressive ceiling.
    /// Default: no-op (controllers without max_rps ignore this).
    fn set_max_rps(&mut self, _max_rps: f64) {}

    /// Return controller-specific state as key-value pairs for Prometheus.
    /// The distributed coordinator includes these in progress updates so
    /// they can be rendered as gauges (e.g., ramp ceiling, effective ceiling).
    /// Default: empty (no custom state to export).
    fn controller_state(&self) -> Vec<(&'static str, f64)> {
        Vec::new()
    }

    /// Return structured information about the controller for introspection.
    /// Default: returns a minimal description with no editable actions.
    fn controller_info(&self) -> ControllerInfo {
        ControllerInfo {
            controller_type: ControllerType::Static,
            current_rps: self.current_rate(),
            editable_actions: vec![],
            params: serde_json::Value::Null,
        }
    }

    /// Return the name of the binding constraint from the last tick.
    /// Used by shadow validation to diagnose divergences between controllers.
    /// Default: None (controller doesn't track binding constraints).
    fn last_binding(&self) -> Option<&str> {
        None
    }

    /// Apply a typed parameter update. Returns a JSON response body on
    /// success, or an error message on failure.
    ///
    /// The `action` string and `params` JSON are parsed from the PUT body.
    /// Each controller type handles its own action set; unrecognized actions
    /// return Err.
    ///
    /// Default: rejects all actions.
    fn apply_update(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let _ = params;
        Err(format!(
            "action '{}' is not valid for controller type '{:?}'",
            action,
            self.controller_info().controller_type
        ))
    }
}
