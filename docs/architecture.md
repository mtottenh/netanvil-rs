# NetAnvil-RS Architecture

This is the authoritative design document for netanvil-rs. It supersedes all prior
design documents (archived in `docs/archive/`).

## 1. Design Principles

1. **Shared-nothing, thread-per-core.** Each CPU core runs an independent worker
   with its own scheduler, connection pool, and metrics. No locks, no atomics on
   the hot path. Linear scaling with core count.

2. **Coordinator pattern.** A single coordinator thread distributes rate targets
   and collects metrics. The same pattern scales from cores to nodes — replace a
   channel with a network connection and you have distributed load testing.

3. **Coordinated omission prevention.** Every request records its *intended* send
   time and *actual* send time. Latency is measured from the intended time, not
   the actual time. This prevents the system from hiding queuing delays.

4. **Traits for extensibility, not abstraction.** Core traits are minimal
   interfaces that advanced features implement. PID control, session simulation,
   eBPF profiling, and distributed coordination are all just trait implementations
   or decorators — not new abstraction layers.

5. **No premature decomposition.** Start with 5 crates. Split only when real
   dependency boundaries or code size justify it.

## 2. Runtime: compio

**Choice:** [compio](https://github.com/compio-rs/compio) — a completion-based
async runtime with thread-per-core architecture.

**Rationale:**
- **io_uring on Linux** for optimal connection throughput (async connect, send,
  recv — no readiness notification overhead)
- **Thread-per-core** with `!Send` tasks, `Rc`-based sharing — no atomic ops
- **Built-in Dispatcher** for multi-core work distribution with CPU pinning
- **hyper bridge** via cyper-core — uses the battle-tested hyper HTTP engine
  (HTTP/1.1, HTTP/2) without reimplementing protocol logic
- **Cross-platform** — io_uring on Linux, IOCP on Windows, kqueue on macOS
  (fallback for development; production perf requires Linux)
- **Actively maintained** — frequent releases (v0.18.0 Jan 2026), responsive
  maintainer, growing community

**Rejected alternatives:**
- *Glommio*: Effectively abandoned (seeking maintainers as of March 2026). No
  hyper bridge. Linux-only with no fallback.
- *Monoio*: Viable but declining commit rate. HTTP stack is a custom h2 fork
  (less proven than hyper). No built-in dispatcher pattern.
- *Tokio*: Not thread-per-core. Would require `Send + Sync` bounds everywhere,
  which is the wrong model for maximum per-core throughput.

**Implications for code:**
- Hot-path types are `!Send`. Use `Rc<RefCell<...>>` for per-core shared state.
- Cross-core communication via `flume` channels (or similar).
- `async fn` in traits is used without `Send` bounds on the returned future.
- compio's buffer ownership model (`IoBuf`/`IoBufMut` with `BufResult`) flows
  through the HTTP executor. This is handled inside `netanvil-http`; the rest of
  the system works with `RequestSpec` and `ExecutionResult` value types.

## 3. System Architecture

### 3.1 Overview

```
                     ┌──────────────────────┐
                     │     Coordinator       │  own thread, synchronous loop (~10-100Hz)
                     │                       │
                     │  RateController       │  ← pluggable: static, PID, step, external
                     │  AggregateMetrics     │  ← merged from per-core snapshots
                     │  TestLifecycle        │  ← warmup → run → cooldown → done
                     └───┬──────────────┬────┘
                         │              │
                rate_tx  │              │  metrics_rx         (flume channels)
               (f64)     │              │  (MetricsSnapshot)
                         │              │
            ┌────────────┼──────────────┼────────────┐
            │            │              │             │
            ▼            ▼              ▲             ▲
     ┌────────────┐┌────────────┐┌────────────┐┌────────────┐
     │  Worker 0  ││  Worker 1  ││  Worker 2  ││  Worker 3  │
     │  (core 0)  ││  (core 1)  ││  (core 2)  ││  (core 3)  │
     │            ││            ││            ││            │
     │ Scheduler  ││ Scheduler  ││ Scheduler  ││ Scheduler  │  ← per-core rate target
     │ Generator  ││ Generator  ││ Generator  ││ Generator  │  ← generates RequestSpec
     │ Transformer││ Transformer││ Transformer││ Transformer│  ← adds headers, auth, etc.
     │ Executor   ││ Executor   ││ Executor   ││ Executor   │  ← HTTP client (Rc-shared)
     │ Metrics    ││ Metrics    ││ Metrics    ││ Metrics    │  ← HDR histogram (Rc-shared)
     │            ││            ││            ││            │
     │ compio rt  ││ compio rt  ││ compio rt  ││ compio rt  │  ← each has own io_uring
     └────────────┘└────────────┘└────────────┘└────────────┘
```

### 3.2 Shared-Nothing Per-Core State

Each worker owns (not shares) all its state:

| State | Per-core? | Rationale |
|-------|-----------|-----------|
| Scheduler | Yes | Each core has its own rate target (global / N) |
| Connection pool | Yes | Each core manages its own TCP connections |
| HDR histogram | Yes | Record locally, merge periodically |
| Session state | Yes | Sessions never cross core boundaries |
| Request generator | Yes | Stateful generators partition the ID/URL space |
| Test config | Yes (cloned) | Read-only after startup, clone to each core |

**Nothing is shared between cores during the hot path.** The only cross-core
communication is via channels to the coordinator, at low frequency.

### 3.3 Communication Model

```
Coordinator ←→ Worker communication:

  Coordinator → Worker:  WorkerCommand enum via flume::Sender
    - UpdateRate(f64)    ~10-100Hz
    - Stop               once

  Worker → Coordinator:  MetricsSnapshot via flume::Sender
    - Histogram + counters   ~1-10Hz
```

The channel boundary is where **type erasure** happens. The coordinator holds
`Vec<WorkerHandle>` and knows nothing about the worker's generic types
(`<S, G, T, E, M>`). This is important — it means the coordinator doesn't need
to be generic over worker internals.

### 3.4 Concurrency Within a Core

The worker's scheduling loop spawns each request as a separate compio task:

```
Scheduling loop (single logical thread):
  1. Compute next intended send time
  2. Sleep until that time
  3. Generate + transform request spec (synchronous, cheap)
  4. Spawn async task for execution (does not block the loop)
  5. Go to 1

Spawned tasks (multiplexed by io_uring on the same core):
  - TCP connect (if needed)
  - TLS handshake (if needed)
  - Send HTTP request
  - Read HTTP response
  - Record metrics
```

Hundreds or thousands of requests can be in-flight simultaneously on a single
core. The scheduling loop is never blocked by network I/O. `compio::runtime::spawn`
creates `!Send` tasks on the same core — no cross-core scheduling.

## 4. Core Traits

All traits live in `netanvil-types`. No runtime dependency. No `Send + Sync`
bounds on hot-path traits. No supertrait requirements (profiling is a decorator).

### 4.1 Hot-Path Traits (per-core, `!Send`)

```rust
use std::time::Instant;

/// Computes when to fire the next request.
///
/// Implementations: ConstantRateScheduler, PoissonScheduler
/// Called by: Worker scheduling loop (single caller, owns the scheduler)
pub trait RequestScheduler {
    /// Returns the next intended send time, or None if test is done.
    fn next_request_time(&mut self) -> Option<Instant>;

    /// Update this core's rate target. Called when coordinator adjusts rate.
    fn update_rate(&mut self, rps: f64);
}

/// Creates request specifications from context.
///
/// Implementations: SimpleGenerator, TemplatedGenerator, SessionAwareGenerator
/// Called by: Worker scheduling loop
pub trait RequestGenerator {
    fn generate(&mut self, context: &RequestContext) -> RequestSpec;
}

/// Modifies requests before execution.
///
/// Implementations: HeaderTransformer, AuthTransformer, CookieTransformer
/// Called by: Worker scheduling loop (could also be Rc-shared if needed)
pub trait RequestTransformer {
    fn transform(&self, spec: RequestSpec, context: &RequestContext) -> RequestSpec;
}

/// Executes requests against the target system.
///
/// Takes &self because it is Rc-shared across concurrent spawned tasks.
/// Uses interior mutability for connection pool state.
///
/// Implementations: HttpExecutor (netanvil-http)
/// Future: GrpcExecutor, WebSocketExecutor
pub trait RequestExecutor {
    async fn execute(
        &self,
        spec: &RequestSpec,
        context: &RequestContext,
    ) -> ExecutionResult;
}

/// Records per-request metrics. Rc-shared across spawned tasks.
/// Uses interior mutability (RefCell<HdrHistogram>, Cell<u64> counters).
///
/// Implementations: HdrMetricsCollector (netanvil-metrics)
pub trait MetricsCollector {
    fn record(&self, result: &ExecutionResult);
    fn snapshot(&self) -> MetricsSnapshot;
}
```

### 4.2 Control-Plane Traits

```rust
/// Pure computation: aggregate metrics in, rate decision out.
///
/// No async. No runtime dependency. No Send bound on the trait itself.
/// The coordinator owns the controller — no sharing needed.
///
/// Implementations:
///   StaticRateController    — constant rate
///   StepRateController      — time-based rate changes
///   PidRateController       — PID feedback loop on latency/error rate
///   ExternalRateController  — receives targets from outside (API, distributed leader)
pub trait RateController {
    fn update(&mut self, metrics: &AggregateMetrics) -> RateDecision;
    fn current_rate(&self) -> f64;
}
```

### 4.3 Why No ProfilingCapability Supertrait

The prior design required every trait to inherit from `ProfilingCapability`. This
forced every `RateController`, `RequestScheduler`, etc. to carry profiling state
even when unused.

Instead, profiling is a **decorator** that wraps any `RequestExecutor`:

```rust
struct ProfilingExecutor<E: RequestExecutor> {
    inner: E,
    profiler: Box<dyn Profiler>,  // eBPF, tracing, noop
}

impl<E: RequestExecutor> RequestExecutor for ProfilingExecutor<E> {
    async fn execute(&self, spec: &RequestSpec, ctx: &RequestContext) -> ExecutionResult {
        self.profiler.begin(ctx.request_id);
        let mut result = self.inner.execute(spec, ctx).await;
        result.profile = self.profiler.end(ctx.request_id);
        result
    }
}
```

Zero cost when unused. Composable. No impact on other traits.

## 5. Core Data Types

All types live in `netanvil-types`. Types that cross the network boundary (for
distributed mode) derive `Serialize`/`Deserialize`.

### 5.1 Request Pipeline Types

```rust
/// Context for a single request. Created by the scheduling loop.
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// Unique request ID (partitioned by core: core_id * MAX + sequence)
    pub request_id: u64,
    /// When this request SHOULD have been sent (for coordinated omission)
    pub intended_time: Instant,
    /// When it was actually dispatched
    pub actual_time: Instant,
    /// Which core is executing this request
    pub core_id: usize,
    /// Whether this request is selected for detailed sampling
    pub is_sampled: bool,
    /// Session ID, if session simulation is active
    pub session_id: Option<SessionId>,
}

/// What to send. Produced by RequestGenerator, modified by RequestTransformer.
#[derive(Debug, Clone)]
pub struct RequestSpec {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// What happened. Produced by RequestExecutor, consumed by MetricsCollector.
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub request_id: u64,
    pub intended_time: Instant,
    pub actual_time: Instant,
    pub timing: TimingBreakdown,
    pub status: Option<u16>,
    pub response_size: u64,
    pub error: Option<ExecutionError>,
    /// Optional profiling data (populated by ProfilingExecutor decorator)
    pub profile: Option<ProfileData>,
}

/// Latency breakdown for a single request.
#[derive(Debug, Clone, Default)]
pub struct TimingBreakdown {
    pub dns_lookup: Duration,
    pub tcp_connect: Duration,
    pub tls_handshake: Duration,
    pub time_to_first_byte: Duration,
    pub content_transfer: Duration,
    pub total: Duration,
}
```

### 5.2 Metrics Types

**Design constraint for distributed:** All metrics types must support associative,
commutative merge. Store raw data (histograms, counters), not derived values
(percentiles, rates). Derived values are computed on read, not stored.

```rust
/// Per-core metrics snapshot. Sent from worker to coordinator.
/// Represents a time window of recorded measurements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    /// Latency histogram (encoded HDR histogram)
    pub latency_histogram: EncodedHistogram,
    /// Total requests completed in this window
    pub total_requests: u64,
    /// Total errors in this window
    pub total_errors: u64,
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Total bytes received
    pub bytes_received: u64,
    /// Number of coordinated-omission-adjusted entries
    /// (requests where actual_time - intended_time > threshold)
    pub co_adjusted_count: u64,
    /// Window boundaries
    pub window_start: Instant,
    pub window_end: Instant,
}

/// Aggregated metrics from multiple sources (cores or nodes).
/// This type is the input to RateController.
///
/// IMPORTANT: merge() must be associative and commutative. This property
/// is what makes the coordinator pattern work at both local and distributed
/// levels. merge(merge(A, B), C) == merge(A, merge(B, C)).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateMetrics {
    pub latency_histogram: EncodedHistogram,
    pub total_requests: u64,
    pub total_errors: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub co_adjusted_count: u64,
    pub window_start: Instant,
    pub window_end: Instant,
    /// Number of sources merged into this aggregate
    pub source_count: usize,
}

impl AggregateMetrics {
    /// Merge another aggregate into this one.
    /// Used by local coordinator (merge per-core snapshots) and by
    /// distributed coordinator (merge per-node aggregates).
    pub fn merge(&mut self, other: &MetricsSnapshot) {
        self.latency_histogram.merge(&other.latency_histogram);
        self.total_requests += other.total_requests;
        self.total_errors += other.total_errors;
        self.bytes_sent += other.bytes_sent;
        self.bytes_received += other.bytes_received;
        self.co_adjusted_count += other.co_adjusted_count;
        self.window_start = self.window_start.min(other.window_start);
        self.window_end = self.window_end.max(other.window_end);
        self.source_count += 1;
    }

    /// Derived: compute percentiles from the merged histogram
    pub fn latency_p50(&self) -> Duration { /* read from histogram */ }
    pub fn latency_p99(&self) -> Duration { /* read from histogram */ }

    /// Derived: compute rates from counters and window
    pub fn request_rate(&self) -> f64 { /* total_requests / window_duration */ }
    pub fn error_rate(&self) -> f64 { /* total_errors / total_requests */ }
}

/// Rate controller output.
#[derive(Debug, Clone)]
pub struct RateDecision {
    pub target_rps: f64,
    pub next_update_interval: Duration,
}
```

### 5.3 Configuration Types

```rust
/// Top-level test configuration. Serializable for distributed transmission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    /// Target URL(s)
    pub targets: Vec<String>,
    /// Test duration
    pub duration: Duration,
    /// Rate configuration
    pub rate: RateConfig,
    /// Number of worker cores (0 = auto-detect)
    pub num_cores: usize,
    /// Connection settings
    pub connections: ConnectionConfig,
    /// Metrics reporting interval
    pub metrics_interval: Duration,
    /// Coordinator control loop interval
    pub control_interval: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RateConfig {
    Static { rps: f64 },
    Step { steps: Vec<(Duration, f64)> },
    Pid { initial_rps: f64, target: PidTarget },
    External { endpoint: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidTarget {
    pub metric: TargetMetric,
    pub value: f64,
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TargetMetric {
    LatencyP50,
    LatencyP90,
    LatencyP99,
    ErrorRate,
}
```

### 5.4 Command and Handle Types

```rust
/// Commands sent from coordinator to worker.
#[derive(Debug, Clone)]
pub enum WorkerCommand {
    UpdateRate(f64),
    Stop,
}

/// Coordinator's view of a worker. Type-erased — coordinator doesn't
/// know the worker's generic parameters.
pub struct WorkerHandle {
    pub command_tx: flume::Sender<WorkerCommand>,
    pub metrics_rx: flume::Receiver<MetricsSnapshot>,
    pub thread: std::thread::JoinHandle<()>,
    pub core_id: usize,
}
```

## 6. Component Design

### 6.1 Worker

The worker is the hot-path engine. Fully monomorphized — all component types
known at compile time, zero virtual dispatch.

```rust
pub struct Worker<S, G, T, E, M>
where
    S: RequestScheduler,
    G: RequestGenerator,
    T: RequestTransformer,
    E: RequestExecutor,
    M: MetricsCollector,
{
    // Owned by scheduling loop — exclusive &mut self access
    scheduler: S,
    generator: G,

    // Shared with spawned request tasks via Rc
    transformer: Rc<T>,
    executor: Rc<E>,
    metrics: Rc<M>,

    // Channels to coordinator
    command_rx: flume::Receiver<WorkerCommand>,
    metrics_tx: flume::Sender<MetricsSnapshot>,

    core_id: usize,
    config: TestConfig,
}
```

**Run loop:**

```rust
impl<S, G, T, E, M> Worker<S, G, T, E, M>
where
    S: RequestScheduler,
    G: RequestGenerator,
    T: RequestTransformer,
    E: RequestExecutor,
    M: MetricsCollector,
{
    pub async fn run(mut self) {
        let mut request_seq: u64 = 0;

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

            // 3. Precision sleep (runtime timer + spin for final microseconds)
            precision_sleep_until(intended_time).await;

            // 4. Build context
            let context = RequestContext {
                request_id: self.core_id as u64 * MAX_REQUESTS_PER_CORE + request_seq,
                intended_time,
                actual_time: Instant::now(),
                core_id: self.core_id,
                is_sampled: false, // sampling logic here
                session_id: None,  // session logic here
            };
            request_seq += 1;

            // 5. Generate and transform (synchronous, on the scheduling thread)
            let spec = self.generator.generate(&context);
            let spec = self.transformer.transform(spec, &context);

            // 6. Spawn execution as concurrent task on this core
            let executor = self.executor.clone();   // Rc clone = pointer bump
            let metrics = self.metrics.clone();      // Rc clone = pointer bump
            compio::runtime::spawn(async move {
                let result = executor.execute(&spec, &context).await;
                metrics.record(&result);
            })
            .detach();

            // 7. Periodic metrics reporting
            if self.should_report() {
                let snapshot = self.metrics.snapshot();
                let _ = self.metrics_tx.try_send(snapshot);
            }
        }
    }
}
```

### 6.2 Coordinator

The coordinator runs on its own thread. It is a synchronous control loop — no
async runtime needed. The loop is factored into a `tick()` method so the
distributed layer can wrap it.

```rust
pub struct Coordinator<R: RateController> {
    rate_controller: R,
    workers: Vec<WorkerHandle>,
    aggregate: AggregateMetrics,
    config: TestConfig,
}

impl<R: RateController> Coordinator<R> {
    /// Run the full coordinator loop. Blocks until test completes.
    pub fn run(&mut self) -> TestResult {
        // Initial rate distribution
        self.distribute_rate(self.config.initial_rps());

        loop {
            std::thread::sleep(self.config.control_interval);
            let decision = self.tick();

            if self.is_test_complete() {
                self.stop_workers();
                break;
            }
        }

        self.collect_final_metrics()
    }

    /// Single control loop iteration. Returns the rate decision.
    /// Exposed publicly so DistributedCoordinator can call it and
    /// also forward metrics/decisions over the network.
    pub fn tick(&mut self) -> RateDecision {
        // Collect metrics from all workers
        self.aggregate.reset();
        for worker in &self.workers {
            while let Ok(snapshot) = worker.metrics_rx.try_recv() {
                self.aggregate.merge(&snapshot);
            }
        }

        // Rate controller computes new target
        let decision = self.rate_controller.update(&self.aggregate);

        // Distribute to workers
        self.distribute_rate(decision.target_rps);

        decision
    }

    /// Get current aggregate metrics (for distributed layer to forward)
    pub fn aggregate_metrics(&self) -> &AggregateMetrics {
        &self.aggregate
    }

    fn distribute_rate(&self, total_rps: f64) {
        let per_core = total_rps / self.workers.len() as f64;
        for w in &self.workers {
            let _ = w.command_tx.send(WorkerCommand::UpdateRate(per_core));
        }
    }

    fn stop_workers(&self) {
        for w in &self.workers {
            let _ = w.command_tx.send(WorkerCommand::Stop);
        }
    }
}
```

### 6.3 Precision Scheduling

Neither compio nor any async runtime provides microsecond timer precision out of
the box. For high-rate tests (>10K rps per core), we use an adaptive strategy:

```rust
/// Sleep until `target`, using the runtime timer for the coarse portion
/// and busy-spinning for the final microseconds.
async fn precision_sleep_until(target: Instant) {
    let now = Instant::now();
    if target <= now {
        return;
    }

    let remaining = target - now;
    if remaining > SPIN_THRESHOLD {
        // Use runtime timer for the coarse sleep (saves CPU)
        compio::time::sleep(remaining - SPIN_THRESHOLD).await;
    }

    // Busy-spin for the final stretch (maximum precision)
    while Instant::now() < target {
        std::hint::spin_loop();
    }
}

/// Threshold below which we switch from timer sleep to busy-spin.
/// Tunable — 1ms is a good default. Lower = more CPU usage, higher precision.
const SPIN_THRESHOLD: Duration = Duration::from_millis(1);
```

For most use cases, the runtime timer alone is sufficient. The spin threshold
can be set to zero (no busy-spin) for lower-rate tests where microsecond
precision doesn't matter.

## 7. Crate Structure

### 7.1 Initial Crates (5)

```
netanvil-rs/
├── Cargo.toml                  (workspace)
├── crates/
│   ├── netanvil-types/           Traits + data types. Zero external dependencies.
│   │   └── src/lib.rs          ~300 lines
│   │
│   ├── netanvil-core/            Worker, Coordinator, schedulers, rate controllers.
│   │   └── src/                Depends on: compio, flume, netanvil-types
│   │       ├── lib.rs
│   │       ├── worker.rs       Worker struct and run loop
│   │       ├── coordinator.rs  Coordinator struct and tick loop
│   │       ├── scheduler/      ConstantRateScheduler, PoissonScheduler
│   │       ├── controller/     StaticRate, StepRate, PidRate controllers
│   │       ├── generator/      SimpleGenerator, TemplatedGenerator
│   │       ├── transformer/    HeaderTransformer, AuthTransformer
│   │       └── precision.rs    precision_sleep_until, adaptive timer
│   │
│   ├── netanvil-http/            HTTP executor using compio + cyper-core + hyper.
│   │   └── src/                Depends on: compio, hyper, cyper-core, rustls, netanvil-types
│   │       ├── lib.rs
│   │       ├── executor.rs     HttpExecutor impl
│   │       ├── pool.rs         Per-core connection pool (Rc-based)
│   │       └── tls.rs          TLS configuration
│   │
│   ├── netanvil-metrics/         HDR histogram-based metrics collection.
│   │   └── src/                Depends on: hdrhistogram, netanvil-types
│   │       ├── lib.rs
│   │       ├── collector.rs    HdrMetricsCollector (RefCell<Histogram>)
│   │       ├── aggregate.rs    AggregateMetrics merge logic
│   │       └── encoding.rs     Histogram serialization for snapshots
│   │
│   └── netanvil-cli/             Command-line interface.
│       └── src/                Depends on: clap, netanvil-core, netanvil-http, netanvil-metrics
│           └── main.rs         Argument parsing, component construction, test execution
│
├── docs/
│   ├── architecture.md         THIS FILE — authoritative design reference
│   └── archive/                Prior design documents (historical reference)
│
└── README.md
```

### 7.2 Future Crates (when needed)

```
netanvil-distributed/     DistributedCoordinator, gossip, leader election, CRDTs
                        Depends on: netanvil-core, netanvil-types, crdts, tonic
netanvil-api/             gRPC/REST server for external control and monitoring
                        Depends on: netanvil-core, netanvil-types, tonic, warp
netanvil-client/          Rust SDK for talking to the API (uses tokio, not compio)
                        Depends on: netanvil-types, tonic, tokio
```

These are added when the single-node implementation is working and tested.

## 8. Extension Points

Every advanced feature is an implementation of an existing trait, a decorator
around an existing trait, or a new coordinator layer. No new abstractions needed.

| Feature | Extension mechanism | Trait / component |
|---------|-------------------|-------------------|
| Constant rate | `impl RateController` | `StaticRateController` |
| Step function | `impl RateController` | `StepRateController` |
| PID control | `impl RateController` | `PidRateController` |
| External rate | `impl RateController` (reads from channel) | `ExternalRateController` |
| Poisson arrivals | `impl RequestScheduler` | `PoissonScheduler` |
| Burst patterns | `impl RequestScheduler` | `BurstScheduler` |
| Session simulation | `impl RequestGenerator` | `SessionAwareGenerator` |
| URL templates | `impl RequestGenerator` | `TemplatedGenerator` |
| Auth headers | `impl RequestTransformer` | `AuthTransformer` |
| Cookie jar | `impl RequestTransformer` | `CookieTransformer` |
| eBPF profiling | Decorator on `RequestExecutor` | `ProfilingExecutor<E>` |
| Network sim | Decorator on `RequestExecutor` | `ThrottledExecutor<E>` |
| gRPC testing | `impl RequestExecutor` | `GrpcExecutor` (future) |
| WebSocket | `impl RequestExecutor` | `WsExecutor` (future) |
| Distributed | New coordinator layer | `DistributedCoordinator` |
| Weighted distribution | Strategy in coordinator | `RateDistributor` trait |
| Prometheus export | Reads `AggregateMetrics` | Background thread |
| Live dashboard | Reads `AggregateMetrics` | WebSocket stream |

## 9. Distributed Integration

The distributed system extends the single-node design without modifying any core
types or traits. The coordinator pattern is recursive: a distributed coordinator
wraps local coordinators the same way local coordinators wrap workers.

### 9.1 Architecture

```
Single-node:
  Coordinator → [Worker₀, Worker₁, ..., Workerₙ]

Distributed:
  Leader (DistributedCoordinator)
    ├→ Node A (Coordinator → [Worker₀, Worker₁])
    ├→ Node B (Coordinator → [Worker₀, Worker₁])
    └→ Node C (Coordinator → [Worker₀, Worker₁])
```

### 9.2 How It Works

**On the leader node:**

```rust
struct DistributedCoordinator<R: RateController> {
    // The leader's own local coordinator (for workers on this node)
    local: Coordinator<ExternalRateController>,

    // Global rate controller
    rate_controller: R,

    // Remote nodes
    nodes: Vec<NodeHandle>,

    // Cluster state (CRDTs) — node registry, load assignments, epochs
    cluster_state: DistributedState,

    // Communication
    gossip: GossipEngine,
    election: ElectionState,
}
```

The `DistributedCoordinator` runs a loop that:
1. Calls `local.tick()` to handle local workers
2. Collects metrics from remote nodes (via gRPC/gossip)
3. Merges all metrics (local aggregate + per-node aggregates) into a global aggregate
4. Runs the global `rate_controller.update(&global_aggregate)`
5. Distributes per-node rate targets (weighted by capacity)
6. Sends the local node's share to `local` via its `ExternalRateController`

**On a worker node:**

The node runs a standard `Coordinator<ExternalRateController>`. The
`ExternalRateController` receives rate targets from the leader over the network.
The node periodically sends its aggregate metrics to the leader.

From the local coordinator's perspective, this is indistinguishable from
single-node mode with an external rate source. No code changes.

### 9.3 CRDT State Management

CRDTs manage cluster-level state that must converge across nodes without
coordination:

| CRDT type | What it stores | Used by |
|-----------|---------------|---------|
| Orswot (OR-Set) | Active node registry | DistributedCoordinator |
| LWWReg (Last-Write-Wins) | Per-node rate assignments | DistributedCoordinator |
| LWWReg | Shared test parameters | DistributedCoordinator |
| GCounter | Global request counter | Metrics reporting |
| Custom (Map + Dot) | Epoch log (leader changes) | Election, fencing |

**CRDTs never touch the hot path.** They are consumed by the
`DistributedCoordinator` to make decisions. Those decisions are expressed as
simple rate targets sent to local coordinators via channels. The per-core workers
never see CRDT state.

### 9.4 Integration Points with Single-Node Design

| Integration point | How it connects | Type changes needed? |
|---|---|---|
| Rate input to local coordinator | `ExternalRateController` impl | None — new impl of existing trait |
| Metrics output from local coordinator | `coordinator.aggregate_metrics()` | None — `tick()` already exposes this |
| Test config distribution | Serialize `TestConfig`, send to nodes | None — `TestConfig` already derives Serialize |
| Metrics aggregation (node → leader) | `AggregateMetrics::merge()` | None — merge is already composable |
| Failure detection → rate redistribution | Leader adjusts rate distribution | None — same as any rate update |
| Node join → capacity update | Leader adds to node list, redistributes | None — coordinator pattern handles this |
| Leader election → new coordinator | New leader constructs DistributedCoordinator | None — construction, not modification |

### 9.5 Core Type Compatibility Audit

Every core type was evaluated for distributed compatibility:

| Type | Distributed-safe? | Notes |
|------|-------------------|-------|
| `RequestSpec` | Yes | Never crosses node boundary |
| `RequestContext` | Yes | Add optional `node_id` later if needed |
| `ExecutionResult` | Yes | Never crosses node boundary |
| `TimingBreakdown` | Yes | Never crosses node boundary |
| `MetricsSnapshot` | Yes | Serializable, mergeable |
| `AggregateMetrics` | Yes | Merge is associative + commutative |
| `RateDecision` | Yes | Simple value type |
| `TestConfig` | Yes | Serializable |
| `WorkerCommand` | Yes | Local only — distributed uses its own protocol |

**No core type breakages.** The distributed layer is purely additive.

### 9.6 What the Distributed Crate Adds (New Types Only)

```rust
// netanvil-distributed/src/

// New types — none of these exist in netanvil-types
pub struct NodeHandle { /* network connection to a remote node */ }
pub struct DistributedState { /* CRDTs: Orswot, LWWReg, GCounter */ }
pub struct GossipEngine { /* UDP gossip protocol */ }
pub struct ElectionState { /* Raft-like leader election */ }
pub struct FencingToken(u64);
pub struct NodeCapacity { pub cores: usize, pub max_rps: f64 }

// New trait for distributing rate across nodes
pub trait RateDistributor {
    fn distribute(&self, total_rps: f64, nodes: &[NodeCapacity]) -> Vec<f64>;
}

// Implementations
pub struct EvenDistributor;          // total / N
pub struct WeightedDistributor;      // proportional to capacity
pub struct SaturationAwareDistributor; // avoid overloaded nodes
```

## 10. Key Design Decisions Summary

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Runtime | compio | Active development, hyper bridge, io_uring, cross-platform |
| Threading model | Thread-per-core, shared-nothing | Linear scaling, no locks on hot path |
| Trait bounds | `!Send` on hot path | Matches thread-per-core model; Rc, not Arc |
| Control plane | Synchronous coordinator on own thread | Simple, no async needed for ~10Hz loop |
| Profiling | Decorator pattern | Zero cost when unused, composable, no supertrait |
| Metrics merge | Associative + commutative | Same merge works for cores and nodes |
| Type erasure | At channel boundary (WorkerHandle) | Coordinator doesn't know worker generics |
| Static dispatch | Workers fully monomorphized | Zero virtual dispatch on hot path |
| Rate controller | Trait object OK in coordinator | Called at ~10Hz, virtual dispatch irrelevant |
| HTTP engine | hyper via cyper-core bridge | Battle-tested, HTTP/1.1 + HTTP/2 |
| Crate count | 5 (not 20) | Split later when justified by real code |
| Distributed | Additive layer, same patterns | No core type changes needed |
| CRDTs | Control plane only | Never on hot path, cluster management state |
| Coordinated omission | intended_time vs actual_time | Measured in every request context |
| Timer precision | Runtime timer + spin for final μs | Adaptive, tunable spin threshold |

## 11. Open Questions and Risks

1. **cyper-core maturity.** The compio-to-hyper bridge is pre-1.0. Need to
   validate: connection pooling, HTTP/2 multiplexing, TLS session resumption,
   and behavior under high concurrency. Fallback: build a thin HTTP client
   directly on compio-net with manual HTTP/1.1 framing.

2. **compio timer precision under load.** Need to benchmark the actual
   achievable timer precision when io_uring is handling thousands of concurrent
   connections. The spin threshold may need core-specific tuning.

3. **HDR histogram merge performance.** At high core counts (32+), periodic
   histogram merges in the coordinator could become a bottleneck. May need
   to use pre-aggregated counters for the rate controller and only merge
   full histograms for final reporting.

4. **flume channel backpressure.** If a worker produces metrics snapshots faster
   than the coordinator consumes them, the channel grows. Use bounded channels
   with `try_send` (drop old snapshots) — metrics are snapshots, not events.

5. **Distributed clock correlation.** Cross-node latency comparison requires
   synchronized clocks (NTP/PTP). This is an operational concern, not a design
   concern, but should be documented for users.
