# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build and Development Commands

```bash
cargo build              # Build workspace
cargo build --release    # Optimized build
cargo test               # Run all tests
cargo test -p netanvil-core # Run tests for a specific crate
cargo check              # Type-check without building
cargo fmt                # Format code
cargo clippy             # Lint
cargo test test_name -- --exact     # Run a specific test
cargo test -- --nocapture           # Tests with stdout
cargo run --bin netanvil-cli -- [args] # Run CLI
```

## Architecture

**Read `docs/architecture.md` for the full design.** Key points:

### Runtime: compio (thread-per-core, io_uring)

- Hot-path code is `!Send`. Use `Rc<RefCell<...>>` for per-core state, NOT `Arc<Mutex<...>>`.
- Cross-core communication via `flume` channels only.
- Each I/O worker core runs its own compio runtime with its own io_uring instance.
- HTTP uses hyper via cyper bridge (not tokio).

### System Structure (N:M Timer Thread Model)

```
Coordinator (main thread, synchronous ~10-100Hz loop)
  ├── Box<dyn RateController> (static, PID, step, composite PID)
  ├── AggregateMetrics (merged from per-core snapshots)
  ├── SaturationDetection (backpressure, scheduling delays)
  └── TimerThreadHandle + IoWorkerHandles

Timer Thread (core 0, synchronous spin-loop, ~1us precision)
  ├── Scheduler[0..N] (one per I/O worker, min-heap dispatch)
  └── Bounded fire channels → I/O workers

I/O Worker (per-core, compio runtime, !Send)
  ├── RequestGenerator (creates HttpRequestSpec — native or plugin)
  ├── RequestTransformer (modifies HttpRequestSpec, Rc-shared)
  ├── RequestExecutor (HTTP client, Rc-shared with spawned tasks)
  └── MetricsCollector (HDR histogram, Rc-shared with spawned tasks)
```

### Crate Structure (10 crates)

- `netanvil-types` — Traits + data types. Zero runtime dependencies.
- `netanvil-metrics` — HDR histogram metrics collector. Depends on hdrhistogram.
- `netanvil-core` — Engine, Coordinator, timer thread, I/O workers, rate controllers. Depends on compio.
- `netanvil-http` — HTTP executor (cyper + hyper). Depends on compio.
- `netanvil-api` — Control API server (standalone + agent mode). Depends on tiny_http.
- `netanvil-distributed` — Distributed coordinator (HTTP-based multi-node). Depends on netanvil-api.
- `netanvil-plugin` — Plugin system (WASM, Rhai, Hybrid Lua). Depends on wasmtime, rhai, mlua.
- `netanvil-plugin-luajit` — LuaJIT per-request generator. Depends on mlua/luajit.
- `netanvil-plugin-lua54` — Lua 5.4 per-request generator. Depends on mlua/lua54. Not in default-members (link conflict with LuaJIT).
- `netanvil-cli` — CLI entry point. Depends on clap.

### Core Traits (in netanvil-types)

Hot-path traits (per-core, `!Send` except RequestScheduler):
- `RequestScheduler: Send` — `fn next_request_time(&mut self) -> Option<Instant>` (Send for timer thread)
- `RequestGenerator` — `fn generate(&mut self, ctx) -> HttpRequestSpec`
- `RequestTransformer` — `fn transform(&self, spec, ctx) -> HttpRequestSpec`
- `RequestExecutor` — `fn execute(&self, spec, ctx) -> impl Future<Output = ExecutionResult>`
- `MetricsCollector` — `fn record(&self, result)` + `fn snapshot(&self) -> MetricsSnapshot`

Control-plane trait:
- `RateController` — `fn update(&mut self, summary: &MetricsSummary) -> RateDecision`

### Key Design Principles

1. **Shared-nothing workers.** All hot-path state is duplicated per core.
2. **N:M timer thread model.** Dedicated timer thread (spin-loop) owns schedulers; I/O workers are pure executors.
3. **No `Send + Sync` on hot-path traits** (except `RequestScheduler` for timer thread). Thread-per-core means `Rc`, not `Arc`.
4. **Coordinated omission prevention.** Every `RequestContext` has `intended_time` and `actual_time`.
5. **Composable metrics.** `AggregateMetrics::merge()` is associative+commutative — works for both local core aggregation and distributed node aggregation.
6. **Plugin transparency.** Plugin generators implement `RequestGenerator` — I/O workers don't know or care if generation is native or scripted.

### Extension Pattern

Every advanced feature is a new trait implementation or a decorator:
- PID/composite PID control → `impl RateController`
- Plugin generators → `impl RequestGenerator` (WASM, Lua, Hybrid, Rhai)
- Connection policies → `impl RequestTransformer` (`ConnectionPolicyTransformer`)
- eBPF profiling → decorator wrapping `RequestExecutor`
- Distributed mode → `DistributedCoordinator` (HTTP-based, in netanvil-distributed)

### Testing

- Unit tests colocated with implementation
- Async tests use compio's test support (not tokio::test)
- Manual mock implementations for traits (no mockall needed — traits are simple)
- Integration tests in `tests/` directory of each crate

### Error Handling

- `thiserror` for error definitions
- `Result` propagation throughout
- `tracing` for logging
