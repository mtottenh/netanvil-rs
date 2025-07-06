//! UDP echo server using compio's io_uring-backed runtime.
//!
//! Spawns a background thread running a compio runtime that receives UDP
//! datagrams and echoes them back to the sender.

use std::net::SocketAddr;
use std::time::Duration;

use compio::buf::BufResult;

/// Handle to a running UDP echo server.
///
/// Dropping the handle sends a shutdown signal and joins the server thread.
pub struct UdpEchoHandle {
    /// The address the server is listening on.
    pub addr: SocketAddr,
    shutdown_tx: Option<flume::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for UdpEchoHandle {
    fn drop(&mut self) {
        // Signal shutdown
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Join the thread
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
    let (shutdown_tx, shutdown_rx) = flume::bounded::<()>(1);
    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();
    let listen_addr = addr.to_string();

    let thread = std::thread::Builder::new()
        .name("udp-echo-server".into())
        .spawn(move || {
            let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
            rt.block_on(run_udp_echo(shutdown_rx, addr_tx, &listen_addr));
        })
        .expect("failed to spawn UDP echo server thread");

    let addr = addr_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("UDP echo server failed to start within 5s");

    UdpEchoHandle {
        addr,
        shutdown_tx: Some(shutdown_tx),
        thread: Some(thread),
    }
}

async fn run_udp_echo(
    shutdown_rx: flume::Receiver<()>,
    addr_tx: std::sync::mpsc::Sender<SocketAddr>,
    listen_addr: &str,
) {
    let socket = compio::net::UdpSocket::bind(listen_addr).await.unwrap();
    let local_addr = socket.local_addr().unwrap();
    addr_tx.send(local_addr).unwrap();

    tracing::info!("UDP echo server listening on {}", local_addr);

    loop {
        // Check for shutdown
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("UDP echo server shutting down");
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
                tracing::warn!("UDP recv error: {}", e);
                break;
            }
            Err(_timeout) => {
                // Timeout — loop back and check shutdown
                continue;
            }
        }
    }
}
