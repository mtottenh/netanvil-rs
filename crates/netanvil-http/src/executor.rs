use std::time::{Duration, Instant};

use netanvil_types::config::{HttpVersion, TlsClientConfig};
use netanvil_types::{
    ExecutionError, ExecutionResult, HttpRequestSpec, RequestContext, RequestExecutor,
    TimingBreakdown,
};

use crate::connector::ThrottledConnector;
use crate::tls::build_tls_connector;

type HyperThrottledClient =
    hyper_util::client::legacy::Client<ThrottledConnector, http_body_util::Full<bytes::Bytes>>;

/// The underlying HTTP client — either the standard cyper client or our
/// custom throttled hyper client. Using an enum avoids `Option` juggling
/// and makes the active variant explicit.
enum HttpClient {
    /// Standard cyper client — zero overhead, used when no bandwidth limit.
    Standard(cyper::Client),
    /// Custom hyper client with per-socket SO_RCVBUF / TCP_WINDOW_CLAMP
    /// tuning and token-bucket read throttling.
    Throttled(HyperThrottledClient),
}

/// HTTP executor using cyper (compio + hyper bridge).
///
/// Each core gets its own `HttpExecutor` instance. The underlying `cyper::Client`
/// uses `Arc` internally so `clone()` is cheap, but each core's executor is
/// independent — no cross-core connection sharing.
///
/// When bandwidth throttling is enabled, a custom hyper client is used instead
/// of cyper, with per-socket `SO_RCVBUF` / `TCP_WINDOW_CLAMP` tuning and a
/// token-bucket rate limiter on the read path. This creates real TCP
/// backpressure — the server sees the client's receive window shrinking.
pub struct HttpExecutor {
    client: HttpClient,
    request_timeout: Duration,
    /// When true, response headers are captured in ExecutionResult.
    capture_headers: bool,
    /// When true, response body is captured in ExecutionResult.
    capture_body: bool,
    /// Percentage of requests for which body is captured (0-100).
    /// Only used when `capture_body` is true.
    capture_body_pct: u32,
}

impl Default for HttpExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpExecutor {
    pub fn new() -> Self {
        Self {
            client: HttpClient::Standard(cyper::Client::new()),
            request_timeout: Duration::from_secs(30),
            capture_headers: false,
            capture_body: false,
            capture_body_pct: 0,
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            client: HttpClient::Standard(cyper::Client::new()),
            request_timeout: timeout,
            capture_headers: false,
            capture_body: false,
            capture_body_pct: 0,
        }
    }

    /// Create an executor with bandwidth throttling.
    ///
    /// `bandwidth_bps` is the target read rate in **bits** per second.
    /// Combined with OS-level socket tuning, this creates real TCP-level
    /// backpressure on the server.
    pub fn with_bandwidth_limit(bandwidth_bps: u64, timeout: Duration) -> Self {
        let connector = ThrottledConnector::new(Some(bandwidth_bps));
        let client = hyper_util::client::legacy::Client::builder(cyper_core::CompioExecutor)
            .set_host(true)
            .timer(cyper_core::CompioTimer)
            .build(connector);
        Self {
            client: HttpClient::Throttled(client),
            request_timeout: timeout,
            capture_headers: false,
            capture_body: false,
            capture_body_pct: 0,
        }
    }

    /// Create an executor with HTTP version pinning and optional bandwidth throttling.
    ///
    /// When `http_version != Http1`, forces the ThrottledConnector path to
    /// get ALPN and hyper builder control, even without explicit TLS config.
    pub fn with_http_version(
        http_version: HttpVersion,
        bandwidth_bps: Option<u64>,
        timeout: Duration,
    ) -> Self {
        if http_version == HttpVersion::Http1 && bandwidth_bps.is_none() {
            return Self::with_timeout(timeout);
        }

        let connector = ThrottledConnector::new(bandwidth_bps);
        let mut builder = hyper_util::client::legacy::Client::builder(cyper_core::CompioExecutor);
        builder.set_host(true);
        builder.timer(cyper_core::CompioTimer);
        apply_http_version(&mut builder, http_version);
        let client = builder.build(connector);
        Self {
            client: HttpClient::Throttled(client),
            request_timeout: timeout,
            capture_headers: false,
            capture_body: false,
            capture_body_pct: 0,
        }
    }

    /// Create an executor with TLS configuration and optional bandwidth throttling.
    ///
    /// Uses rustls with the ring crypto provider. All HTTPS connections go
    /// through [`ThrottledConnector`] with the pre-configured TLS connector,
    /// bypassing cyper's built-in TLS handling.
    pub fn with_tls_config(
        tls_config: &TlsClientConfig,
        bandwidth_bps: Option<u64>,
        timeout: Duration,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::with_tls_and_version(tls_config, bandwidth_bps, timeout, HttpVersion::Http1)
    }

    /// Create an executor with TLS configuration, bandwidth throttling, and HTTP version.
    pub fn with_tls_and_version(
        tls_config: &TlsClientConfig,
        bandwidth_bps: Option<u64>,
        timeout: Duration,
        http_version: HttpVersion,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let compio_tls = build_tls_connector(tls_config, http_version)?;
        let connector = ThrottledConnector::with_tls(
            bandwidth_bps,
            compio_tls,
            tls_config.sni_override.clone(),
        );
        let mut builder = hyper_util::client::legacy::Client::builder(cyper_core::CompioExecutor);
        builder.set_host(true);
        builder.timer(cyper_core::CompioTimer);
        apply_http_version(&mut builder, http_version);
        let client = builder.build(connector);
        Ok(Self {
            client: HttpClient::Throttled(client),
            request_timeout: timeout,
            capture_headers: false,
            capture_body: false,
            capture_body_pct: 0,
        })
    }

    /// Enable response header and/or body capture.
    ///
    /// When headers are captured, `ExecutionResult.response_headers` is populated.
    /// When body is captured, `ExecutionResult.response_body` is populated for
    /// `body_pct`% of requests (0 = never, 100 = always).
    pub fn enable_capture(&mut self, headers: bool, body_pct: u32) {
        self.capture_headers = headers;
        self.capture_body = body_pct > 0;
        self.capture_body_pct = body_pct.min(100);
    }

    /// Create an executor with connection settings from TestConfig.
    ///
    /// Handles noreuse (fd leaking), dns_mode (address family filtering),
    /// and HTTP version pinning, all of which require the ThrottledConnector
    /// path even without bandwidth limits.
    pub fn with_connection_config(
        tls_config: Option<&TlsClientConfig>,
        bandwidth_bps: Option<u64>,
        timeout: Duration,
        noreuse: bool,
        dns_mode: Option<netanvil_types::config::DnsMode>,
        http_version: HttpVersion,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let needs_custom_connector = tls_config.is_some()
            || bandwidth_bps.is_some()
            || noreuse
            || dns_mode.is_some()
            || http_version != HttpVersion::Http1;

        if !needs_custom_connector {
            return Ok(Self::with_timeout(timeout));
        }

        let mut connector = match tls_config {
            Some(tls) => {
                let compio_tls = build_tls_connector(tls, http_version)?;
                ThrottledConnector::with_tls(bandwidth_bps, compio_tls, tls.sni_override.clone())
            }
            None => ThrottledConnector::new(bandwidth_bps),
        };

        connector.set_noreuse(noreuse);
        connector.set_dns_mode(dns_mode);

        let mut builder = hyper_util::client::legacy::Client::builder(cyper_core::CompioExecutor);
        builder.set_host(true);
        builder.timer(cyper_core::CompioTimer);
        apply_http_version(&mut builder, http_version);

        // When noreuse is active, disable connection pooling so each request
        // gets a fresh connection (the old one's fd leaks intentionally).
        if noreuse {
            builder.pool_max_idle_per_host(0);
        }

        let client = builder.build(connector);
        Ok(Self {
            client: HttpClient::Throttled(client),
            request_timeout: timeout,
            capture_headers: false,
            capture_body: false,
            capture_body_pct: 0,
        })
    }
}

impl RequestExecutor for HttpExecutor {
    type Spec = HttpRequestSpec;

    async fn execute(&self, spec: &HttpRequestSpec, context: &RequestContext) -> ExecutionResult {
        let start = Instant::now();

        // Only capture body for the configured percentage of requests
        let should_capture_body = self.capture_body
            && (self.capture_body_pct >= 100
                || (context.request_id % 100) < self.capture_body_pct as u64);

        let result = compio::time::timeout(self.request_timeout, async {
            match &self.client {
                HttpClient::Standard(c) => {
                    do_execute_cyper(c, spec, start, self.capture_headers, should_capture_body)
                        .await
                }
                HttpClient::Throttled(c) => {
                    do_execute_throttled(c, spec, start, self.capture_headers, should_capture_body)
                        .await
                }
            }
        })
        .await;

        let bytes_sent = spec.body.as_ref().map_or(0, |b| b.len() as u64);

        match result {
            Ok(Ok(http_result)) => ExecutionResult {
                request_id: context.request_id,
                intended_time: context.intended_time,
                actual_time: context.actual_time,
                timing: http_result.timing,
                status: Some(http_result.status),
                bytes_sent,
                response_size: http_result.response_size,
                error: None,
                response_headers: http_result.headers,
                response_body: http_result.body,
            },
            Ok(Err(err)) => ExecutionResult {
                request_id: context.request_id,
                intended_time: context.intended_time,
                actual_time: context.actual_time,
                timing: TimingBreakdown {
                    total: start.elapsed(),
                    ..Default::default()
                },
                status: None,
                bytes_sent: 0,
                response_size: 0,
                error: Some(err),
                response_headers: None,
                response_body: None,
            },
            Err(_timeout) => ExecutionResult {
                request_id: context.request_id,
                intended_time: context.intended_time,
                actual_time: context.actual_time,
                timing: TimingBreakdown {
                    total: start.elapsed(),
                    ..Default::default()
                },
                status: None,
                bytes_sent: 0,
                response_size: 0,
                error: Some(ExecutionError::Timeout),
                response_headers: None,
                response_body: None,
            },
        }
    }
}

/// Result from a single HTTP request execution.
struct HttpResult {
    status: u16,
    response_size: u64,
    timing: TimingBreakdown,
    /// Response headers (only when capture_headers is true).
    headers: Option<Vec<(String, String)>>,
    /// Response body bytes (only when capture_body is true).
    /// Uses `bytes::Bytes` for zero-copy reference counting.
    body: Option<bytes::Bytes>,
}

/// Execute a request using the standard cyper client (no throttling).
async fn do_execute_cyper(
    client: &cyper::Client,
    spec: &HttpRequestSpec,
    start: Instant,
    capture_headers: bool,
    capture_body: bool,
) -> Result<HttpResult, ExecutionError> {
    let mut builder = client
        .request(spec.method.clone(), spec.url.as_str())
        .map_err(|e| ExecutionError::Connect(e.to_string()))?;

    for (name, value) in &spec.headers {
        builder = builder
            .header(name.as_str(), value.as_str())
            .map_err(|e| ExecutionError::Http(e.to_string()))?;
    }

    if let Some(body) = &spec.body {
        builder = builder.body(body.clone());
    }

    let request_sent = Instant::now();
    let response = builder.send().await.map_err(categorize_cyper_error)?;

    let ttfb = request_sent.elapsed();
    let status = response.status().as_u16();

    // Capture response headers before consuming the body
    let headers = if capture_headers {
        Some(
            response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect(),
        )
    } else {
        None
    };

    let body_start = Instant::now();
    let body_bytes = response
        .bytes()
        .await
        .map_err(|e| ExecutionError::Http(e.to_string()))?;
    let content_transfer = body_start.elapsed();
    let response_size = body_bytes.len() as u64;

    let total = start.elapsed();

    Ok(HttpResult {
        status,
        response_size,
        timing: TimingBreakdown {
            dns_lookup: Duration::ZERO,
            tcp_connect: Duration::ZERO,
            tls_handshake: Duration::ZERO,
            time_to_first_byte: ttfb,
            content_transfer,
            total,
        },
        headers,
        body: if capture_body { Some(body_bytes) } else { None },
    })
}

/// Execute a request using the throttled hyper client.
async fn do_execute_throttled(
    client: &HyperThrottledClient,
    spec: &HttpRequestSpec,
    start: Instant,
    capture_headers: bool,
    capture_body: bool,
) -> Result<HttpResult, ExecutionError> {
    use http_body_util::BodyExt;

    // Build hyper request
    let mut builder = hyper::Request::builder()
        .method(spec.method.clone())
        .uri(&spec.url);

    for (name, value) in &spec.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }

    let body_bytes = match &spec.body {
        Some(b) => bytes::Bytes::from(b.clone()),
        None => bytes::Bytes::new(),
    };
    let request = builder
        .body(http_body_util::Full::new(body_bytes))
        .map_err(|e| ExecutionError::Http(e.to_string()))?;

    let request_sent = Instant::now();
    let response = client
        .request(request)
        .await
        .map_err(categorize_hyper_error)?;

    let ttfb = request_sent.elapsed();
    let status = response.status().as_u16();

    // Capture response headers before consuming the body
    let headers = if capture_headers {
        Some(
            response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect(),
        )
    } else {
        None
    };

    let body_start = Instant::now();
    let body_bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|e| ExecutionError::Http(e.to_string()))?
        .to_bytes();
    let content_transfer = body_start.elapsed();
    let response_size = body_bytes.len() as u64;

    let total = start.elapsed();

    Ok(HttpResult {
        status,
        response_size,
        timing: TimingBreakdown {
            dns_lookup: Duration::ZERO,
            tcp_connect: Duration::ZERO,
            tls_handshake: Duration::ZERO,
            time_to_first_byte: ttfb,
            content_transfer,
            total,
        },
        headers,
        body: if capture_body { Some(body_bytes) } else { None },
    })
}

/// Configure the hyper client builder for the requested HTTP version.
///
/// - `Http1` → no change (default is HTTP/1.1; ALPN set to h1.1 elsewhere)
/// - `Http2` / `Http2c` → `.http2_only(true)` (use h2 prior knowledge or fail)
/// - `Auto` → default (try h2, fall back to h1 based on ALPN)
fn apply_http_version(
    builder: &mut hyper_util::client::legacy::Builder,
    http_version: HttpVersion,
) {
    match http_version {
        HttpVersion::Http1 => {
            // Default behavior: HTTP/1.1 only (ALPN set to h1.1 in TLS config)
        }
        HttpVersion::Http2 | HttpVersion::Http2c => {
            builder.http2_only(true);
        }
        HttpVersion::Auto => {
            // Default behavior: let ALPN negotiate
        }
    }
}

fn categorize_cyper_error(err: cyper::Error) -> ExecutionError {
    let msg = err.to_string();
    categorize_error_msg(&msg)
}

fn categorize_hyper_error(err: hyper_util::client::legacy::Error) -> ExecutionError {
    let msg = err.to_string();
    categorize_error_msg(&msg)
}

fn categorize_error_msg(msg: &str) -> ExecutionError {
    let lower = msg.to_lowercase();
    if lower.contains("connect") || lower.contains("refused") || lower.contains("dns") {
        ExecutionError::Connect(msg.to_string())
    } else if lower.contains("tls") || lower.contains("ssl") || lower.contains("certificate") {
        ExecutionError::Tls(msg.to_string())
    } else if lower.contains("timeout") {
        ExecutionError::Timeout
    } else {
        ExecutionError::Http(msg.to_string())
    }
}
