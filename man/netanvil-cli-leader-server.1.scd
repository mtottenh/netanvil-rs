netanvil-cli-leader-server(1)

# NAME

netanvil-cli-leader-server - run a persistent netanvil leader daemon

# SYNOPSIS

*netanvil-cli* *leader-server* _--workers HOST:PORT,..._ [_OPTIONS_]

# DESCRIPTION

Runs a long-lived leader daemon that exposes an HTTP API for queueing, starting,
and inspecting distributed tests. Unlike *netanvil-cli-leader*(1) which runs a
single test and exits, *leader-server* accepts test specifications over HTTP,
executes them in sequence against a fixed pool of agents, and persists results
to disk.

This mode is intended for long-running deployments (systemd service, container
workload) where an external scheduler or UI drives test execution. A web-based
report viewer reads from *--results-dir* to render per-test summaries.

# REQUIRED OPTIONS

*--workers* _HOSTS_
	Comma-separated list of agent addresses (_host:port_). The daemon
	connects to these agents once on startup and reuses them for every test.

# SERVER OPTIONS

*--listen* _ADDR_
	HTTP API listen address. Default: _0.0.0.0:8080_.

*--results-dir* _DIR_
	Directory where completed test results (metrics, configs, traces) are
	stored. Each test produces a timestamped subdirectory. Default:
	_/opt/netanvil-rs/results_.

*--max-results* _N_
	Maximum number of completed results to retain. Older results are
	pruned when the cap is exceeded. Default: _100_.

# METRICS OPTIONS

*--metrics-port* _PORT_
	Port for the Prometheus metrics endpoint. Exposes the same aggregated
	metrics as one-shot mode, so Prometheus scrape configuration is
	identical regardless of which leader variant is running. Omit to
	disable.

# TLS OPTIONS

mTLS for agent communication. Enabled when all three cert flags are provided.
Leader-server does *not* currently enforce client authentication on its own
HTTP API; deploy behind a reverse proxy for that.

*--tls-ca* _PATH_
	CA certificate PEM used to verify agent certificates.

*--tls-cert* _PATH_
	Leader's certificate PEM, presented to agents.

*--tls-key* _PATH_
	Leader's private key PEM.

# EXTERNAL METRICS

*--external-metrics-url* _URL_
	Optional URL to poll for external metrics during tests. Available to
	tests submitted via the HTTP API as an additional constraint input.

*--external-metrics-field* _FIELD_
	JSON field name to extract from *--external-metrics-url* responses.

# HTTP API

The daemon exposes a REST-style API on *--listen*. Broad endpoint categories:

- *POST /tests* -- submit a new test specification (JSON body matching the
  fields accepted by *netanvil-cli-leader*(1)).
- *GET /tests* -- list queued, running, and completed tests.
- *GET /tests/{id}* -- fetch the status and metrics of a specific test.
- *DELETE /tests/{id}* -- cancel a queued test or abort a running one.
- *GET /results/{id}* -- fetch the persisted result payload.
- *GET /metrics/prometheus* -- Prometheus exposition format (if not routed to
  *--metrics-port*).

See the *netanvil-api* crate source for the authoritative endpoint list and
request/response schemas.

# ENVIRONMENT

*RUST_LOG*
	Log verbosity. See *netanvil-cli*(1).

# EXAMPLES

Minimal deployment with two agents and a results directory:

	netanvil-cli leader-server --workers agent1:9090,agent2:9090 --listen 0.0.0.0:8080 --results-dir /var/lib/netanvil/results

Production deployment with mTLS to agents and dedicated metrics port:

	netanvil-cli leader-server --workers agent1:9090,agent2:9090 --listen 127.0.0.1:8080 --results-dir /var/lib/netanvil/results --max-results 500 --metrics-port 9500 --tls-ca /etc/netanvil/ca.pem --tls-cert /etc/netanvil/leader.pem --tls-key /etc/netanvil/leader.key

Submit a test via curl:

	curl -X POST http://localhost:8080/tests -H 'Content-Type: application/json' -d '{"url": ["http://target:8080/"], "rps": 1000, "duration": "5m", "rate_mode": "adaptive", "latency_limit": 300}'

# SEE ALSO

*netanvil-cli*(1), *netanvil-cli-leader*(1), *netanvil-cli-agent*(1),
*netanvil-rate-control*(7), *netanvil-protocols*(7)
