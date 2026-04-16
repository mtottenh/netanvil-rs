netanvil-cli-agent(1)

# NAME

netanvil-cli-agent - run a remotely controllable netanvil agent node

# SYNOPSIS

*netanvil-cli* *agent* [_--listen ADDR_] [_OPTIONS_]

# DESCRIPTION

Runs the node as a worker that accepts test commands from a netanvil leader
over HTTP. The agent exposes an RPC-style API used by *netanvil-cli-leader*(1)
and *netanvil-cli-leader-server*(1) to start/stop tests, push plugins, and
stream metrics.

A single agent binary can host any number of tests sequentially. When running
in distributed mode, agents typically span multiple machines and the leader
fans out identical test configurations to each.

Authentication uses mTLS: the agent presents its certificate and verifies the
leader's certificate against the configured CA. Only clients whose certificate
SANs match *--trusted-san* are authorized to issue commands.

# OPTIONS

*--listen* _ADDR_
	Listen address: a bare port (_9090_) or _addr:port_ (_10.0.0.2:9090_).
	When only a port is given, the agent binds to _0.0.0.0_. Default:
	_9090_.

*--node-id* _ID_
	Human-readable identifier for metrics labels and logging. Defaults to
	_<hostname>:<port>_ if not specified.

*--cores* _N_
	Number of I/O worker cores. _0_ = auto-detect from available cores.
	Default: _0_.

*--isolate-cpus*
	Migrate non-essential system threads off hot-path CPU cores before
	starting workers. Shields timer and I/O worker cores from scheduling
	noise by moving other processes to the housekeeping core. Requires
	CAP_SYS_NICE for cross-process migration. Recommended on production
	load-test hosts where consistent latency measurements matter.

# TLS OPTIONS

mTLS is enabled when all three of *--tls-ca*, *--tls-cert*, and *--tls-key*
are provided. In mTLS mode, the agent only accepts connections from clients
whose certificate SAN matches one of *--trusted-san*.

*--tls-ca* _PATH_
	Path to the CA certificate PEM. Used to verify leader certificates.

*--tls-cert* _PATH_
	Path to this agent's certificate PEM.

*--tls-key* _PATH_
	Path to this agent's private key PEM.

*--trusted-san* _NAMES_
	Comma-separated list of Subject Alternative Names. Only clients whose
	certificate presents a SAN matching one of these values are authorized
	to start tests. Requires TLS to be enabled. Example value:
	_leader.test.example.com,backup-leader.test.example.com_.

# METRICS OPTIONS

*--metrics-port* _PORT_
	Plain HTTP port for Prometheus metrics scraping. Exposes only
	_/metrics/prometheus_ without client-certificate authentication, so
	scrapers don't need mTLS credentials. Omit to disable.

# ENVIRONMENT

*RUST_LOG*
	Log verbosity. See *netanvil-cli*(1).

# EXAMPLES

Simple agent, no TLS, default port:

	netanvil-cli agent

Agent on a specific interface with 16 workers:

	netanvil-cli agent --listen 10.0.0.2:9090 --cores 16 --isolate-cpus

Agent with mTLS and a Prometheus scrape port:

	netanvil-cli agent --listen 0.0.0.0:9090 --tls-ca /etc/netanvil/ca.pem --tls-cert /etc/netanvil/agent.pem --tls-key /etc/netanvil/agent.key --trusted-san leader.internal --metrics-port 9100

Under a typical systemd unit, _ExecStart_ would be:

	/usr/bin/netanvil-cli agent --listen 0.0.0.0:9090 --isolate-cpus --tls-ca /etc/netanvil/ca.pem --tls-cert /etc/netanvil/agent.pem --tls-key /etc/netanvil/agent.key --trusted-san leader.internal

# SEE ALSO

*netanvil-cli*(1), *netanvil-cli-leader*(1), *netanvil-cli-leader-server*(1)
