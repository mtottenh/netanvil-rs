//! UDP echo server using compio's io_uring-backed runtime.
//!
//! Spawns a background thread running a compio runtime that receives UDP
//! datagrams and echoes them back to the sender.
//!
//! When connected UDP is enabled (idle_timeout > 0), the listener creates
//! per-client connected sockets via SO_REUSEPORT + connect(). The kernel
//! routes subsequent packets from that peer to the connected socket (4-tuple
//! match wins over 2-tuple in `__udp4_lib_lookup`), providing kernel-level
//! demuxing, route caching, and simpler io_uring ops (send/recv vs sendmsg/recvmsg).

use std::cell::RefCell;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use compio::buf::BufResult;
use compio::net::SocketOpts;

use crate::bufpool::BufPool;
use crate::metrics::WorkerMetrics;
use crate::protocol;
use crate::tcp::make_pool;
use crate::ServerConfig;

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

/// Handle to a running UDP echo server.
///
/// Dropping the handle sends a shutdown signal and joins the server thread.
pub struct UdpEchoHandle {
    /// The address the server is listening on.
    pub addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for UdpEchoHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// Start a UDP echo server on a random port (127.0.0.1:0).
///
/// Returns a handle that stops the server when dropped.
/// Uses unconnected mode (backward compat).
pub fn start_udp_echo() -> UdpEchoHandle {
    start_udp_echo_on("127.0.0.1:0")
}

/// Start a UDP echo server on the given address.
///
/// Use `"127.0.0.1:0"` for a random port or `"127.0.0.1:9000"` for a fixed one.
/// Returns a handle that stops the server when dropped.
/// Uses unconnected mode (backward compat).
pub fn start_udp_echo_on(addr: &str) -> UdpEchoHandle {
    start_udp_echo_with_config(ServerConfig {
        addr: addr.to_string(),
        ..ServerConfig::udp_default()
    })
}

/// Start a UDP echo server with full configuration.
pub fn start_udp_echo_with_config(config: ServerConfig) -> UdpEchoHandle {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();
    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();

    let thread = std::thread::Builder::new()
        .name("udp-echo-server".into())
        .spawn(move || {
            let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
            rt.block_on(async {
                let socket_opts = SocketOpts::new()
                    .reuse_address(true)
                    .recv_buffer_size(config.recv_buf_size)
                    .send_buffer_size(config.send_buf_size);

                let socket = compio::net::UdpSocket::bind_with_options(&config.addr, &socket_opts)
                    .await
                    .unwrap();
                let local_addr = socket.local_addr().unwrap();
                let local_port = local_addr.port();
                addr_tx.send(local_addr).unwrap();

                tracing::info!("UDP echo server listening on {}", local_addr);

                let pool = Rc::new(make_pool(&config, 65536, 64));
                let metrics = Arc::new(WorkerMetrics::new());
                udp_echo_loop(
                    socket,
                    &shutdown_clone,
                    &pool,
                    config.udp_idle_timeout_secs,
                    local_port,
                    &metrics,
                    config.udp_drop_rate,
                    config.udp_latency_us,
                    config.udp_pacing_bps,
                )
                .await;
            });
        })
        .expect("failed to spawn UDP echo server thread");

    let addr = addr_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("UDP echo server failed to start within 5s");

    UdpEchoHandle {
        addr,
        shutdown,
        thread: Some(thread),
    }
}

/// Maximum number of recv errors to log before suppressing.
const MAX_LOGGED_ERRORS: u64 = 10;

/// Maximum UDP payload size (65535 - 20 byte IP header - 8 byte UDP header).
const MAX_UDP_PAYLOAD: usize = 65507;

/// Session mode for connected UDP clients, determined from the first datagram.
enum UdpSessionMode {
    /// No protocol header detected — echo all datagrams.
    Echo,
    /// Request-response: respond to each datagram with `response_size` bytes.
    Rr {
        response_size: u32,
        latency_us: Option<u32>,
        error_rate: Option<u32>,
    },
    /// Sink: receive and discard all datagrams, never respond.
    Sink,
    /// Source: send `response_size`-byte datagrams continuously.
    Source {
        response_size: u32,
        latency_us: Option<u32>,
    },
    /// Bidirectional: concurrent SOURCE (send continuously) + SINK (recv and discard).
    Bidir {
        response_size: u32,
        latency_us: Option<u32>,
    },
}

impl UdpSessionMode {
    /// Build a session mode from a parsed protocol header.
    /// CRR (0x05) is treated as RR since UDP has no connection to close.
    fn from_header(h: &protocol::ParsedHeader) -> Self {
        match h.mode {
            protocol::MODE_RR | protocol::MODE_CRR => UdpSessionMode::Rr {
                response_size: h.response_size,
                latency_us: h.latency_us,
                error_rate: h.error_rate,
            },
            protocol::MODE_SINK => UdpSessionMode::Sink,
            protocol::MODE_SOURCE => UdpSessionMode::Source {
                response_size: h.response_size,
                latency_us: h.latency_us,
            },
            protocol::MODE_BIDIR => UdpSessionMode::Bidir {
                response_size: h.response_size,
                latency_us: h.latency_us,
            },
            _ => UdpSessionMode::Echo,
        }
    }
}

/// Build a response buffer of the given size from the pool, clamped to MAX_UDP_PAYLOAD.
fn build_udp_response(pool: &BufPool, response_size: u32) -> Vec<u8> {
    let size = (response_size as usize).min(MAX_UDP_PAYLOAD);
    let mut buf = pool.take();
    if buf.len() > size {
        buf.truncate(size);
    } else if buf.len() < size {
        buf.resize(size, 0xAA);
    }
    buf
}

/// Build a half-size error response filled with 0xEE.
fn build_udp_error_response(response_size: u32) -> Vec<u8> {
    let size = (response_size as usize).min(MAX_UDP_PAYLOAD);
    let half = size / 2;
    vec![0xEE_u8; half]
}

/// Check if error should be injected this iteration.
fn should_inject_error(error_rate: Option<u32>, rng: &mut SmallRng) -> bool {
    if let Some(rate) = error_rate {
        rng.gen_range(0..10000u32) < rate
    } else {
        false
    }
}

/// Token-bucket rate limiter for UDP send pacing.
struct TokenBucket {
    tokens: f64,
    capacity: f64,
    rate: f64,
    last_refill: Instant,
}

impl TokenBucket {
    fn new(rate_bps: u64) -> Self {
        let rate = rate_bps as f64;
        Self {
            tokens: rate,
            capacity: rate * 2.0,
            rate,
            last_refill: Instant::now(),
        }
    }

    /// Always deducts tokens (may go negative). Returns Some(delay) if tokens
    /// went negative, None if the send can proceed immediately.
    fn try_consume(&mut self, bytes: usize) -> Option<Duration> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + self.rate * elapsed).min(self.capacity);
        self.last_refill = now;

        self.tokens -= bytes as f64;
        if self.tokens >= 0.0 {
            None
        } else {
            Some(Duration::from_secs_f64(-self.tokens / self.rate))
        }
    }
}

/// Core UDP echo loop. Takes a pre-bound socket and runs until shutdown.
///
/// Used by both `start_udp_echo_with_config` (single-worker) and
/// `TestServer` (multi-worker with SO_REUSEPORT).
///
/// When `idle_timeout_secs > 0`, enables connected UDP mode: new peers get
/// a dedicated connected socket for kernel-level demuxing. When 0, uses the
/// classic unconnected recvfrom/sendto path.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn udp_echo_loop(
    socket: compio::net::UdpSocket,
    shutdown: &AtomicBool,
    pool: &BufPool,
    idle_timeout_secs: u32,
    local_port: u16,
    metrics: &WorkerMetrics,
    udp_drop_rate: u32,
    udp_latency_us: u64,
    udp_pacing_bps: u64,
) {
    if idle_timeout_secs > 0 {
        udp_echo_loop_connected(
            socket,
            shutdown,
            pool,
            idle_timeout_secs,
            local_port,
            metrics,
            udp_drop_rate,
            udp_latency_us,
            udp_pacing_bps,
        )
        .await;
    } else {
        udp_echo_loop_unconnected(
            socket,
            shutdown,
            pool,
            metrics,
            udp_drop_rate,
            udp_latency_us,
        )
        .await;
    }
}

/// Unconnected UDP loop with per-datagram protocol detection.
///
/// Each datagram is inspected for a protocol sentinel. If found, the header
/// determines the mode for that single datagram. Without a sentinel, the
/// datagram is echoed (backward compat). SOURCE and BIDIR modes are rejected
/// in unconnected mode since they require a persistent peer address.
async fn udp_echo_loop_unconnected(
    socket: compio::net::UdpSocket,
    shutdown: &AtomicBool,
    pool: &BufPool,
    metrics: &WorkerMetrics,
    udp_drop_rate: u32,
    udp_latency_us: u64,
) {
    let mut error_count: u64 = 0;
    let mut rng = SmallRng::seed_from_u64(0xBEEF_CAFE);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("UDP echo loop shutting down");
            break;
        }

        let buf = pool.take();
        let recv_result =
            compio::time::timeout(Duration::from_millis(100), socket.recv_from(buf)).await;

        match recv_result {
            Ok(BufResult(Ok((n, peer)), recv_buf)) => {
                metrics.inc_datagrams_received();
                metrics.add_bytes_received(n as u64);

                if udp_drop_rate > 0 && rng.gen_range(0..10000u32) < udp_drop_rate {
                    metrics.inc_datagrams_dropped();
                    pool.give(recv_buf);
                    continue;
                }

                // Protocol detection: check first byte for sentinel.
                if n > 0 && protocol::is_protocol_sentinel(recv_buf[0]) {
                    let mode = match protocol::parse_header_with_offset(&recv_buf[..n]) {
                        Some((header, _offset)) => UdpSessionMode::from_header(&header),
                        None => {
                            // Malformed header — fall back to echo.
                            UdpSessionMode::Echo
                        }
                    };

                    match mode {
                        UdpSessionMode::Rr {
                            response_size,
                            latency_us,
                            error_rate,
                        } => {
                            pool.give(recv_buf);
                            // Use header's latency if present, otherwise config-level.
                            let lat = latency_us.map(|us| us as u64).unwrap_or(udp_latency_us);
                            if lat > 0 {
                                compio::time::sleep(Duration::from_micros(lat)).await;
                            }
                            // Error injection.
                            if should_inject_error(error_rate, &mut rng) {
                                let err_buf = build_udp_error_response(response_size);
                                let send_size = err_buf.len();
                                let BufResult(_, returned) = socket.send_to(err_buf, peer).await;
                                drop(returned);
                                metrics.inc_errors();
                                metrics.inc_datagrams_sent();
                                metrics.add_bytes_sent(send_size as u64);
                            } else {
                                let send_buf = build_udp_response(pool, response_size);
                                let send_size = send_buf.len();
                                let BufResult(result, returned) =
                                    socket.send_to(send_buf, peer).await;
                                pool.give(returned);
                                if result.is_ok() {
                                    metrics.inc_datagrams_sent();
                                    metrics.add_bytes_sent(send_size as u64);
                                } else {
                                    metrics.inc_datagrams_dropped();
                                }
                            }
                            metrics.inc_requests_completed();
                        }
                        UdpSessionMode::Sink => {
                            // Receive and discard — no response.
                            pool.give(recv_buf);
                        }
                        UdpSessionMode::Source { .. } | UdpSessionMode::Bidir { .. } => {
                            // SOURCE/BIDIR require connected mode.
                            tracing::debug!(
                                "UDP SOURCE/BIDIR mode requires connected UDP, ignoring datagram from {}",
                                peer
                            );
                            metrics.inc_datagrams_dropped();
                            pool.give(recv_buf);
                        }
                        UdpSessionMode::Echo => {
                            // Malformed header fallback — echo.
                            let mut send_buf = pool.take();
                            send_buf.truncate(n);
                            if send_buf.len() < n {
                                send_buf.resize(n, 0);
                            }
                            send_buf[..n].copy_from_slice(&recv_buf[..n]);
                            pool.give(recv_buf);
                            let BufResult(result, returned) = socket.send_to(send_buf, peer).await;
                            pool.give(returned);
                            if result.is_ok() {
                                metrics.inc_datagrams_sent();
                                metrics.add_bytes_sent(n as u64);
                            } else {
                                metrics.inc_datagrams_dropped();
                            }
                        }
                    }
                } else {
                    // No protocol sentinel — echo mode.
                    if udp_latency_us > 0 {
                        compio::time::sleep(Duration::from_micros(udp_latency_us)).await;
                    }

                    let mut send_buf = pool.take();
                    send_buf.truncate(n);
                    if send_buf.len() < n {
                        send_buf.resize(n, 0);
                    }
                    send_buf[..n].copy_from_slice(&recv_buf[..n]);
                    pool.give(recv_buf);

                    let BufResult(result, returned) = socket.send_to(send_buf, peer).await;
                    pool.give(returned);
                    if result.is_ok() {
                        metrics.inc_datagrams_sent();
                        metrics.add_bytes_sent(n as u64);
                    } else {
                        metrics.inc_datagrams_dropped();
                    }
                }
            }
            Ok(BufResult(Err(e), recv_buf)) => {
                pool.give(recv_buf);
                error_count += 1;
                metrics.inc_errors();
                if error_count <= MAX_LOGGED_ERRORS {
                    tracing::warn!(
                        "UDP recv error ({}/{}): {}",
                        error_count,
                        MAX_LOGGED_ERRORS,
                        e
                    );
                } else if error_count == MAX_LOGGED_ERRORS + 1 {
                    tracing::warn!(
                        "UDP recv error logging suppressed after {} errors",
                        MAX_LOGGED_ERRORS
                    );
                }
                continue;
            }
            Err(_timeout) => {
                continue;
            }
        }
    }
}

/// Connected UDP loop with per-session protocol negotiation.
///
/// The first datagram from a new peer is inspected for a protocol header.
/// If found, the session mode is determined and passed to the connected
/// handler which maintains that mode for the lifetime of the session.
#[allow(clippy::too_many_arguments)]
async fn udp_echo_loop_connected(
    socket: compio::net::UdpSocket,
    shutdown: &AtomicBool,
    pool: &BufPool,
    idle_timeout_secs: u32,
    local_port: u16,
    metrics: &WorkerMetrics,
    udp_drop_rate: u32,
    udp_latency_us: u64,
    udp_pacing_bps: u64,
) {
    let idle_timeout = Duration::from_secs(idle_timeout_secs as u64);
    let active_clients: Rc<RefCell<HashSet<SocketAddr>>> = Rc::new(RefCell::new(HashSet::new()));
    let mut error_count: u64 = 0;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("UDP connected loop shutting down");
            break;
        }

        let buf = pool.take();
        let recv_result =
            compio::time::timeout(Duration::from_millis(100), socket.recv_from(buf)).await;

        match recv_result {
            Ok(BufResult(Ok((n, peer)), recv_buf)) => {
                metrics.inc_datagrams_received();
                metrics.add_bytes_received(n as u64);
                let is_known = active_clients.borrow().contains(&peer);

                if is_known {
                    // Kernel should have routed to connected socket; this is a
                    // brief race window. Echo on listener as fallback.
                    let mut send_buf = pool.take();
                    send_buf.truncate(n);
                    if send_buf.len() < n {
                        send_buf.resize(n, 0);
                    }
                    send_buf[..n].copy_from_slice(&recv_buf[..n]);
                    pool.give(recv_buf);
                    let BufResult(result, returned) = socket.send_to(send_buf, peer).await;
                    pool.give(returned);
                    if result.is_ok() {
                        metrics.inc_datagrams_sent();
                        metrics.add_bytes_sent(n as u64);
                    } else {
                        metrics.inc_datagrams_dropped();
                    }
                } else {
                    // New peer: detect protocol mode from first datagram.
                    let mode = if n > 0 && protocol::is_protocol_sentinel(recv_buf[0]) {
                        match protocol::parse_header_with_offset(&recv_buf[..n]) {
                            Some((header, _)) => UdpSessionMode::from_header(&header),
                            None => UdpSessionMode::Echo,
                        }
                    } else {
                        UdpSessionMode::Echo
                    };

                    // Handle first-packet response on the listener based on mode.
                    match &mode {
                        UdpSessionMode::Echo => {
                            // Echo first packet on listener.
                            let mut send_buf = pool.take();
                            send_buf.truncate(n);
                            if send_buf.len() < n {
                                send_buf.resize(n, 0);
                            }
                            send_buf[..n].copy_from_slice(&recv_buf[..n]);
                            pool.give(recv_buf);
                            let BufResult(result, returned) = socket.send_to(send_buf, peer).await;
                            pool.give(returned);
                            if result.is_ok() {
                                metrics.inc_datagrams_sent();
                                metrics.add_bytes_sent(n as u64);
                            } else {
                                metrics.inc_datagrams_dropped();
                            }
                        }
                        UdpSessionMode::Rr { response_size, .. } => {
                            // Send a response_size response for the first datagram.
                            pool.give(recv_buf);
                            let send_buf = build_udp_response(pool, *response_size);
                            let send_size = send_buf.len();
                            let BufResult(result, returned) = socket.send_to(send_buf, peer).await;
                            pool.give(returned);
                            if result.is_ok() {
                                metrics.inc_datagrams_sent();
                                metrics.add_bytes_sent(send_size as u64);
                            } else {
                                metrics.inc_datagrams_dropped();
                            }
                            metrics.inc_requests_completed();
                        }
                        UdpSessionMode::Sink
                        | UdpSessionMode::Source { .. }
                        | UdpSessionMode::Bidir { .. } => {
                            // Control packet — no response on listener.
                            pool.give(recv_buf);
                        }
                    }

                    // Create connected socket and spawn mode-specific handler.
                    match crate::create_connected_udp(local_port, peer, 4 * 1024 * 1024) {
                        Ok(std_socket) => {
                            match compio::net::UdpSocket::from_std(std_socket) {
                                Ok(connected) => {
                                    active_clients.borrow_mut().insert(peer);
                                    let clients = Rc::clone(&active_clients);
                                    let sd_flag = shutdown as *const AtomicBool;
                                    // SAFETY: shutdown lives for the lifetime of the loop,
                                    // and all spawned tasks are on the same single-threaded runtime.
                                    let sd_ref = unsafe { &*sd_flag };
                                    let handler_pool = pool as *const BufPool;
                                    let handler_pool_ref = unsafe { &*handler_pool };
                                    let metrics_ptr = metrics as *const WorkerMetrics;
                                    compio::runtime::spawn(async move {
                                        // SAFETY: metrics outlives all spawned tasks (same
                                        // argument as shutdown and pool raw pointers above).
                                        let m = unsafe { &*metrics_ptr };
                                        dispatch_connected_udp(
                                            connected,
                                            mode,
                                            sd_ref,
                                            handler_pool_ref,
                                            idle_timeout,
                                            m,
                                            udp_drop_rate,
                                            udp_latency_us,
                                            udp_pacing_bps,
                                        )
                                        .await;
                                        clients.borrow_mut().remove(&peer);
                                    })
                                    .detach();
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "failed to convert connected UDP socket for {}: {}",
                                        peer,
                                        e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "failed to create connected UDP socket for {}: {} (falling back to listener)",
                                peer,
                                e
                            );
                        }
                    }
                }
            }
            Ok(BufResult(Err(e), recv_buf)) => {
                pool.give(recv_buf);
                error_count += 1;
                metrics.inc_errors();
                if error_count <= MAX_LOGGED_ERRORS {
                    tracing::warn!(
                        "UDP recv error ({}/{}): {}",
                        error_count,
                        MAX_LOGGED_ERRORS,
                        e
                    );
                } else if error_count == MAX_LOGGED_ERRORS + 1 {
                    tracing::warn!(
                        "UDP recv error logging suppressed after {} errors",
                        MAX_LOGGED_ERRORS
                    );
                }
                continue;
            }
            Err(_timeout) => {
                continue;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Connected mode handlers
// ---------------------------------------------------------------------------

/// Dispatch to the appropriate connected UDP handler based on session mode.
#[allow(clippy::too_many_arguments)]
async fn dispatch_connected_udp(
    socket: compio::net::UdpSocket,
    mode: UdpSessionMode,
    shutdown: &AtomicBool,
    pool: &BufPool,
    idle_timeout: Duration,
    metrics: &WorkerMetrics,
    udp_drop_rate: u32,
    udp_latency_us: u64,
    udp_pacing_bps: u64,
) {
    match mode {
        UdpSessionMode::Echo => {
            handle_udp_echo(
                socket,
                shutdown,
                pool,
                idle_timeout,
                metrics,
                udp_drop_rate,
                udp_latency_us,
                udp_pacing_bps,
            )
            .await;
        }
        UdpSessionMode::Rr {
            response_size,
            latency_us,
            error_rate,
        } => {
            handle_udp_rr(
                socket,
                shutdown,
                pool,
                idle_timeout,
                metrics,
                response_size,
                latency_us,
                error_rate,
                udp_drop_rate,
                udp_latency_us,
                udp_pacing_bps,
            )
            .await;
        }
        UdpSessionMode::Sink => {
            handle_udp_sink(socket, shutdown, pool, idle_timeout, metrics).await;
        }
        UdpSessionMode::Source {
            response_size,
            latency_us,
        } => {
            handle_udp_source(
                socket,
                shutdown,
                pool,
                idle_timeout,
                metrics,
                response_size,
                latency_us,
                udp_pacing_bps,
            )
            .await;
        }
        UdpSessionMode::Bidir {
            response_size,
            latency_us,
        } => {
            handle_udp_bidir(
                socket,
                shutdown,
                pool,
                idle_timeout,
                metrics,
                response_size,
                latency_us,
                udp_pacing_bps,
            )
            .await;
        }
    }
}

/// Echo handler: recv datagrams and echo them back. Original behavior.
#[allow(clippy::too_many_arguments)]
async fn handle_udp_echo(
    socket: compio::net::UdpSocket,
    shutdown: &AtomicBool,
    pool: &BufPool,
    idle_timeout: Duration,
    metrics: &WorkerMetrics,
    udp_drop_rate: u32,
    udp_latency_us: u64,
    udp_pacing_bps: u64,
) {
    let mut last_activity = Instant::now();
    let mut rng = SmallRng::seed_from_u64(0xBEEF_CAFE);
    let mut bucket = if udp_pacing_bps > 0 {
        Some(TokenBucket::new(udp_pacing_bps))
    } else {
        None
    };

    loop {
        if shutdown.load(Ordering::Relaxed) || last_activity.elapsed() > idle_timeout {
            break;
        }

        let buf = pool.take();
        let recv_result = compio::time::timeout(Duration::from_millis(500), socket.recv(buf)).await;

        match recv_result {
            Ok(BufResult(Ok(n), recv_buf)) => {
                if n == 0 {
                    pool.give(recv_buf);
                    break;
                }
                last_activity = Instant::now();
                metrics.inc_datagrams_received();
                metrics.add_bytes_received(n as u64);

                if udp_drop_rate > 0 && rng.gen_range(0..10000u32) < udp_drop_rate {
                    metrics.inc_datagrams_dropped();
                    pool.give(recv_buf);
                    continue;
                }

                if udp_latency_us > 0 {
                    compio::time::sleep(Duration::from_micros(udp_latency_us)).await;
                }

                if let Some(ref mut bucket) = bucket {
                    if let Some(delay) = bucket.try_consume(n) {
                        compio::time::sleep(delay).await;
                    }
                }

                let mut send_buf = pool.take();
                send_buf.truncate(n);
                if send_buf.len() < n {
                    send_buf.resize(n, 0);
                }
                send_buf[..n].copy_from_slice(&recv_buf[..n]);
                pool.give(recv_buf);
                let BufResult(result, returned) = socket.send(send_buf).await;
                pool.give(returned);
                if result.is_ok() {
                    metrics.inc_datagrams_sent();
                    metrics.add_bytes_sent(n as u64);
                } else {
                    metrics.inc_datagrams_dropped();
                }
            }
            Ok(BufResult(Err(_), recv_buf)) => {
                pool.give(recv_buf);
                continue;
            }
            Err(_timeout) => {
                continue;
            }
        }
    }
}

/// RR handler: receive datagrams, respond with `response_size`-byte responses.
#[allow(clippy::too_many_arguments)]
async fn handle_udp_rr(
    socket: compio::net::UdpSocket,
    shutdown: &AtomicBool,
    pool: &BufPool,
    idle_timeout: Duration,
    metrics: &WorkerMetrics,
    response_size: u32,
    latency_us: Option<u32>,
    error_rate: Option<u32>,
    udp_drop_rate: u32,
    udp_latency_us: u64,
    udp_pacing_bps: u64,
) {
    let mut last_activity = Instant::now();
    let mut rng = SmallRng::seed_from_u64(0xBEEF_CAFE);
    let mut bucket = if udp_pacing_bps > 0 {
        Some(TokenBucket::new(udp_pacing_bps))
    } else {
        None
    };

    // Pre-allocate a reusable response buffer.
    let resp_size = (response_size as usize).min(MAX_UDP_PAYLOAD);
    let mut resp_buf = pool.take();
    if resp_buf.len() > resp_size {
        resp_buf.truncate(resp_size);
    } else if resp_buf.len() < resp_size {
        resp_buf.resize(resp_size, 0xAA);
    }

    loop {
        if shutdown.load(Ordering::Relaxed) || last_activity.elapsed() > idle_timeout {
            break;
        }

        let buf = pool.take();
        let recv_result = compio::time::timeout(Duration::from_millis(500), socket.recv(buf)).await;

        match recv_result {
            Ok(BufResult(Ok(n), recv_buf)) => {
                if n == 0 {
                    pool.give(recv_buf);
                    break;
                }
                last_activity = Instant::now();
                metrics.inc_datagrams_received();
                metrics.add_bytes_received(n as u64);
                pool.give(recv_buf);

                if udp_drop_rate > 0 && rng.gen_range(0..10000u32) < udp_drop_rate {
                    metrics.inc_datagrams_dropped();
                    continue;
                }

                // Use header's latency if present, otherwise config-level.
                let lat = latency_us.map(|us| us as u64).unwrap_or(udp_latency_us);
                if lat > 0 {
                    compio::time::sleep(Duration::from_micros(lat)).await;
                }

                // Error injection: send half-size 0xEE response.
                if should_inject_error(error_rate, &mut rng) {
                    let err_buf = build_udp_error_response(response_size);
                    let send_size = err_buf.len();
                    let BufResult(_, _) = socket.send(err_buf).await;
                    metrics.inc_errors();
                    metrics.inc_datagrams_sent();
                    metrics.add_bytes_sent(send_size as u64);
                    metrics.inc_requests_completed();
                    continue;
                }

                if let Some(ref mut bucket) = bucket {
                    if let Some(delay) = bucket.try_consume(resp_size) {
                        compio::time::sleep(delay).await;
                    }
                }

                // Send response_size bytes. write_all returns the buffer
                // unchanged for compio, so we reuse it across iterations.
                let BufResult(result, returned) = socket.send(resp_buf).await;
                resp_buf = returned;
                if result.is_ok() {
                    metrics.inc_datagrams_sent();
                    metrics.add_bytes_sent(resp_size as u64);
                } else {
                    metrics.inc_datagrams_dropped();
                }
                metrics.inc_requests_completed();
            }
            Ok(BufResult(Err(_), recv_buf)) => {
                pool.give(recv_buf);
                continue;
            }
            Err(_timeout) => {
                continue;
            }
        }
    }
    pool.give(resp_buf);
}

/// SINK handler: receive and discard all datagrams. Never send.
async fn handle_udp_sink(
    socket: compio::net::UdpSocket,
    shutdown: &AtomicBool,
    pool: &BufPool,
    idle_timeout: Duration,
    metrics: &WorkerMetrics,
) {
    let mut last_activity = Instant::now();

    loop {
        if shutdown.load(Ordering::Relaxed) || last_activity.elapsed() > idle_timeout {
            break;
        }

        let buf = pool.take();
        let recv_result = compio::time::timeout(Duration::from_millis(500), socket.recv(buf)).await;

        match recv_result {
            Ok(BufResult(Ok(n), recv_buf)) => {
                pool.give(recv_buf);
                if n == 0 {
                    break;
                }
                last_activity = Instant::now();
                metrics.inc_datagrams_received();
                metrics.add_bytes_received(n as u64);
            }
            Ok(BufResult(Err(_), recv_buf)) => {
                pool.give(recv_buf);
                continue;
            }
            Err(_timeout) => {
                continue;
            }
        }
    }
}

/// SOURCE handler: send `response_size`-byte datagrams continuously.
#[allow(clippy::too_many_arguments)]
async fn handle_udp_source(
    socket: compio::net::UdpSocket,
    shutdown: &AtomicBool,
    pool: &BufPool,
    _idle_timeout: Duration,
    metrics: &WorkerMetrics,
    response_size: u32,
    latency_us: Option<u32>,
    udp_pacing_bps: u64,
) {
    let mut bucket = if udp_pacing_bps > 0 {
        Some(TokenBucket::new(udp_pacing_bps))
    } else {
        None
    };

    let resp_size = (response_size as usize).min(MAX_UDP_PAYLOAD);
    let mut buf = pool.take();
    if buf.len() > resp_size {
        buf.truncate(resp_size);
    } else if buf.len() < resp_size {
        buf.resize(resp_size, 0xAA);
    }

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        if let Some(us) = latency_us {
            compio::time::sleep(Duration::from_micros(us as u64)).await;
        }

        if let Some(ref mut bucket) = bucket {
            if let Some(delay) = bucket.try_consume(resp_size) {
                compio::time::sleep(delay).await;
            }
        }

        let BufResult(result, returned) = socket.send(buf).await;
        buf = returned;
        if result.is_err() {
            break;
        }
        metrics.inc_datagrams_sent();
        metrics.add_bytes_sent(resp_size as u64);
    }
    pool.give(buf);
}

/// BIDIR handler: concurrent SOURCE (send continuously) + SINK (recv and discard).
///
/// Two interleaved tasks on the same compio runtime: one sends `response_size`
/// datagrams continuously, the other receives and discards incoming datagrams.
#[allow(clippy::too_many_arguments)]
async fn handle_udp_bidir(
    socket: compio::net::UdpSocket,
    shutdown: &AtomicBool,
    pool: &BufPool,
    idle_timeout: Duration,
    metrics: &WorkerMetrics,
    response_size: u32,
    latency_us: Option<u32>,
    udp_pacing_bps: u64,
) {
    use std::cell::Cell;

    let stop = Rc::new(Cell::new(false));

    // Clone socket: SINK task gets the clone, SOURCE runs inline with the original.
    let recv_socket = socket.clone();

    // SINK task: recv and discard.
    let stop_sink = Rc::clone(&stop);
    let sink_handle = {
        let sd_flag = shutdown as *const AtomicBool;
        let sd_ref = unsafe { &*sd_flag };
        let pool_ptr = pool as *const BufPool;
        let pool_ref = unsafe { &*pool_ptr };
        let metrics_ptr = metrics as *const WorkerMetrics;
        compio::runtime::spawn(async move {
            let m = unsafe { &*metrics_ptr };
            let mut last_activity = Instant::now();
            loop {
                if sd_ref.load(Ordering::Relaxed)
                    || stop_sink.get()
                    || last_activity.elapsed() > idle_timeout
                {
                    stop_sink.set(true);
                    break;
                }

                let buf = pool_ref.take();
                let recv_result =
                    compio::time::timeout(Duration::from_millis(500), recv_socket.recv(buf)).await;

                match recv_result {
                    Ok(BufResult(Ok(n), recv_buf)) => {
                        pool_ref.give(recv_buf);
                        if n == 0 {
                            stop_sink.set(true);
                            break;
                        }
                        last_activity = Instant::now();
                        m.inc_datagrams_received();
                        m.add_bytes_received(n as u64);
                    }
                    Ok(BufResult(Err(_), recv_buf)) => {
                        pool_ref.give(recv_buf);
                        stop_sink.set(true);
                        break;
                    }
                    Err(_timeout) => {
                        continue;
                    }
                }
            }
        })
    };

    // SOURCE task: send continuously (runs inline on this task).
    {
        let mut bucket = if udp_pacing_bps > 0 {
            Some(TokenBucket::new(udp_pacing_bps))
        } else {
            None
        };

        let resp_size = (response_size as usize).min(MAX_UDP_PAYLOAD);
        let mut buf = pool.take();
        if buf.len() > resp_size {
            buf.truncate(resp_size);
        } else if buf.len() < resp_size {
            buf.resize(resp_size, 0xAA);
        }

        loop {
            if shutdown.load(Ordering::Relaxed) || stop.get() {
                pool.give(buf);
                break;
            }

            if let Some(us) = latency_us {
                compio::time::sleep(Duration::from_micros(us as u64)).await;
            }

            if let Some(ref mut bucket) = bucket {
                if let Some(delay) = bucket.try_consume(resp_size) {
                    compio::time::sleep(delay).await;
                }
            }

            let BufResult(result, returned) = socket.send(buf).await;
            buf = returned;
            if result.is_err() {
                stop.set(true);
                pool.give(buf);
                break;
            }
            metrics.inc_datagrams_sent();
            metrics.add_bytes_sent(resp_size as u64);
        }
    }

    // Wait for the SINK task to finish.
    let _ = sink_handle.await;
}
