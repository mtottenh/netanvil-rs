use std::cell::RefCell;
use std::time::{Duration, Instant};

use compio::buf::BufResult;
use netanvil_types::{
    ExecutionError, ExecutionResult, RequestContext, RequestExecutor, TimingBreakdown,
};

use crate::spec::UdpRequestSpec;

/// UDP executor with lazy-initialized socket reuse.
///
/// Instead of creating a new `UdpSocket` per request (which costs a `bind()`
/// syscall each time), the executor binds once on first use and reuses the
/// socket for all subsequent requests on this core. The socket is stored in
/// a `RefCell<Option<...>>` with a take/replace pattern to avoid holding a
/// `RefCell` borrow across await points.
///
/// Only one concurrent task can use the socket at a time. For fire-and-forget
/// UDP this is fine since tasks complete quickly. For request-response, the
/// socket is held for the full round trip.
pub struct UdpExecutor {
    request_timeout: Duration,
    socket: RefCell<Option<compio::net::UdpSocket>>,
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
            socket: RefCell::new(None),
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            request_timeout: timeout,
            socket: RefCell::new(None),
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

        let bytes_sent = spec.payload.len() as u64;

        match result {
            Ok(Ok((response_size, timing))) => ExecutionResult {
                request_id: context.request_id,
                intended_time: context.intended_time,
                actual_time: context.actual_time,
                timing,
                status: None,
                bytes_sent,
                response_size,
                error: None,
                response_headers: None,
                response_body: None,
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

impl UdpExecutor {
    async fn do_execute(
        &self,
        spec: &UdpRequestSpec,
        start: Instant,
    ) -> Result<(u64, TimingBreakdown), ExecutionError> {
        // Lazily bind socket on first use (avoids per-request bind syscall)
        let need_init = self.socket.borrow().is_none();
        if need_init {
            let sock = compio::net::UdpSocket::bind("0.0.0.0:0")
                .await
                .map_err(|e| ExecutionError::Connect(format!("UDP bind: {e}")))?;
            *self.socket.borrow_mut() = Some(sock);
        }

        // Take socket out for use (brief borrow, then async, then put back)
        let sock = self
            .socket
            .borrow_mut()
            .take()
            .ok_or_else(|| ExecutionError::Connect("socket unavailable".into()))?;

        // Send datagram (compio buffer-passing model: BufResult tuple struct)
        let BufResult(result, _returned_buf) =
            sock.send_to(spec.payload.clone(), spec.target).await;
        if let Err(e) = result {
            // Put socket back before returning error
            *self.socket.borrow_mut() = Some(sock);
            return Err(ExecutionError::Protocol(format!("send: {e}")));
        }

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
                    // Put socket back before returning error
                    *self.socket.borrow_mut() = Some(sock);
                    return Err(ExecutionError::Protocol(format!("recv: {e}")));
                }
            }
        }

        // Put socket back for reuse
        *self.socket.borrow_mut() = Some(sock);

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
