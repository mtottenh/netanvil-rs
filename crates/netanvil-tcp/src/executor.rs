//! TCP request executor implementing [`RequestExecutor`].

use std::time::{Duration, Instant};

use netanvil_types::{
    ExecutionError, ExecutionResult, RequestContext, RequestExecutor, TimingBreakdown,
};

use crate::framing;
use crate::spec::TcpRequestSpec;

/// Executor that opens a fresh TCP connection per request, sends a framed
/// payload, and optionally reads a framed response.
pub struct TcpExecutor {
    request_timeout: Duration,
}

impl Default for TcpExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl TcpExecutor {
    pub fn new() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            request_timeout: timeout,
        }
    }
}

impl RequestExecutor for TcpExecutor {
    type Spec = TcpRequestSpec;

    async fn execute(&self, spec: &TcpRequestSpec, context: &RequestContext) -> ExecutionResult {
        let start = Instant::now();

        let result = compio::time::timeout(self.request_timeout, async {
            self.do_execute(spec, start).await
        })
        .await;

        match result {
            Ok(Ok((response_size, timing))) => ExecutionResult {
                request_id: context.request_id,
                intended_time: context.intended_time,
                actual_time: context.actual_time,
                timing,
                status: None,
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

impl TcpExecutor {
    async fn do_execute(
        &self,
        spec: &TcpRequestSpec,
        start: Instant,
    ) -> Result<(u64, TimingBreakdown), ExecutionError> {
        // 1. TCP connect
        let mut stream = compio::net::TcpStream::connect(spec.target)
            .await
            .map_err(|e| ExecutionError::Connect(e.to_string()))?;
        let tcp_connect = start.elapsed();

        // 2. Encode and send payload
        let encoded = framing::encode_frame(&spec.payload, &spec.framing);
        let write_result = compio::io::AsyncWriteExt::write_all(&mut stream, encoded).await;
        write_result
            .0
            .map_err(|e| ExecutionError::Protocol(format!("write: {e}")))?;

        // 3. Read response (if expected)
        let mut response_size = 0u64;
        let mut ttfb = Duration::ZERO;

        if spec.expect_response {
            let read_start = Instant::now();
            match framing::read_framed(&mut stream, &spec.framing, spec.response_max_bytes).await {
                Ok(data) => {
                    ttfb = read_start.elapsed();
                    response_size = data.len() as u64;
                }
                Err(e) => {
                    return Err(ExecutionError::Protocol(format!("read: {e}")));
                }
            }
        }

        let total = start.elapsed();
        Ok((
            response_size,
            TimingBreakdown {
                dns_lookup: Duration::ZERO,
                tcp_connect,
                tls_handshake: Duration::ZERO,
                time_to_first_byte: ttfb,
                content_transfer: total.saturating_sub(tcp_connect).saturating_sub(ttfb),
                total,
            },
        ))
    }
}
