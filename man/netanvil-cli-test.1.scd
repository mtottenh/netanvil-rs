netanvil-cli-test(1)

# NAME

netanvil-cli-test - run a single-node load test

# SYNOPSIS

*netanvil-cli* *test* [_OPTIONS_] _--url URL_ [_--url URL_ ...]++
*netanvil-cli* *test* [_OPTIONS_] _--plugin PATH_

# DESCRIPTION

Runs a load test from the local machine. Workers are spawned across I/O cores,
share nothing, and generate traffic against the target URL(s). Metrics are
collected via HDR histograms and emitted to stdout (text or JSON) at the end of
the test. Live metrics and mid-test rate changes can be exposed over an HTTP
control API (*--api-port*).

For distributed tests across multiple nodes, see *netanvil-cli-leader*(1).

# TARGET OPTIONS

*--url* _URL_ (repeatable)
	Target URL. Required unless *--plugin* is specified. Pass multiple
	*--url* arguments for round-robin across targets. Scheme selects the
	protocol executor -- see *netanvil-protocols*(7).

*--plugin* _PATH_
	Path to a plugin script (_.lua_, _.js_) or WASM module (_.wasm_). The
	plugin implements request generation logic. Mutually exclusive with
	*--url*.

*--plugin-type* _TYPE_
	_hybrid_ (Lua config -> native hot path), _lua_ (LuaJIT per-request),
	_wasm_ (WASM per-request), _js_ (V8 JavaScript; requires *v8* feature),
	or _auto_ (detect from extension). Default: _auto_.

# TEST DURATION AND RATE

*--rps* _N_
	Target requests per second (initial RPS for *pid*/*ramp*/*adaptive*
	modes). Default: _100_.

*--duration* _DURATION_
	Test duration. Accepts _10s_, _1m_, _500ms_. Default: _10s_.

*--rate-mode* _MODE_
	Rate control mode: _static_ (default), _step_, _pid_, _ramp_,
	_adaptive_. See *netanvil-rate-control*(7) for the full flag set.

*--scheduler* _DISCIPLINE_
	_constant_ (default) or _poisson_. See *netanvil-rate-control*(7).

The following rate-mode flags are documented in full in
*netanvil-rate-control*(7):

	*--steps*, *--pid-metric*, *--pid-target*, *--pid-kp*, *--pid-ki*,
	*--pid-kd*, *--pid-min-rps*, *--pid-max-rps*, *--pid-constraint*,
	*--pid-autotune-duration*, *--ramp-warmup*, *--ramp-multiplier*,
	*--ramp-max-errors*, *--baseline-floor*, *--latency-limit*,
	*--error-rate-limit*, *--latency-setpoint*, *--rate-config*,
	*--response-signal*, *--control-trace*

# WORKER AND TIMEOUT OPTIONS

*--cores* _N_
	Number of I/O worker cores. _0_ = auto-detect from available cores.
	Default: _0_.

*--timeout* _DURATION_
	Per-request timeout. Default: _30s_.

# PROTOCOL OPTIONS

Protocol-specific flags are documented in full in *netanvil-protocols*(7).
Summary:

	*HTTP*: *--method*, *-H*/*--header*, *--http-version*,
	*--connection-policy*, *--persistent-ratio*, *--conn-lifetime*,
	*--bandwidth*, *--error-threshold*

	*TCP/UDP*: *--payload*, *--payload-hex*, *--payload-file*, *--framing*,
	*--delimiter*, *--no-response*, *--mode*, *--request-size*,
	*--response-size*, *--chunk-size*, *--latency-us*, *--error-rate*

	*DNS*: *--dns-domains*, *--dns-query-type*

# OBSERVABILITY OPTIONS

*--output* _FORMAT_
	_text_ (default, human-readable) or _json_ (machine-readable to stdout
	at test end).

*--api-port* _PORT_
	Enable the HTTP control API on this port. Allows mid-test rate changes,
	target updates, plugin reloads, and live metrics polling via curl. See
	the *netanvil-api* crate for endpoint documentation.

*--event-log-dir* _DIR_
	Directory for per-request Arrow IPC event logs. Each I/O worker writes
	_events\_core\_N.arrow_ for post-test statistical analysis.

*--event-sample-rate* _FRACTION_
	Fraction (0.0-1.0) of requests to include in the event log. Default:
	_1.0_. Lower values reduce disk I/O at high RPS.

*--health-sample-rate* _FRACTION_
	Fraction (0.0-1.0) of TCP reads to piggyback with getsockopt for
	SO_INCOMING_CPU observation. Uses io_uring linked SQEs to atomically
	read the value after recv. Requires kernel >= 6.7. Default: _0.0_.

*--control-trace* _FILE_
	Record per-tick controller decisions to a JSONL file (adaptive mode
	only). See *netanvil-rate-control*(7).

# EXAMPLES

Simple HTTP test at 1000 RPS for 30 seconds:

	netanvil-cli test --url http://target:8080/ --rps 1000 --duration 30s

Poisson arrivals against two targets, keepalive disabled:

	netanvil-cli test --url http://a.target:8080/ --url http://b.target:8080/ --scheduler poisson --connection-policy always-new --rps 500 --duration 1m

Adaptive with a latency ceiling and error cap:

	netanvil-cli test --url http://target:8080/ --rate-mode adaptive --latency-limit 500 --error-rate-limit 1.0 --duration 5m

PID holding p99 at 150ms, traces written for analysis:

	netanvil-cli test --url http://target:8080/ --rate-mode pid --pid-metric latency-p99 --pid-target 150 --control-trace /tmp/trace.jsonl --duration 2m

TCP request-response against a netanvil test server:

	netanvil-cli test --url tcp://target:9000 --mode rr --request-size fixed:128 --response-size uniform:512,4096 --rps 2000 --duration 30s

Plugin-driven request generation with Lua:

	netanvil-cli test --plugin scripts/auth_flow.lua --rate-mode adaptive --latency-limit 300 --duration 1m

Live control API on port 8080 -- start the test in the background then poll:

	netanvil-cli test --url http://target:8080/ --api-port 8080 --duration 1h
	curl http://localhost:8080/metrics
	curl -X POST http://localhost:8080/rate -d '{"rps": 5000}'

# SEE ALSO

*netanvil-cli*(1), *netanvil-cli-leader*(1), *netanvil-rate-control*(7),
*netanvil-protocols*(7), *netanvil-test-server*(1)
