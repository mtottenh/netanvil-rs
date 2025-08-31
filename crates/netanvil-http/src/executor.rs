use std::time::{Duration, Instant};

use netanvil_types::config::TlsClientConfig;
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
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            client: HttpClient::Standard(cyper::Client::new()),
            request_timeout: timeout,
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
        let compio_tls = build_tls_connector(tls_config)?;
        let connector = ThrottledConnector::with_tls(
            bandwidth_bps,
            compio_tls,
            tls_config.sni_override.clone(),
        );
        let client = hyper_util::client::legacy::Client::builder(cyper_core::CompioExecutor)
            .set_host(true)
            .timer(cyper_core::CompioTimer)
            .build(connector);
        Ok(Self {
            client: HttpClient::Throttled(client),
            request_timeout: timeout,
        })
    }
}

impl RequestExecutor for HttpExecutor {
    type Spec = HttpRequestSpec;

    async fn execute(&self, spec: &HttpRequestSpec, context: &RequestContext) -> ExecutionResult {
        let start = Instant::now();

        let result = compio::time::timeout(self.request_timeout, async {
            match &self.client {
                HttpClient::Standard(c) => do_execute_cyper(c, spec, start).await,
                HttpClient::Throttled(c) => do_execute_throttled(c, spec, start).await,
            }
        })
        .await;

        let bytes_sent = spec.body.as_ref().map_or(0, |b| b.len() as u64);

        match result {
            Ok(Ok((status, response_size, timing))) => ExecutionResult {
                request_id: context.request_id,
                intended_time: context.intended_time,
                actual_time: context.actual_time,
                timing,
                status: Some(status),
                bytes_sent,
                response_size,
                error: None,
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
            },
        }
    }
}

/// Execute a request using the standard cyper client (no throttling).
async fn do_execute_cyper(
    client: &cyper::Client,
    spec: &HttpRequestSpec,
    start: Instant,
) -> Result<(u16, u64, TimingBreakdown), ExecutionError> {
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

    let body_start = Instant::now();
    let body = response
        .bytes()
        .await
        .map_err(|e| ExecutionError::Http(e.to_string()))?;
    let content_transfer = body_start.elapsed();
    let response_size = body.len() as u64;

    let total = start.elapsed();

    Ok((
        status,
        response_size,
        TimingBreakdown {
            dns_lookup: Duration::ZERO,
            tcp_connect: Duration::ZERO,
            tls_handshake: Duration::ZERO,
            time_to_first_byte: ttfb,
            content_transfer,
            total,
        },
    ))
}

/// Execute a request using the throttled hyper client.
async fn do_execute_throttled(
    client: &HyperThrottledClient,
    spec: &HttpRequestSpec,
    start: Instant,
) -> Result<(u16, u64, TimingBreakdown), ExecutionError> {
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

    let body_start = Instant::now();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|e| ExecutionError::Http(e.to_string()))?
        .to_bytes();
    let content_transfer = body_start.elapsed();
    let response_size = body.len() as u64;

    let total = start.elapsed();

    Ok((
        status,
        response_size,
        TimingBreakdown {
            dns_lookup: Duration::ZERO,
            tcp_connect: Duration::ZERO,
            tls_handshake: Duration::ZERO,
            time_to_first_byte: ttfb,
            content_transfer,
            total,
        },
    ))
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
