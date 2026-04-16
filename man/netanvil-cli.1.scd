netanvil-cli(1)

# NAME

netanvil-cli - high-performance network load testing

# SYNOPSIS

*netanvil-cli* [_--debug_] _COMMAND_ [_OPTIONS_]

# DESCRIPTION

*netanvil-cli* is a high-performance network load generator built on a
thread-per-core architecture backed by io_uring via the compio async runtime.
It supports HTTP/1.1, HTTP/2, TCP, UDP, DNS, and Redis targets, with pluggable
rate control (static, step, PID, ramp, adaptive constraint arbitration), plugin
runtimes (Lua, WASM, JavaScript), and distributed execution across multiple
agent nodes.

Four operational modes are exposed as subcommands:

- *test* runs a single-node load test from the local machine.
- *agent* runs the node as a remotely controllable worker for a distributed test.
- *leader* drives a one-shot distributed test across a fleet of agents.
- *leader-server* runs a persistent leader daemon with an HTTP API and result
  queue, suitable for deployment as a systemd service.

# COMMANDS

*test*
	Run a single-node load test. See *netanvil-cli-test*(1).

*agent*
	Run as a remotely controllable agent node. See *netanvil-cli-agent*(1).

*leader*
	Coordinate a one-shot distributed test across agents.
	See *netanvil-cli-leader*(1).

*leader-server*
	Run a persistent HTTP-API-driven leader daemon.
	See *netanvil-cli-leader-server*(1).

# GLOBAL OPTIONS

*--debug*
	Enable debug/trace logging. Equivalent to *RUST_LOG=debug*.

*-h*, *--help*
	Print help.

*-V*, *--version*
	Print version.

# ENVIRONMENT

*RUST_LOG*
	Controls log verbosity via the tracing-subscriber crate. Takes
	precedence over *--debug*. Examples: *RUST_LOG=info*,
	*RUST_LOG=netanvil_core=debug,netanvil_http=info*.

# EXIT STATUS

*0*
	Test completed successfully.

*1*
	Configuration error, I/O failure, or test aborted.

# CONCEPTS

The rate control system and protocol-specific flags are documented separately
since they are shared across *test*, *leader*, and (for rate control) the
*leader-server* HTTP API:

- *netanvil-rate-control*(7) covers static, step, PID, ramp, and adaptive
  rate modes, constraint arbitration, and PID autotuning.
- *netanvil-protocols*(7) covers URL scheme dispatch (http://, tcp://, udp://,
  dns://, redis://), payload and framing flags, and protocol-specific modes.

# EXAMPLES

Simple HTTP load test at 1000 RPS for 30 seconds:

	netanvil-cli test --url http://target:8080/ --rps 1000 --duration 30s

Adaptive test with a latency ceiling of 200ms:

	netanvil-cli test --url http://target:8080/ --rate-mode adaptive --latency-limit 200

Distributed test across three agents:

	netanvil-cli leader --workers agent1:9090,agent2:9090,agent3:9090 --url http://target:8080/ --rps 5000 --duration 1m

# SEE ALSO

*netanvil-cli-test*(1), *netanvil-cli-agent*(1), *netanvil-cli-leader*(1),
*netanvil-cli-leader-server*(1), *netanvil-rate-control*(7),
*netanvil-protocols*(7), *netanvil-test-server*(1)

Project: https://github.com/mtottenh/netanvil-rs

# AUTHORS

NetAnvil-RS Contributors.

# BUGS

Report bugs at https://github.com/mtottenh/netanvil-rs/issues.
