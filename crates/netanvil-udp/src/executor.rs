use std::cell::{Cell, RefCell};
use std::net::SocketAddr;
use std::rc::Rc;
use std::time::{Duration, Instant};

use compio::buf::BufResult;
use netanvil_types::{
    ExecutionError, ExecutionResult, RequestContext, RequestExecutor, TimingBreakdown,
};

use crate::loss_tracker::{LossTracker, UdpPacketSource};
use crate::spec::UdpRequestSpec;

/// 8-byte little-endian sequence number header prepended to each datagram.
const SEQ_HEADER_LEN: usize = 8;

/// UDP executor with shared connected socket and per-core loss tracking.
///
/// Each outbound datagram carries an 8-byte sequence number header (prepended
/// by the generator).  When a response arrives, the executor extracts the
/// echoed sequence number and feeds it to the [`LossTracker`] bitmap, which
/// detects loss without waiting for the full request timeout.
///
/// The socket is connected to its target on first use. The kernel caches the
/// route at connect() time, eliminating per-packet route lookup overhead.
/// Connected sockets also use simpler io_uring ops (SEND/RECV instead of
/// SENDMSG/RECVMSG — no sockaddr per packet).
///
/// Unlike TCP, UDP datagrams are independent and atomic — multiple concurrent
/// send/recv operations on the same socket fd are safe. The socket is shared
/// via `RefCell::borrow()` (immutable), allowing all in-flight tasks on a
/// core to perform I/O concurrently through io_uring. With response mixing
/// (task A may receive task B's echo), per-request sequence correlation is
/// approximate, but aggregate loss tracking via the [`LossTracker`] bitmap
/// remains accurate.
pub struct UdpExecutor {
    request_timeout: Duration,
    socket: RefCell<Option<compio::net::UdpSocket>>,
    connected_to: Cell<Option<SocketAddr>>,
    tracker: Rc<RefCell<LossTracker>>,
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
            connected_to: Cell::new(None),
            tracker: Rc::new(RefCell::new(LossTracker::new())),
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            request_timeout: timeout,
            socket: RefCell::new(None),
            connected_to: Cell::new(None),
            tracker: Rc::new(RefCell::new(LossTracker::new())),
        }
    }

    /// Ensure the socket is initialized and connected to `target`.
    ///
    /// Fast path: socket exists and is connected — no-op.
    /// Slow path: create, bind, connect. If two tasks race through the slow
    /// path, the first to finish stores its socket; the second detects the
    /// existing socket after its own `.await` and drops its duplicate.
    async fn ensure_socket(&self, target: SocketAddr) -> Result<(), ExecutionError> {
        {
            let guard = self.socket.borrow();
            if guard.is_some() && self.connected_to.get() == Some(target) {
                return Ok(());
            }
        } // Ref drops here — no borrow held across await

        // Slow path: need to create + connect.
        // Drop the old socket first (brief borrow_mut, no await).
        *self.socket.borrow_mut() = None;

        let sock = compio::net::UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| ExecutionError::Connect(format!("UDP bind: {e}")))?;
        sock.connect(target)
            .await
            .map_err(|e| ExecutionError::Connect(format!("UDP connect: {e}")))?;

        // Double-check: another task may have initialized during our await.
        let mut guard = self.socket.borrow_mut();
        if guard.is_none() || self.connected_to.get() != Some(target) {
            *guard = Some(sock);
            drop(guard);
            self.connected_to.set(Some(target));
        }
        // else: another task beat us — drop our socket silently

        Ok(())
    }
}

impl RequestExecutor for UdpExecutor {
    type Spec = UdpRequestSpec;
    type PacketSource = UdpPacketSource;

    fn packet_counter_source(&self) -> Option<UdpPacketSource> {
        Some(UdpPacketSource(self.tracker.clone()))
    }

    async fn execute(&self, spec: &UdpRequestSpec, context: &RequestContext) -> ExecutionResult {
        let start = Instant::now();

        // Extract and record the outbound seq_no from the payload header.
        let sent_seq = if spec.payload.len() >= SEQ_HEADER_LEN {
            let seq = u64::from_le_bytes(spec.payload[..SEQ_HEADER_LEN].try_into().unwrap());
            self.tracker.borrow_mut().on_send();
            Some(seq)
        } else {
            None
        };

        let result = compio::time::timeout(self.request_timeout, async {
            self.do_execute(spec, start).await
        })
        .await;

        let bytes_sent = spec.payload.len() as u64;

        match result {
            Ok(Ok((response_size, timing, echo_seq))) => {
                // Feed the echoed seq_no to the loss tracker.
                if let Some(seq) = echo_seq {
                    self.tracker.borrow_mut().on_receive(seq);
                }

                // Verify correlation: echoed seq should match sent seq.
                let error = match (sent_seq, echo_seq) {
                    (Some(s), Some(r)) if s != r => Some(ExecutionError::Protocol(format!(
                        "seq mismatch: sent {s}, got {r}"
                    ))),
                    _ => None,
                };

                ExecutionResult {
                    request_id: context.request_id,
                    intended_time: context.intended_time,
                    actual_time: context.actual_time,
                    sent_time: context.sent_time,
                    dispatch_time: context.dispatch_time,
                    timing,
                    status: None,
                    bytes_sent,
                    response_size,
                    error,
                    response_headers: None,
                    response_body: None,
                }
            }
            Ok(Err(err)) => ExecutionResult {
                request_id: context.request_id,
                intended_time: context.intended_time,
                actual_time: context.actual_time,
                sent_time: context.sent_time,
                dispatch_time: context.dispatch_time,
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
                sent_time: context.sent_time,
                dispatch_time: context.dispatch_time,
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
    // Holding Ref across await is intentional: on compio's single-threaded runtime,
    // multiple tasks share the socket via concurrent immutable borrows. The only
    // borrow_mut() is in ensure_socket(), which is a no-op once initialized.
    #[allow(clippy::await_holding_refcell_ref)]
    async fn do_execute(
        &self,
        spec: &UdpRequestSpec,
        start: Instant,
    ) -> Result<(u64, TimingBreakdown, Option<u64>), ExecutionError> {
        self.ensure_socket(spec.target).await?;

        // Shared access: immutable borrow held across await points.
        // Multiple Ref guards can coexist — concurrent tasks share the socket.
        let guard = self.socket.borrow();
        let sock = guard
            .as_ref()
            .ok_or_else(|| ExecutionError::Connect("socket not initialized".into()))?;

        // Send datagram on connected socket (no address needed).
        let BufResult(result, _returned_buf) = sock.send(spec.payload.clone()).await;
        if let Err(e) = result {
            return Err(ExecutionError::Protocol(format!("send: {e}")));
        }

        let mut response_size = 0u64;
        let mut ttfb = Duration::ZERO;
        let mut echo_seq = None;

        if spec.expect_response {
            let recv_start = Instant::now();
            let buf = vec![0u8; spec.response_max_bytes];
            let BufResult(result, returned_buf) = sock.recv(buf).await;
            match result {
                Ok(n) => {
                    ttfb = recv_start.elapsed();
                    response_size = n as u64;
                    // Extract echoed seq_no from the response header.
                    if n >= SEQ_HEADER_LEN {
                        echo_seq = Some(u64::from_le_bytes(
                            returned_buf[..SEQ_HEADER_LEN].try_into().unwrap(),
                        ));
                    }
                }
                Err(e) => {
                    return Err(ExecutionError::Protocol(format!("recv: {e}")));
                }
            }
        }

        // guard drops here — Ref released

        let total = start.elapsed();
        Ok((
            response_size,
            TimingBreakdown {
                dns_lookup: Duration::ZERO,
                tcp_connect: Duration::ZERO,
                tls_handshake: Duration::ZERO,
                time_to_first_byte: ttfb,
                content_transfer: Duration::ZERO,
                total,
            },
            echo_seq,
        ))
    }
}
