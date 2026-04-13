# NetAnvil

NetAnvil is a network load testing tool written in Rust. It generates
controlled request traffic against HTTP, TCP, UDP, DNS, and Redis targets,
measures latency with HDR histograms, and reports percentile distributions.

NetAnvil uses a thread-per-core architecture where each CPU core runs an
independent I/O worker with its own scheduler, connection pool, and metrics
collector. Workers share no mutable state and require no locks on the request
path. On Linux, I/O is completion-based via io_uring. A dedicated timer thread
with a spin-loop scheduler dispatches requests at microsecond precision,
preventing coordinated omission — every request records its intended send time
alongside its actual send time, so latency percentiles reflect queuing delays
rather than hiding them.

Rate control ranges from a fixed RPS to an adaptive feedback controller that
adjusts load based on observed latency and error rates. A PID controller, AIMD
back-off, and constraint-based policies can be composed to find the maximum
sustainable throughput for a given latency or error budget. Tests can also be
scripted with Lua, JavaScript (V8), Rhai, or compiled WASM modules for
per-request generation logic.

Multiple nodes can coordinate as a distributed test cluster, with a leader
aggregating metrics from remote agents over HTTP. See [Distributed
Mode](#distributed-mode) below.

## Building

Requires Rust 1.75+ and Linux kernel 5.6+ (for io_uring).

```
cargo build --release
```

## Quick start

```
netanvil-cli test --url https://example.com --rps 1000 --duration 30s
```

For adaptive rate control (find max RPS under a latency constraint):

```
netanvil-cli test --url https://example.com \
  --rate-mode adaptive --latency-limit 200 --duration 60s
```

See `netanvil-cli test --help` for the full option set.

## Distributed Mode

NetAnvil can scale a load test across multiple machines. A **leader** node
orchestrates one or more **agent** nodes, distributing target RPS in proportion
to each agent's CPU core count and aggregating metrics centrally.

### Running agents

Start an agent on each worker machine:

```
netanvil-cli agent --listen 0.0.0.0:9090
```

Agents expose an HTTP control API and wait for instructions from the leader.
Use `--node-id` to assign a human-readable name (defaults to `hostname:port`),
and `--cores N` to limit the number of I/O worker cores (0 = auto-detect).

### Running a one-shot test from the leader

```
netanvil-cli leader \
  --workers 10.0.0.2:9090,10.0.0.3:9090 \
  --url https://target.example.com \
  --rps 5000 --duration 60s
```

The leader discovers agents, pushes the test configuration, and prints
aggregated results when the test finishes. All rate-control modes (static,
step, pid, ramp, adaptive) work the same as in single-node mode — the leader
runs the controller and distributes the resulting rate across agents each tick.

### Running as a daemon (server mode)

For repeated or API-driven tests, run the leader as a long-lived server:

```
netanvil-cli server --workers 10.0.0.2:9090,10.0.0.3:9090
```

Submit tests via the HTTP API:

```
curl -X POST http://leader:8080/tests \
  -H 'Content-Type: application/json' \
  -d '{"targets":["https://target.example.com"],"duration":"60s","rate":{"mode":"static","rps":5000}}'
```

The server queues tests and runs them sequentially. While a test is running you
can poll progress, push external signals, freeze/release the rate, or update
controller parameters through the REST API. See `GET /tests/{id}/metrics` for
live per-node breakdowns.

Key API endpoints:

| Endpoint | Method | Description |
|---|---|---|
| `/tests` | POST | Enqueue a new test |
| `/tests` | GET | List tests (filter by status) |
| `/tests/{id}` | GET | Test info and live progress |
| `/tests/{id}` | DELETE | Cancel a running test |
| `/tests/{id}/result` | GET | Final aggregated result |
| `/tests/{id}/hold` | PUT | Freeze rate at a given RPS |
| `/tests/{id}/hold` | DELETE | Resume dynamic rate control |
| `/tests/{id}/signal` | PUT | Push an external signal value |
| `/agents` | GET | List discovered agents |
| `/health` | GET | Health check |

### mTLS

For production deployments, agent–leader communication can be secured with
mutual TLS:

```
# Agent
netanvil-cli agent --listen 0.0.0.0:9090 \
  --tls-ca ca.pem --tls-cert agent.pem --tls-key agent-key.pem

# Leader
netanvil-cli leader --workers 10.0.0.2:9090 \
  --tls-ca ca.pem --tls-cert leader.pem --tls-key leader-key.pem \
  --url https://target.example.com --rps 1000 --duration 30s
```

### Rate distribution

Each control-loop tick the leader computes a new target RPS (via the configured
rate controller) and splits it across agents weighted by core count. For
example, three agents with 4, 8, and 4 cores receiving a total target of
1000 RPS would get 250, 500, and 250 RPS respectively.

If an agent becomes unreachable it is marked failed and excluded from future
ticks. The remaining agents absorb its share of the load.

## Documentation

- [Architecture](docs/architecture.md) — system design, crate structure, and
  extension points
- [Plugins](docs/plugins.md) — Lua, WASM, Rhai, and JavaScript plugin system

## License

MIT
