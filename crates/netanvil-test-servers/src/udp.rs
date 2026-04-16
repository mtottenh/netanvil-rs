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

/// Unconnected UDP echo loop — original behavior.
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

/// Connected UDP echo loop — per-client connected sockets with idle cleanup.
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
            tracing::info!("UDP connected echo loop shutting down");
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
                    // New peer: echo first packet on listener, then spawn connected handler.
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

                    // Create connected socket
                    match crate::create_connected_udp(local_port, peer, 4 * 1024 * 1024) {
                        Ok(std_socket) => {
                            match compio::net::UdpSocket::from_std(std_socket) {
                                Ok(connected) => {
                                    active_clients.borrow_mut().insert(peer);
                                    let clients = Rc::clone(&active_clients);
                                    let sd_flag = shutdown as *const AtomicBool;
                                    // SAFETY: shutdown lives for the lifetime of the echo loop,
                                    // and all spawned tasks are on the same single-threaded runtime.
                                    let sd_ref = unsafe { &*sd_flag };
                                    let handler_pool = pool as *const BufPool;
                                    let handler_pool_ref = unsafe { &*handler_pool };
                                    let metrics_ptr = metrics as *const WorkerMetrics;
                                    compio::runtime::spawn(async move {
                                        // SAFETY: metrics outlives all spawned tasks (same
                                        // argument as shutdown and pool raw pointers above).
                                        let m = unsafe { &*metrics_ptr };
                                        handle_connected_udp_client(
                                            connected,
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
                            // Already echoed on listener above — no additional action needed.
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

/// Handler for a single connected UDP client. Runs recv()/send() in a loop
/// until idle timeout or shutdown.
#[allow(clippy::too_many_arguments)]
async fn handle_connected_udp_client(
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
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        if last_activity.elapsed() > idle_timeout {
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
