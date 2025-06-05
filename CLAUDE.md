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
- Each worker core runs its own compio runtime with its own io_uring instance.
- HTTP uses hyper via cyper-core bridge (not tokio).

### System Structure

```
Coordinator (own thread, synchronous ~10-100Hz loop)
  ├── RateController (static, PID, step, external)
  ├── AggregateMetrics (merged from per-core snapshots)
  └── WorkerHandles (flume channels to each core)

Worker (per-core, compio runtime, !Send)
  ├── RequestScheduler (computes next send time)
  ├── RequestGenerator (creates RequestSpec)
  ├── RequestTransformer (modifies RequestSpec)
  ├── RequestExecutor (HTTP client, Rc-shared with spawned tasks)
  └── MetricsCollector (HDR histogram, Rc-shared with spawned tasks)
```

### Crate Structure (5 crates)

- `netanvil-types` — Traits + data types. Zero dependencies.
- `netanvil-core` — Worker, Coordinator, schedulers, rate controllers. Depends on compio.
- `netanvil-http` — HTTP executor (cyper-core + hyper). Depends on compio.
- `netanvil-metrics` — HDR histogram metrics collector. Depends on hdrhistogram.
- `netanvil-cli` — CLI entry point. Depends on clap.

### Core Traits (in netanvil-types)

Hot-path traits (per-core, `!Send`):
- `RequestScheduler` — `fn next_request_time(&mut self) -> Option<Instant>`
- `RequestGenerator` — `fn generate(&mut self, ctx) -> RequestSpec`
- `RequestTransformer` — `fn transform(&self, spec, ctx) -> RequestSpec`
- `RequestExecutor` — `async fn execute(&self, spec, ctx) -> ExecutionResult`
- `MetricsCollector` — `fn record(&self, result)` + `fn snapshot(&self) -> MetricsSnapshot`

Control-plane trait:
- `RateController` — `fn update(&mut self, metrics) -> RateDecision`

### Key Design Principles

1. **Shared-nothing workers.** All hot-path state is duplicated per core.
2. **No `Send + Sync` on hot-path traits.** Thread-per-core means `Rc`, not `Arc`.
3. **Profiling via decorators,** not supertraits. Wrap `RequestExecutor` with `ProfilingExecutor<E>`.
4. **Coordinated omission prevention.** Every `RequestContext` has `intended_time` and `actual_time`.
5. **Composable metrics.** `AggregateMetrics::merge()` is associative+commutative — works for both local core aggregation and future distributed node aggregation.

### Extension Pattern

Every advanced feature is a new trait implementation or a decorator:
- PID control → `impl RateController`
- Session simulation → `impl RequestGenerator`
- eBPF profiling → decorator wrapping `RequestExecutor`
- Distributed mode → `DistributedCoordinator` wrapping `Coordinator` (future crate)

### Testing

- Unit tests colocated with implementation
- Async tests use compio's test support (not tokio::test)
- Manual mock implementations for traits (no mockall needed — traits are simple)
- Integration tests in `tests/` directory of each crate

### Error Handling

- `thiserror` for error definitions
- `Result` propagation throughout
- `tracing` for logging
