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
/// send/recv operations on the same socket fd are safe. The socket is handed
/// out as an `Rc<UdpSocket>` clone per request, so tasks perform their I/O
/// awaits without holding the `RefCell` borrow. With response mixing (task A
/// may receive task B's echo), per-request sequence correlation is approximate,
/// but aggregate loss tracking via the [`LossTracker`] bitmap remains accurate.
pub struct UdpExecutor {
    request_timeout: Duration,
    socket: RefCell<Option<Rc<compio::net::UdpSocket>>>,
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

    /// Ensure the socket is initialized and connected to `target`, returning
    /// an `Rc` clone for the caller to use across its own await points.
    ///
    /// Fast path: socket exists and is connected — clone the `Rc` and return.
    /// Slow path: create, bind, connect. If two tasks race through the slow
    /// path, the first to finish stores its socket; the second detects the
    /// existing matching socket after its own `.await` and returns that one,
    /// dropping its own duplicate.
    ///
    /// No `RefCell` borrow is ever held across an `.await`, so concurrent
    /// tasks can safely mutate the stored socket without conflicting with
    /// in-flight send/recv on earlier `Rc` clones.
    async fn ensure_socket(
        &self,
        target: SocketAddr,
    ) -> Result<Rc<compio::net::UdpSocket>, ExecutionError> {
        {
            let guard = self.socket.borrow();
            if let Some(sock) = guard.as_ref() {
                if self.connected_to.get() == Some(target) {
                    return Ok(sock.clone());
                }
            }
        } // Ref drops here — no borrow held across await

        // Slow path: create + connect a fresh socket.
        let sock = compio::net::UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| ExecutionError::Connect(format!("UDP bind: {e}")))?;
        sock.connect(target)
            .await
            .map_err(|e| ExecutionError::Connect(format!("UDP connect: {e}")))?;
        let sock = Rc::new(sock);

        // Another task may have raced us. If their stored socket matches our
        // target, use theirs and drop ours.
        let mut guard = self.socket.borrow_mut();
        if let Some(existing) = guard.as_ref() {
            if self.connected_to.get() == Some(target) {
                return Ok(existing.clone());
            }
        }
        *guard = Some(sock.clone());
        drop(guard);
        self.connected_to.set(Some(target));
        Ok(sock)
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
    async fn do_execute(
        &self,
        spec: &UdpRequestSpec,
        start: Instant,
    ) -> Result<(u64, TimingBreakdown, Option<u64>), ExecutionError> {
        // Rc clone of the connected socket. No RefCell borrow is held across
        // the send/recv awaits — another task may safely rebind the cell.
        let sock = self.ensure_socket(spec.target).await?;

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
