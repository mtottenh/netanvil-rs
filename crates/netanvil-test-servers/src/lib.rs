//! Compio-based test servers for netanvil-rs integration testing.
//!
//! Provides embeddable TCP, UDP, and DNS echo servers that use the same
//! thread-per-core, io_uring architecture as the load generator.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::metrics::{ServerMetricsSummary, WorkerMetrics};
use crate::reporter::ReportMode;

pub mod bufpool;
pub mod dns;
pub mod metrics;
pub mod protocol;
pub mod reporter;
pub mod tcp;
pub mod udp;

/// Configuration for test servers.
///
/// Provides socket tuning and connection management parameters with sensible
/// defaults. Use `Default::default()` for standard values, or customize
/// individual fields as needed.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Listen address (e.g. "127.0.0.1:0" for random port).
    pub addr: String,
    /// Maximum simultaneous TCP connections (0 = unlimited).
    pub max_connections: usize,
    /// SO_RCVBUF size in bytes.
    pub recv_buf_size: usize,
    /// SO_SNDBUF size in bytes.
    pub send_buf_size: usize,
    /// Number of worker threads (0 = auto-detect, 1 = single-worker default).
    pub num_workers: usize,
    /// Pin each worker to a CPU core via core_affinity.
    pub pin_cores: bool,
    /// Optional data pattern to tile across write buffers.
    /// If None, buffers are filled with deterministic PRNG data.
    /// Loaded from `--fill-data /path/to/file` on the CLI.
    pub fill_pattern: Option<Vec<u8>>,
    /// Idle timeout for connected UDP sockets (seconds).
    /// 0 = disabled (use unconnected recvfrom/sendto echo — backward compat).
    /// When > 0, the server creates per-client connected sockets for kernel-level
    /// demuxing and route caching.
    pub udp_idle_timeout_secs: u32,
    /// Metrics reporting mode (None, Text, Json). Default: None.
    pub report_mode: ReportMode,
    /// Metrics reporting interval in seconds. Default: 1.0.
    pub report_interval_secs: f64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:0".to_string(),
            max_connections: 10_000,
            recv_buf_size: 256 * 1024, // 256 KiB
            send_buf_size: 256 * 1024, // 256 KiB
            num_workers: 1,
            pin_cores: true,
            fill_pattern: None,
            udp_idle_timeout_secs: 0,
            report_mode: ReportMode::None,
            report_interval_secs: 1.0,
        }
    }
}

impl ServerConfig {
    /// Create a UDP/DNS-tuned config with 4 MiB buffers.
    pub fn udp_default() -> Self {
        Self {
            recv_buf_size: 4 * 1024 * 1024, // 4 MiB
            send_buf_size: 4 * 1024 * 1024, // 4 MiB
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// TestServer: multi-protocol, multi-worker server
// ---------------------------------------------------------------------------

/// Handle to a running multi-protocol test server.
///
/// Manages N worker threads, each with its own compio runtime and sockets
/// bound to the same ports via SO_REUSEPORT. Dropping the handle shuts down
/// all workers and joins their threads.
pub struct TestServer {
    tcp_addr: Option<SocketAddr>,
    udp_addr: Option<SocketAddr>,
    dns_addr: Option<SocketAddr>,
    workers: Vec<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    worker_metrics: Vec<Arc<WorkerMetrics>>,
}

impl TestServer {
    pub fn builder() -> TestServerBuilder {
        TestServerBuilder {
            tcp_port: None,
            udp_port: None,
            dns_port: None,
            config: ServerConfig::default(),
        }
    }

    pub fn tcp_addr(&self) -> Option<SocketAddr> {
        self.tcp_addr
    }

    pub fn udp_addr(&self) -> Option<SocketAddr> {
        self.udp_addr
    }

    pub fn dns_addr(&self) -> Option<SocketAddr> {
        self.dns_addr
    }

    /// Read the current aggregate metrics across all workers.
    ///
    /// Returns a point-in-time snapshot — counters are read with `Relaxed`
    /// ordering so individual fields may be from slightly different instants,
    /// but each counter is internally consistent.
    pub fn metrics_snapshot(&self) -> ServerMetricsSummary {
        let snapshots: Vec<_> = self.worker_metrics.iter().map(|m| m.snapshot()).collect();
        ServerMetricsSummary::from_snapshots(&snapshots)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

/// Builder for configuring and starting a [`TestServer`].
pub struct TestServerBuilder {
    tcp_port: Option<u16>,
    udp_port: Option<u16>,
    dns_port: Option<u16>,
    config: ServerConfig,
}

impl TestServerBuilder {
    /// Enable TCP on the given port (0 = random OS-assigned port).
    pub fn tcp(mut self, port: u16) -> Self {
        self.tcp_port = Some(port);
        self
    }

    /// Enable UDP on the given port (0 = random OS-assigned port).
    pub fn udp(mut self, port: u16) -> Self {
        self.udp_port = Some(port);
        self
    }

    /// Enable DNS on the given port (0 = random OS-assigned port).
    pub fn dns(mut self, port: u16) -> Self {
        self.dns_port = Some(port);
        self
    }

    /// Set the number of worker threads (0 = auto-detect from available cores).
    pub fn workers(mut self, n: usize) -> Self {
        self.config.num_workers = n;
        self
    }

    /// Enable or disable CPU core pinning (default: true).
    pub fn pin_cores(mut self, pin: bool) -> Self {
        self.config.pin_cores = pin;
        self
    }

    /// Set the idle timeout for connected UDP sockets (seconds).
    /// 0 = disabled (unconnected mode). When > 0, the server creates per-client
    /// connected sockets for kernel-level demuxing.
    pub fn udp_idle_timeout(mut self, secs: u32) -> Self {
        self.config.udp_idle_timeout_secs = secs;
        self
    }

    /// Set the metrics reporting mode.
    pub fn report_mode(mut self, mode: ReportMode) -> Self {
        self.config.report_mode = mode;
        self
    }

    /// Set the metrics reporting interval in seconds.
    pub fn report_interval(mut self, secs: f64) -> Self {
        self.config.report_interval_secs = secs;
        self
    }

    /// Override the full server configuration.
    pub fn config(mut self, config: ServerConfig) -> Self {
        self.config = config;
        self
    }

    /// Build and start the server. Blocks until all workers are ready.
    pub fn build(self) -> TestServer {
        use compio::net::SocketOpts;

        // Resolve port 0 → actual OS-assigned ports
        let tcp_port = self
            .tcp_port
            .map(|p| if p == 0 { allocate_tcp_port() } else { p });
        let udp_port = self
            .udp_port
            .map(|p| if p == 0 { allocate_udp_port() } else { p });
        let dns_port = self
            .dns_port
            .map(|p| if p == 0 { allocate_udp_port() } else { p });

        let num_workers = if self.config.num_workers == 0 {
            core_affinity::get_core_ids()
                .map(|ids| ids.len())
                .unwrap_or(1)
        } else {
            self.config.num_workers
        };

        let shutdown = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = flume::bounded::<()>(num_workers);
        let mut workers = Vec::with_capacity(num_workers);

        let all_metrics: Vec<Arc<WorkerMetrics>> = (0..num_workers)
            .map(|_| Arc::new(WorkerMetrics::new()))
            .collect();

        for (i, worker_metrics_arc) in all_metrics.iter().enumerate().take(num_workers) {
            let shutdown = shutdown.clone();
            let config = self.config.clone();
            let ready_tx = ready_tx.clone();
            let worker_metrics = worker_metrics_arc.clone();

            let thread = std::thread::Builder::new()
                .name(format!("test-server-{}", i))
                .spawn(move || {
                    if config.pin_cores {
                        let core = core_affinity::CoreId { id: i };
                        if !core_affinity::set_for_current(core) {
                            tracing::debug!(worker = i, "failed to pin test server worker");
                        }
                    }

                    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
                    rt.block_on(async {
                        // TCP listener with SO_REUSEPORT
                        let tcp_listener = if let Some(port) = tcp_port {
                            let opts = SocketOpts::new()
                                .reuse_address(true)
                                .reuse_port(true)
                                .recv_buffer_size(config.recv_buf_size)
                                .send_buffer_size(config.send_buf_size);
                            let addr = format!("127.0.0.1:{}", port);
                            Some(
                                compio::net::TcpListener::bind_with_options(&addr, &opts)
                                    .await
                                    .unwrap_or_else(|e| {
                                        panic!("worker {i}: TCP bind to {addr} failed: {e}")
                                    }),
                            )
                        } else {
                            None
                        };

                        // UDP socket with SO_REUSEPORT
                        // NOTE: compio's UdpSocket::bind_with_options applies socket opts
                        // AFTER bind, so SO_REUSEPORT isn't set in time for multi-listener.
                        // Work around by creating the socket via socket2 (options before bind)
                        // and converting via from_std().
                        let udp_socket =
                            if let Some(port) = udp_port {
                                let addr = SocketAddr::from(([127, 0, 0, 1], port));
                                let std_socket = bind_udp_reuse_port(addr, 4 * 1024 * 1024)
                                    .unwrap_or_else(|e| {
                                        panic!("worker {i}: UDP bind to {addr} failed: {e}")
                                    });
                                Some(compio::net::UdpSocket::from_std(std_socket).unwrap_or_else(
                                    |e| panic!("worker {i}: UDP from_std failed: {e}"),
                                ))
                            } else {
                                None
                            };

                        // DNS socket with SO_REUSEPORT (same workaround as UDP)
                        let dns_socket =
                            if let Some(port) = dns_port {
                                let addr = SocketAddr::from(([127, 0, 0, 1], port));
                                let std_socket = bind_udp_reuse_port(addr, 4 * 1024 * 1024)
                                    .unwrap_or_else(|e| {
                                        panic!("worker {i}: DNS bind to {addr} failed: {e}")
                                    });
                                Some(compio::net::UdpSocket::from_std(std_socket).unwrap_or_else(
                                    |e| panic!("worker {i}: DNS from_std failed: {e}"),
                                ))
                            } else {
                                None
                            };

                        // Signal ready
                        let _ = ready_tx.send(());
                        drop(ready_tx);

                        // Spawn handler tasks (detached — they check shutdown flag)
                        if let Some(listener) = tcp_listener {
                            let sd = shutdown.clone();
                            let cfg = config.clone();
                            let pool = std::rc::Rc::new(crate::tcp::make_pool(&config, 65536, 256));
                            let wm = worker_metrics.clone();
                            compio::runtime::spawn(async move {
                                crate::tcp::tcp_accept_loop(listener, &sd, &cfg, pool, wm).await;
                            })
                            .detach();
                        }

                        if let Some(socket) = udp_socket {
                            let sd = shutdown.clone();
                            let pool = std::rc::Rc::new(crate::tcp::make_pool(&config, 65536, 64));
                            let idle_timeout_secs = config.udp_idle_timeout_secs;
                            let local_port = udp_port.unwrap();
                            let wm = worker_metrics.clone();
                            compio::runtime::spawn(async move {
                                crate::udp::udp_echo_loop(
                                    socket,
                                    &sd,
                                    &pool,
                                    idle_timeout_secs,
                                    local_port,
                                    &wm,
                                )
                                .await;
                            })
                            .detach();
                        }

                        if let Some(socket) = dns_socket {
                            let sd = shutdown.clone();
                            let pool = std::rc::Rc::new(crate::tcp::make_pool(&config, 4096, 32));
                            let idle_timeout_secs = config.udp_idle_timeout_secs;
                            let local_port = dns_port.unwrap();
                            let wm = worker_metrics.clone();
                            compio::runtime::spawn(async move {
                                crate::dns::dns_echo_loop(
                                    socket,
                                    &sd,
                                    &pool,
                                    idle_timeout_secs,
                                    local_port,
                                    &wm,
                                )
                                .await;
                            })
                            .detach();
                        }

                        // Keep runtime alive until shutdown
                        while !shutdown.load(Ordering::Relaxed) {
                            compio::time::sleep(Duration::from_millis(100)).await;
                        }
                        // Give handlers time to finish current operation
                        compio::time::sleep(Duration::from_millis(200)).await;
                    });
                })
                .expect("failed to spawn test server worker");

            workers.push(thread);
        }

        drop(ready_tx);

        // Wait for all workers to bind and be ready
        for _ in 0..num_workers {
            ready_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("test server worker failed to start within 5s");
        }

        let tcp_addr = tcp_port.map(|p| SocketAddr::from(([127, 0, 0, 1], p)));
        let udp_addr = udp_port.map(|p| SocketAddr::from(([127, 0, 0, 1], p)));
        let dns_addr = dns_port.map(|p| SocketAddr::from(([127, 0, 0, 1], p)));

        TestServer {
            tcp_addr,
            udp_addr,
            dns_addr,
            workers,
            shutdown,
            worker_metrics: all_metrics,
        }
    }
}

/// Allocate a random TCP port by binding to port 0 and reading the assigned port.
fn allocate_tcp_port() -> u16 {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("failed to allocate random TCP port");
    listener.local_addr().unwrap().port()
}

/// Allocate a random UDP port by binding to port 0 and reading the assigned port.
fn allocate_udp_port() -> u16 {
    let socket =
        std::net::UdpSocket::bind("127.0.0.1:0").expect("failed to allocate random UDP port");
    socket.local_addr().unwrap().port()
}

/// Create a UDP socket with SO_REUSEPORT set BEFORE bind.
///
/// compio's `UdpSocket::bind_with_options` applies socket options after bind,
/// which means SO_REUSEPORT isn't set when the kernel checks during bind().
/// This helper uses `socket2` to set options in the correct order, then
/// converts to a `std::net::UdpSocket` for use with `compio::net::UdpSocket::from_std`.
fn bind_udp_reuse_port(addr: SocketAddr, buf_size: usize) -> std::io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;
    socket.set_recv_buffer_size(buf_size)?;
    socket.set_send_buffer_size(buf_size)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

/// Create a connected UDP socket bound to `local_port` and connected to `peer`.
///
/// Uses SO_REUSEPORT so the kernel routes packets from `peer` to this socket
/// (4-tuple match wins over 2-tuple on the listener). The socket is created
/// via socket2 with options set before bind, then connected before conversion
/// to `std::net::UdpSocket`.
pub(crate) fn create_connected_udp(
    local_port: u16,
    peer: SocketAddr,
    buf_size: usize,
) -> std::io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;
    socket.set_recv_buffer_size(buf_size)?;
    socket.set_send_buffer_size(buf_size)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from(([0, 0, 0, 0], local_port)).into())?;
    socket.connect(&peer.into())?;
    Ok(socket.into())
}
