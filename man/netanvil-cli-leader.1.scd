netanvil-cli-leader(1)

# NAME

netanvil-cli-leader - run a one-shot distributed netanvil test leader

# SYNOPSIS

*netanvil-cli* *leader* _--workers HOST:PORT,..._ _--url URL_ [_OPTIONS_]

# DESCRIPTION

Coordinates a one-shot distributed load test across a fleet of agent nodes
(see *netanvil-cli-agent*(1)). The leader partitions the target RPS across
agents, pushes an identical test configuration to each, collects per-agent
metrics, and prints an aggregated summary when the test completes.

Rate control decisions are made centrally by the leader and broadcast to
agents each tick, so *adaptive* and *pid* modes operate on the aggregate
system response rather than per-agent.

For a long-running daemon with an HTTP API and test queue, see
*netanvil-cli-leader-server*(1).

# REQUIRED OPTIONS

*--workers* _HOSTS_
	Comma-separated list of agent addresses (_host:port_). At least one is
	required. Example: _agent1:9090,agent2:9090,agent3:9090_.

*--url* _URL_ (repeatable)
	Target URL(s). Scheme selects the protocol -- see
	*netanvil-protocols*(7).

# PLUGIN OPTIONS

*--plugin* _PATH_
	Path to a plugin script (_.lua_, _.js_, _.wasm_). The plugin is read
	locally and pushed to every agent before the test starts.

*--plugin-type* _TYPE_
	_hybrid_, _lua_, _wasm_, _js_, or _auto_. Default: _auto_.

# TEST DURATION AND RATE

*--rps* _N_
	Target requests per second, *total across all agents*. Each agent gets
	_rps / len(workers)_. Default: _100_.

*--duration* _DURATION_
	Test duration. Default: _10s_.

*--rate-mode* _MODE_
	_static_, _step_, _pid_, _ramp_, _adaptive_. See
	*netanvil-rate-control*(7).

Rate-mode flags are documented in full in *netanvil-rate-control*(7):

	*--steps*, *--pid-metric*, *--pid-target*, *--pid-kp*, *--pid-ki*,
	*--pid-kd*, *--pid-min-rps*, *--pid-max-rps*, *--pid-constraint*,
	*--pid-autotune-duration*, *--ramp-warmup*, *--ramp-multiplier*,
	*--ramp-max-errors*, *--baseline-floor*, *--latency-limit*,
	*--error-rate-limit*, *--latency-setpoint*, *--rate-config*,
	*--response-signal*, *--control-trace*

# WORKER AND TIMEOUT OPTIONS

*--cores* _N_
	Number of I/O worker cores *per agent*. _0_ = auto-detect on each
	agent. Default: _0_.

*--timeout* _DURATION_
	Per-request timeout. Default: _30s_.

# PROTOCOL OPTIONS

Protocol-specific flags are documented in *netanvil-protocols*(7). Summary:

	*HTTP*: *--method*, *-H*/*--header*, *--http-version*, *--error-threshold*

	*TCP/UDP*: *--payload*, *--payload-hex*, *--payload-file*, *--framing*,
	*--delimiter*, *--no-response*, *--mode*, *--request-size*,
	*--response-size*, *--chunk-size*

	*DNS*: *--dns-domains*, *--dns-query-type*

# EXTERNAL METRICS

For PID/adaptive mode, the leader can poll an external metrics endpoint on
the target and feed a JSON field into the controller as a constraint input.

*--external-metrics-url* _URL_
	URL of a JSON endpoint on the target system. Polled each coordinator
	tick.

*--external-metrics-field* _FIELD_
	JSON field to extract from the response. Feed it into a constraint via
	*--pid-constraint* or *--pid-metric external:...*.

# TLS OPTIONS

mTLS is enabled when all three of *--tls-ca*, *--tls-cert*, and *--tls-key*
are provided. Both the leader and every agent must be configured consistently.

*--tls-ca* _PATH_
	CA certificate PEM used to verify agent certificates.

*--tls-cert* _PATH_
	Leader's certificate PEM.

*--tls-key* _PATH_
	Leader's private key PEM.

# OBSERVABILITY OPTIONS

*--output* _FORMAT_
	_text_ (default) or _json_.

*--metrics-port* _PORT_
	Port for the leader's aggregated Prometheus metrics endpoint. Serves
	_/metrics/prometheus_ with cluster-level aggregates so a single scrape
	target covers the whole test.

*--health-sample-rate* _FRACTION_
	Fraction (0.0-1.0) of TCP reads on each agent to sample for
	SO_INCOMING_CPU observation. Requires kernel >= 6.7 on agents.
	Default: _0.0_.

*--control-trace* _FILE_
	Record per-tick controller decisions to a JSONL file. See
	*netanvil-rate-control*(7).

# EXAMPLES

Distributed static test across three agents:

	netanvil-cli leader --workers agent1:9090,agent2:9090,agent3:9090 --url http://target:8080/ --rps 15000 --duration 5m

Distributed adaptive test with latency ceiling, TLS enabled:

	netanvil-cli leader --workers agent1:9090,agent2:9090 --url http://target:8080/ --rate-mode adaptive --latency-limit 300 --tls-ca /etc/netanvil/ca.pem --tls-cert /etc/netanvil/leader.pem --tls-key /etc/netanvil/leader.key --duration 10m

PID against an external signal exposed by the target:

	netanvil-cli leader --workers agent1:9090,agent2:9090 --url http://target:8080/ --rate-mode pid --pid-metric external:cpu --pid-target 70 --external-metrics-url http://target:9090/load.json --external-metrics-field cpu --duration 5m

Leader aggregated metrics for Prometheus:

	netanvil-cli leader --workers agent1:9090,agent2:9090 --url http://target:8080/ --metrics-port 9500 --duration 1h

# SEE ALSO

*netanvil-cli*(1), *netanvil-cli-test*(1), *netanvil-cli-agent*(1),
*netanvil-cli-leader-server*(1), *netanvil-rate-control*(7),
*netanvil-protocols*(7)
