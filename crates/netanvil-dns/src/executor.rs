//! DNS executor using UDP datagrams via compio.

use std::time::{Duration, Instant};

use compio::net::UdpSocket;
use compio::BufResult;

use netanvil_types::dns_spec::DnsRequestSpec;
use netanvil_types::request::{ExecutionError, ExecutionResult, RequestContext, TimingBreakdown};
use netanvil_types::RequestExecutor;

use crate::wire;

/// Executes DNS queries over UDP.
///
/// Each query creates a fresh UDP socket bound to an ephemeral port.
/// TCP fallback for truncated responses is a future enhancement.
pub struct DnsExecutor {
    request_timeout: Duration,
}

impl DnsExecutor {
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

impl Default for DnsExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestExecutor for DnsExecutor {
    type Spec = DnsRequestSpec;
    type PacketSource = netanvil_types::NoopPacketSource;

    async fn execute(&self, spec: &DnsRequestSpec, ctx: &RequestContext) -> ExecutionResult {
        let start = Instant::now();

        // Build DNS wire-format query
        let query_bytes = wire::encode_query(spec);
        let bytes_sent = query_bytes.len() as u64;

        // Create UDP socket
        let sock = match UdpSocket::bind("0.0.0.0:0").await {
            Ok(s) => s,
            Err(e) => {
                return ExecutionResult {
                    request_id: ctx.request_id,
                    intended_time: ctx.intended_time,
                    actual_time: ctx.actual_time,
                    timing: TimingBreakdown {
                        total: start.elapsed(),
                        ..Default::default()
                    },
                    status: None,
                    bytes_sent: 0,
                    response_size: 0,
                    error: Some(ExecutionError::Connect(format!("UDP bind: {e}"))),
                    response_headers: None,
                    response_body: None,
                };
            }
        };

        // Send query
        let BufResult(send_result, _) = sock.send_to(query_bytes, spec.server).await;
        if let Err(e) = send_result {
            return ExecutionResult {
                request_id: ctx.request_id,
                intended_time: ctx.intended_time,
                actual_time: ctx.actual_time,
                timing: TimingBreakdown {
                    total: start.elapsed(),
                    ..Default::default()
                },
                status: None,
                bytes_sent,
                response_size: 0,
                error: Some(ExecutionError::Connect(format!("UDP send: {e}"))),
                response_headers: None,
                response_body: None,
            };
        }

        // Receive response with timeout
        let recv_buf = vec![0u8; 4096];
        let recv_future = sock.recv_from(recv_buf);
        let result = compio::time::timeout(self.request_timeout, recv_future).await;

        match result {
            Ok(BufResult(Ok((n, _addr)), buf)) => {
                let ttfb = start.elapsed();
                let rcode = wire::parse_rcode(&buf[..n]);

                ExecutionResult {
                    request_id: ctx.request_id,
                    intended_time: ctx.intended_time,
                    actual_time: ctx.actual_time,
                    timing: TimingBreakdown {
                        total: ttfb,
                        time_to_first_byte: ttfb,
                        ..Default::default()
                    },
                    status: Some(rcode as u16),
                    bytes_sent,
                    response_size: n as u64,
                    error: None,
                    response_headers: None,
                    response_body: None,
                }
            }
            Ok(BufResult(Err(e), _)) => ExecutionResult {
                request_id: ctx.request_id,
                intended_time: ctx.intended_time,
                actual_time: ctx.actual_time,
                timing: TimingBreakdown {
                    total: start.elapsed(),
                    ..Default::default()
                },
                status: None,
                bytes_sent,
                response_size: 0,
                error: Some(ExecutionError::Other(format!("UDP recv: {e}"))),
                response_headers: None,
                response_body: None,
            },
            Err(_timeout) => ExecutionResult {
                request_id: ctx.request_id,
                intended_time: ctx.intended_time,
                actual_time: ctx.actual_time,
                timing: TimingBreakdown {
                    total: start.elapsed(),
                    ..Default::default()
                },
                status: None,
                bytes_sent,
                response_size: 0,
                error: Some(ExecutionError::Timeout),
                response_headers: None,
                response_body: None,
            },
        }
    }
}
