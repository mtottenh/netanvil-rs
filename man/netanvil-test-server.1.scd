netanvil-test-server(1)

# NAME

netanvil-test-server - multi-protocol test server for netanvil load testing

# SYNOPSIS

*netanvil-test-server* tcp [_OPTIONS_]++
*netanvil-test-server* udp [_OPTIONS_]++
*netanvil-test-server* dns [_OPTIONS_]++
*netanvil-test-server* multi [_OPTIONS_]

*netanvil-tcp-echo* [_OPTIONS_]++
*netanvil-udp-echo* [_OPTIONS_]++
*netanvil-dns-echo* [_OPTIONS_]

# DESCRIPTION

*netanvil-test-server* is a multi-protocol test server designed for use with the
netanvil load generator. It supports TCP, UDP, and DNS protocols, running on a
thread-per-core architecture backed by io_uring via the compio async runtime.

The TCP server supports multiple workload modes: request-response (RR),
connect-request-response-close (CRR), echo, sink (discard), source (generate),
and bidirectional. These modes are negotiated via a binary protocol header; if
no header is detected, the server falls back to plain echo mode. The UDP server
echoes datagrams with optional pacing, latency injection, and drop simulation.
The DNS server responds to any query with 127.0.0.1.

The server uses SO_REUSEPORT for multi-worker scaling, allowing multiple worker
threads to share the same listen port with kernel-level load balancing.

The binary supports three invocation patterns:

- Subcommands: *netanvil-test-server tcp --port 9000*
- Symlinks (busybox pattern): create symlinks named *netanvil-tcp-echo*,
  *netanvil-udp-echo*, *netanvil-dns-echo* pointing at the binary. The server
  detects argv[0] and runs in single-protocol mode.
- Multi-protocol: *netanvil-test-server multi* starts TCP, UDP, and DNS
  listeners simultaneously.

# COMMANDS

*tcp* [_OPTIONS_]
	Start a TCP test server. Supports RR, CRR, echo, sink, source, and
	bidirectional modes via the netanvil wire protocol. Falls back to echo
	when no protocol header is detected.

*udp* [_OPTIONS_]
	Start a UDP echo server. Supports connected-mode UDP with per-client
	sockets, pacing, latency injection, and drop simulation.

*dns* [_OPTIONS_]
	Start a DNS authoritative echo server. Responds to any DNS query with
	an A record for 127.0.0.1.

*multi* [_OPTIONS_]
	Start a multi-protocol server with TCP, UDP, and DNS listeners on the
	same set of worker threads.

# TCP OPTIONS

*--port* _PORT_
	TCP listen port. Set to 0 for a random OS-assigned port. Default: _9000_.

*--max-connections* _N_
	Maximum simultaneous TCP connections. Set to 0 for unlimited.
	Default: _10000_.

*--tcp-health-sample-rate* _RATE_
	Fraction of TCP reads (0.0-1.0) to piggyback with
	getsockopt(TCP_INFO) via io_uring linked SQEs. Records RTT, retransmits,
	and lost segments into server metrics. Requires kernel >= 6.7 for
	IORING_OP_URING_CMD; automatically disabled with a warning on older
	kernels. Default: _0.0_ (disabled).

# UDP OPTIONS

*--port* _PORT_
	UDP listen port. Set to 0 for a random OS-assigned port. Default: _9001_.

*--udp-idle-timeout* _SECONDS_
	Connected UDP idle timeout. When > 0, the server creates per-client
	connected sockets for kernel-level 4-tuple demuxing and route caching.
	Set to 0 to use unconnected recvfrom/sendto mode.
	Default: _30_.

*--udp-pacing-bps* _BPS_
	UDP send rate limit in bytes/sec per connected client. Only effective
	when *--udp-idle-timeout* > 0. Set to 0 for unlimited. Default: _0_.

*--udp-latency-us* _MICROSECONDS_
	Delay in microseconds injected before each UDP echo response. Set to 0
	for no delay. Default: _0_.

*--udp-drop-rate* _RATE_
	UDP response drop rate per 10000 requests. For example, 100 means 1%
	of responses are silently dropped. Set to 0 for no drops. Default: _0_.

# DNS OPTIONS

*--port* _PORT_
	DNS listen port. Set to 0 for a random OS-assigned port. Default: _9053_.

# MULTI OPTIONS

*--tcp-port* _PORT_
	TCP listen port. Default: _9000_.

*--udp-port* _PORT_
	UDP listen port. Default: _9001_.

*--dns-port* _PORT_
	DNS listen port. Omit to disable DNS.

The multi subcommand also accepts *--max-connections*, *--tcp-health-sample-rate*,
*--udp-idle-timeout*, *--udp-pacing-bps*, *--udp-latency-us*, and
*--udp-drop-rate* with the same meanings as the single-protocol subcommands.

# COMMON OPTIONS

These options are available in all subcommands.

*--workers* _N_
	Number of worker threads. Each worker runs its own compio runtime with
	dedicated io_uring instances. All workers share the same listen port via
	SO_REUSEPORT. Set to 0 for auto-detect from available CPU cores.
	Default: _1_.

*--pin-cores* _true|false_
	Pin each worker thread to a CPU core via core_affinity. Set to _false_
	to let the OS scheduler decide placement. Default: _true_.

*--fill-data* _PATH_
	Path to a file whose contents are tiled across write buffers for source
	and RR response modes. When not specified, buffers are filled with
	deterministic PRNG data.

*--report* _MODE_
	Metrics reporting mode. Valid values:

	- *none*: no reporting (default)
	- *text*: iperf3-style tabular output to stderr
	- *json*: one JSON object per interval to stderr
	- *prometheus:*_PORT_: Prometheus exposition format served over HTTP on
	  the given port (e.g., *prometheus:9090*)

	Default: _none_.

*--report-interval* _SECONDS_
	Metrics reporting interval for text and json modes. Prometheus mode
	reports on demand when scraped. Default: _1.0_.

# ENVIRONMENT

*RUST_LOG*
	Controls log verbosity via the tracing-subscriber crate. Examples:
	*RUST_LOG=info*, *RUST_LOG=netanvil_test_servers=debug*.

# EXIT STATUS

*0*
	Normal shutdown (Ctrl+C / SIGINT).

*1*
	Configuration error: invalid port, unreadable fill-data file, or no
	listeners configured.

# TCP PROTOCOL

The TCP server uses a binary protocol header to negotiate the workload mode.
The first byte is a sentinel (0xF0); if not present, the server falls back to
plain echo mode for backward compatibility.

Protocol modes:

*RR (request-response)*
	Read _request\_size_ bytes, write _response\_size_ bytes, repeat.

*CRR (connect-RR-close)*
	Single RR transaction per connection. The server closes the connection
	after sending the response.

*SINK (discard)*
	Read and discard all incoming data. No response is sent.

*SOURCE (generate)*
	Write _response\_size_-byte chunks continuously. No reads after the
	header.

*BIDIR (bidirectional)*
	Concurrent read and write. The server reads and writes simultaneously.

*ECHO (fallback)*
	Reflect all received data back to the client. Active when no protocol
	header is detected.

# EXAMPLES

Start a TCP echo server on port 9000 with 4 workers:

	netanvil-test-server tcp --port 9000 --workers 4

Start a UDP echo server with 1% response drop rate and 500us latency:

	netanvil-test-server udp --port 9001 --udp-drop-rate 100 --udp-latency-us 500

Start a multi-protocol server with text metrics reporting:

	netanvil-test-server multi --tcp-port 9000 --udp-port 9001 --report text --report-interval 2.0

Create a symlink for single-protocol invocation:

	ln -sf netanvil-test-server netanvil-tcp-echo

Start via symlink with Prometheus metrics:

	netanvil-tcp-echo --port 9000 --report prometheus:9090

Start a DNS echo server for testing DNS load:

	netanvil-test-server dns --port 5353 --workers 2

# SEE ALSO

*netanvil-cli*(1)

Project: https://github.com/mtottenh/netanvil-rs

# AUTHORS

NetAnvil-RS Contributors.

# BUGS

Report bugs at https://github.com/mtottenh/netanvil-rs/issues.
