# NetAnvil-RS

A high-performance network load testing framework built in Rust, featuring
thread-per-core architecture, io_uring-based I/O, coordinated omission
prevention, and a design that naturally extends to distributed multi-node
testing.

## Features

- **Thread-per-core** — Each CPU core runs an independent worker with its own
  scheduler, connection pool, and metrics. No locks on the hot path.
- **io_uring I/O** — Completion-based async I/O via compio for maximum
  connection throughput on Linux.
- **Coordinated omission prevention** — Every request tracks intended vs actual
  send time, ensuring latency measurements reflect real user experience.
- **Accurate percentiles** — HDR histograms for precise latency recording.
- **Pluggable rate control** — Static, step function, PID feedback loop, or
  external rate sources.
- **HTTP/1.1 and HTTP/2** — Via the battle-tested hyper HTTP engine.

## Architecture

```
Coordinator (control loop, ~10-100Hz)
  ├── RateController
  └── WorkerHandles (channels)
        │
        ├── Worker 0 (core 0, compio + io_uring)
        ├── Worker 1 (core 1, compio + io_uring)
        └── Worker N (core N, compio + io_uring)
```

See [docs/architecture.md](docs/architecture.md) for the full design.

## Crates

| Crate | Purpose |
|-------|---------|
| `netanvil-types` | Core traits and data types |
| `netanvil-core` | Worker, coordinator, schedulers, rate controllers |
| `netanvil-http` | HTTP executor (compio + hyper) |
| `netanvil-metrics` | HDR histogram metrics collection |
| `netanvil-cli` | Command-line interface |

## Prerequisites

- Rust 1.75+
- Linux kernel 5.6+ (for io_uring; macOS/Windows supported via fallback drivers)

## Building

```bash
cargo build --release
```

## Usage

```bash
cargo run --bin netanvil-cli -- test \
  --url https://example.com \
  --rps 1000 \
  --duration 60s
```

## Development Status

Under active development. The single-node load testing pipeline is being built
first. The architecture is designed to extend to distributed multi-node testing
without modifying core types — see the distributed integration section in
[docs/architecture.md](docs/architecture.md).

## License

Licensed under either of:
- Apache License, Version 2.0
- MIT license

at your option.
