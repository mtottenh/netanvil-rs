use std::time::{Duration, Instant};

use compio::buf::BufResult;
use netanvil_types::{
    ExecutionError, ExecutionResult, RequestContext, RequestExecutor, TimingBreakdown,
};

use crate::spec::UdpRequestSpec;

/// UDP executor that creates a new socket per request.
///
/// UDP sockets are cheap to create and this avoids all sharing issues when
/// the executor is `Rc`-shared across concurrent spawned tasks on a single core.
/// For fire-and-forget (no response), the socket is dropped immediately after send.
/// For request-response, the socket lives for the duration of the exchange.
pub struct UdpExecutor {
    request_timeout: Duration,
}

impl Default for UdpExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl UdpExecutor {
    pub fn new() -> Self {
        Self {
            request_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            request_timeout: timeout,
        }
    }
}

impl RequestExecutor for UdpExecutor {
    type Spec = UdpRequestSpec;

    async fn execute(&self, spec: &UdpRequestSpec, context: &RequestContext) -> ExecutionResult {
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

impl UdpExecutor {
    async fn do_execute(
        &self,
        spec: &UdpRequestSpec,
        start: Instant,
    ) -> Result<(u64, TimingBreakdown), ExecutionError> {
        // Create a new socket per request. UDP sockets are cheap and this
        // avoids RefCell borrow-across-await issues with Rc-shared executors.
        let sock = compio::net::UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| ExecutionError::Connect(format!("UDP bind: {e}")))?;

        // Send datagram (compio buffer-passing model: BufResult tuple struct)
        let BufResult(result, _returned_buf) =
            sock.send_to(spec.payload.clone(), spec.target).await;
        result.map_err(|e| ExecutionError::Protocol(format!("send: {e}")))?;

        let mut response_size = 0u64;
        let mut ttfb = Duration::ZERO;

        // Optionally receive a response datagram
        if spec.expect_response {
            let recv_start = Instant::now();
            let buf = vec![0u8; spec.response_max_bytes];
            let BufResult(result, _returned_buf) = sock.recv_from(buf).await;
            match result {
                Ok((n, _addr)) => {
                    ttfb = recv_start.elapsed();
                    response_size = n as u64;
                }
                Err(e) => {
                    return Err(ExecutionError::Protocol(format!("recv: {e}")));
                }
            }
        }

        let total = start.elapsed();
        Ok((
            response_size,
            TimingBreakdown {
                dns_lookup: Duration::ZERO,
                tcp_connect: Duration::ZERO, // UDP has no connect phase
                tls_handshake: Duration::ZERO,
                time_to_first_byte: ttfb,
                content_transfer: Duration::ZERO,
                total,
            },
        ))
    }
}
