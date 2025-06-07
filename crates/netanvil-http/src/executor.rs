use std::time::{Duration, Instant};

use netanvil_types::{
    ExecutionError, ExecutionResult, RequestContext, RequestExecutor, RequestSpec, TimingBreakdown,
};

/// HTTP executor using cyper (compio + hyper bridge).
///
/// Each core gets its own `HttpExecutor` instance. The underlying `cyper::Client`
/// uses `Arc` internally so `clone()` is cheap, but each core's executor is
/// independent — no cross-core connection sharing.
pub struct HttpExecutor {
    client: cyper::Client,
    request_timeout: Duration,
}

impl HttpExecutor {
    pub fn new() -> Self {
        Self {
            client: cyper::Client::new(),
            request_timeout: Duration::from_secs(30),
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            client: cyper::Client::new(),
            request_timeout: timeout,
        }
    }
}

impl RequestExecutor for HttpExecutor {
    async fn execute(&self, spec: &RequestSpec, context: &RequestContext) -> ExecutionResult {
        let start = Instant::now();

        let result = compio::time::timeout(self.request_timeout, async {
            self.do_execute(spec, start).await
        })
        .await;

        match result {
            Ok(Ok((status, response_size, timing))) => ExecutionResult {
                request_id: context.request_id,
                intended_time: context.intended_time,
                actual_time: context.actual_time,
                timing,
                status: Some(status),
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
                response_size: 0,
                error: Some(ExecutionError::Timeout),
            },
        }
    }
}

impl HttpExecutor {
    async fn do_execute(
        &self,
        spec: &RequestSpec,
        start: Instant,
    ) -> Result<(u16, u64, TimingBreakdown), ExecutionError> {
        // Build the request
        let mut builder = self
            .client
            .request(spec.method.clone(), spec.url.as_str())
            .map_err(|e| ExecutionError::Connect(e.to_string()))?;

        // Add headers
        for (name, value) in &spec.headers {
            builder = builder
                .header(name.as_str(), value.as_str())
                .map_err(|e| ExecutionError::Http(e.to_string()))?;
        }

        // Add body
        if let Some(body) = &spec.body {
            builder = builder.body(body.clone());
        }

        // Send request — this includes connect + TLS + send
        let request_sent = Instant::now();
        let response = builder
            .send()
            .await
            .map_err(|e| categorize_error(e))?;

        let ttfb = request_sent.elapsed();
        let status = response.status().as_u16();

        // Read body
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
                dns_lookup: Duration::ZERO, // cyper doesn't expose this
                tcp_connect: Duration::ZERO, // cyper doesn't expose this
                tls_handshake: Duration::ZERO, // cyper doesn't expose this
                time_to_first_byte: ttfb,
                content_transfer,
                total,
            },
        ))
    }
}

fn categorize_error(err: cyper::Error) -> ExecutionError {
    let msg = err.to_string();
    let lower = msg.to_lowercase();

    if lower.contains("connect") || lower.contains("refused") || lower.contains("dns") {
        ExecutionError::Connect(msg)
    } else if lower.contains("tls") || lower.contains("ssl") || lower.contains("certificate") {
        ExecutionError::Tls(msg)
    } else if lower.contains("timeout") {
        ExecutionError::Timeout
    } else {
        ExecutionError::Http(msg)
    }
}
