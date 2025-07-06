//! TCP echo server using compio's io_uring-backed runtime.
//!
//! Spawns a background thread running a compio runtime that accepts TCP
//! connections and echoes back whatever the client sends.

use std::net::SocketAddr;
use std::time::Duration;

use compio::buf::BufResult;

/// Handle to a running TCP echo server.
///
/// Dropping the handle sends a shutdown signal and joins the server thread.
pub struct TcpEchoHandle {
    /// The address the server is listening on.
    pub addr: SocketAddr,
    shutdown_tx: Option<flume::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for TcpEchoHandle {
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

/// Start a TCP echo server on a random port (127.0.0.1:0).
///
/// Returns a handle that stops the server when dropped.
pub fn start_tcp_echo() -> TcpEchoHandle {
    start_tcp_echo_on("127.0.0.1:0")
}

/// Start a TCP echo server on the given address.
///
/// Use `"127.0.0.1:0"` for a random port or `"127.0.0.1:9000"` for a fixed one.
/// Returns a handle that stops the server when dropped.
pub fn start_tcp_echo_on(addr: &str) -> TcpEchoHandle {
    let (shutdown_tx, shutdown_rx) = flume::bounded::<()>(1);
    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();
    let listen_addr = addr.to_string();

    let thread = std::thread::Builder::new()
        .name("tcp-echo-server".into())
        .spawn(move || {
            let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
            rt.block_on(run_tcp_echo(shutdown_rx, addr_tx, &listen_addr));
        })
        .expect("failed to spawn TCP echo server thread");

    let addr = addr_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("TCP echo server failed to start within 5s");

    TcpEchoHandle {
        addr,
        shutdown_tx: Some(shutdown_tx),
        thread: Some(thread),
    }
}

async fn run_tcp_echo(
    shutdown_rx: flume::Receiver<()>,
    addr_tx: std::sync::mpsc::Sender<SocketAddr>,
    listen_addr: &str,
) {
    let listener = compio::net::TcpListener::bind(listen_addr).await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    addr_tx.send(local_addr).unwrap();

    tracing::info!("TCP echo server listening on {}", local_addr);

    loop {
        // Check for shutdown
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("TCP echo server shutting down");
            break;
        }

        // Accept with a timeout so we can check for shutdown periodically
        let accept_result =
            compio::time::timeout(Duration::from_millis(100), listener.accept()).await;

        match accept_result {
            Ok(Ok((stream, _peer))) => {
                compio::runtime::spawn(handle_tcp_connection(stream)).detach();
            }
            Ok(Err(e)) => {
                tracing::warn!("TCP accept error: {}", e);
            }
            Err(_timeout) => {
                // Timeout — loop back and check shutdown
                continue;
            }
        }
    }
}

async fn handle_tcp_connection(mut stream: compio::net::TcpStream) {
    use compio::io::{AsyncRead, AsyncWriteExt};

    let mut buf = vec![0u8; 65536];
    loop {
        let BufResult(result, b) = stream.read(buf).await;
        buf = b;
        match result {
            Ok(0) => break, // EOF
            Ok(n) => {
                let data = buf[..n].to_vec();
                let BufResult(result, _) = stream.write_all(data).await;
                if result.is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}
