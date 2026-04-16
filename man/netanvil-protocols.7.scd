netanvil-protocols(7)

# NAME

netanvil-protocols - protocol support and protocol-specific flags for netanvil-cli

# DESCRIPTION

*netanvil-cli* dispatches to a protocol-specific executor based on the URL
scheme passed to *--url*. This page documents the supported protocols, the
flags each one recognises, and the shared payload/framing/mode system.

# URL SCHEMES

Each URL scheme selects an executor:

- *http://* and *https://* -- HTTP/1.1, HTTP/2 (via ALPN), or HTTP/2 cleartext
- *tcp://HOST:PORT* -- raw TCP with configurable framing
- *udp://HOST:PORT* -- UDP datagrams
- *dns://HOST:PORT* -- DNS queries
- *redis://HOST:PORT* -- RESP protocol

Multiple *--url* arguments select round-robin across targets.

# HTTP OPTIONS

*--method* _METHOD_
	HTTP method: _GET_, _POST_, _PUT_, _DELETE_, etc. Default: _GET_.

*--http-version* _VERSION_
	_1.1_ (default), _2_ (h2 over TLS), _2c_ (h2 cleartext), or _auto_ (ALPN
	negotiation). Default: _1.1_.

*-H*, *--header* _NAME: VALUE_ (repeatable)
	Add a header to every request. Example:
	_-H 'Authorization: Bearer abc123'_.

*--connection-policy* _POLICY_
	_keepalive_ (default): reuse connections across requests. _always-new_:
	open a fresh connection per request. _mixed_: reuse a fraction
	controlled by *--persistent-ratio*.

*--persistent-ratio* _FRACTION_
	For *mixed* policy only: fraction (0.0-1.0) of requests that reuse
	existing connections. Default: _0.7_.

*--conn-lifetime* _SPEC_
	Connection lifetime distribution -- how many requests per connection
	before closing. Accepts:

	- _fixed:100_
	- _uniform:50,200_
	- _normal:100,20_ (mean, stddev)

	Omit for unlimited lifetime.

*--bandwidth* _LIMIT_
	Bandwidth cap for modem-speed simulation. Throttles TCP reads to create
	real server-side backpressure. Accepts human-friendly values
	(_56k_, _384k_, _1m_, _10m_) or raw bps (_56000_).

*--timeout* _DURATION_
	Per-request timeout. Default: _30s_.

*--error-threshold* _CODE_
	HTTP status codes >= this value count as errors. _0_ = transport errors
	only. Default: _400_.

# TCP OPTIONS

TCP targets (_tcp://HOST:PORT_) use the payload and framing system described
in *PAYLOADS* and *FRAMING* below. They also accept *--mode*, *--no-response*,
and request/response size distributions for the multi-mode wire protocol used
by *netanvil-test-server*(1).

## Test modes (--mode)

*echo* (default)
	Send payload, read echoed response, measure round-trip.

*rr*
	Request-response mode: send request, read response of a potentially
	different size. Requires *--request-size* and *--response-size*.

*stream*
	Write-only: continuously send *--chunk-size* chunks. Measures outbound
	throughput.

*maerts*
	Read-only (reverse of stream): continuously read chunks from the server.

*bidir*
	Concurrent read and write.

## TCP-specific flags

*--chunk-size* _BYTES_
	Chunk size for *stream*/*maerts*/*bidir* modes. Default: _65536_.

*--request-size* _SPEC_, *--response-size* _SPEC_
	Payload size distribution for *rr*, *source*, and *bidir* modes. See
	*SIZE DISTRIBUTIONS* below.

*--no-response*
	Fire-and-forget: don't wait for a response. Useful for UDP or TCP write
	benchmarks.

*--latency-us* _MICROSECONDS_
	Request server-side latency injection (requires v2 protocol header
	support on the server). The server inserts a delay before each response.

*--error-rate* _PER_10000_
	Request server-side error injection rate. The server truncates responses
	at this rate, causing client-side errors.

# UDP OPTIONS

UDP targets (_udp://HOST:PORT_) accept the same payload flags as TCP.
*--no-response* is especially useful for pure-send UDP workloads.

# DNS OPTIONS

DNS targets (_dns://HOST:PORT_) issue DNS queries and measure response time.

*--dns-domains* _LIST_
	Comma-separated list of domain names to query. Queries are issued
	round-robin across the list.

*--dns-query-type* _TYPE_
	Query type: _A_, _AAAA_, _MX_, _CNAME_, _TXT_, _NS_, _SOA_, _PTR_,
	_SRV_, or _ANY_. Default: _A_.

# PAYLOADS

TCP and UDP protocols accept exactly one of the following payload sources:

*--payload* _STRING_
	UTF-8 string payload, sent verbatim.

*--payload-hex* _HEX_
	Hex-encoded bytes, e.g. _deadbeef_ for the four bytes 0xDE 0xAD 0xBE
	0xEF.

*--payload-file* _PATH_
	Read the payload from a file. Useful for large or binary payloads.

# FRAMING

TCP targets support pluggable framing:

*--framing* _MODE_
	- *raw*: no framing; bytes flow both ways unstructured (default).
	- *length-prefix:1*, *length-prefix:2*, *length-prefix:4*: prefix each
	  frame with a big-endian length of 1, 2, or 4 bytes.
	- *delimiter*: frames are separated by *--delimiter* bytes.
	- *fixed:N*: every frame is exactly _N_ bytes.

*--delimiter* _BYTES_
	Delimiter for *delimiter* framing. Backslash escapes accepted
	(_\r\n_, _\n_, _\0_). Default: _\r\n_.

# SIZE DISTRIBUTIONS

*--request-size* and *--response-size* accept either a bare integer or
distribution syntax:

- *1200* -- fixed 1200-byte payload (shorthand for *fixed:1200*).
- *fixed:1200* -- explicit fixed size.
- *uniform:200,1500* -- uniformly sampled between 200 and 1500 bytes.
- *normal:800,200* -- normally distributed, mean 800, stddev 200.
- *weighted:200@30,1200@50,1500@20* -- discrete mixture; 30% 200B, 50% 1200B,
  20% 1500B. Weights need not sum to 100.

# EXAMPLES

HTTP POST with a JSON body:

	netanvil-cli test --url https://api:8443/login --method POST -H 'Content-Type: application/json' --payload-file body.json

TCP request-response against the test server:

	netanvil-cli test --url tcp://target:9000 --mode rr --request-size fixed:128 --response-size uniform:512,4096

UDP fire-and-forget:

	netanvil-cli test --url udp://target:9001 --payload 'ping' --no-response

DNS A-record load:

	netanvil-cli test --url dns://10.0.0.53:53 --dns-domains example.com,example.org --dns-query-type A

# SEE ALSO

*netanvil-cli-test*(1), *netanvil-cli-leader*(1), *netanvil-rate-control*(7),
*netanvil-test-server*(1)
