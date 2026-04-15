//! UDP echo server using compio's io_uring-backed runtime.
//!
//! Spawns a background thread running a compio runtime that receives UDP
//! datagrams and echoes them back to the sender.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use compio::buf::BufResult;
use compio::net::SocketOpts;

use crate::ServerConfig;

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
pub fn start_udp_echo() -> UdpEchoHandle {
    start_udp_echo_on("127.0.0.1:0")
}

/// Start a UDP echo server on the given address.
///
/// Use `"127.0.0.1:0"` for a random port or `"127.0.0.1:9000"` for a fixed one.
/// Returns a handle that stops the server when dropped.
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
                addr_tx.send(local_addr).unwrap();

                tracing::info!("UDP echo server listening on {}", local_addr);

                udp_echo_loop(socket, &shutdown_clone).await;
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

/// Core UDP echo loop. Takes a pre-bound socket and runs until shutdown.
///
/// Used by both `start_udp_echo_with_config` (single-worker) and
/// `TestServer` (multi-worker with SO_REUSEPORT).
pub(crate) async fn udp_echo_loop(socket: compio::net::UdpSocket, shutdown: &AtomicBool) {
    let mut error_count: u64 = 0;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("UDP echo loop shutting down");
            break;
        }

        let buf = vec![0u8; 65536];
        let recv_result =
            compio::time::timeout(Duration::from_millis(100), socket.recv_from(buf)).await;

        match recv_result {
            Ok(BufResult(Ok((n, peer)), buf)) => {
                let response = buf[..n].to_vec();
                let _ = socket.send_to(response, peer).await;
            }
            Ok(BufResult(Err(e), _)) => {
                error_count += 1;
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
