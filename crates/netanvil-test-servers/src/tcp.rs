//! TCP test server using compio's io_uring-backed runtime.
//!
//! Spawns a background thread running a compio runtime that accepts TCP
//! connections. Each connection is inspected for a protocol header (8 bytes);
//! if a valid header is found, the connection is dispatched to the
//! corresponding mode handler (RR, SINK, SOURCE, BIDIR). Otherwise the
//! server falls back to plain echo mode for backward compatibility.

use std::cell::Cell;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use compio::buf::BufResult;
use compio::net::SocketOpts;

use crate::protocol;
use crate::ServerConfig;

/// Handle to a running TCP echo server.
///
/// Dropping the handle sends a shutdown signal and joins the server thread.
pub struct TcpEchoHandle {
    /// The address the server is listening on.
    pub addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for TcpEchoHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
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
    start_tcp_echo_with_config(ServerConfig {
        addr: addr.to_string(),
        ..Default::default()
    })
}

/// Start a TCP echo server with full configuration.
pub fn start_tcp_echo_with_config(config: ServerConfig) -> TcpEchoHandle {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();
    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();

    let thread = std::thread::Builder::new()
        .name("tcp-echo-server".into())
        .spawn(move || {
            let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
            rt.block_on(async {
                let listener_opts = SocketOpts::new()
                    .reuse_address(true)
                    .recv_buffer_size(config.recv_buf_size)
                    .send_buffer_size(config.send_buf_size);

                let listener =
                    compio::net::TcpListener::bind_with_options(&config.addr, &listener_opts)
                        .await
                        .unwrap();
                let local_addr = listener.local_addr().unwrap();
                addr_tx.send(local_addr).unwrap();

                tracing::info!("TCP echo server listening on {}", local_addr);

                tcp_accept_loop(listener, &shutdown_clone, &config).await;
            });
        })
        .expect("failed to spawn TCP echo server thread");

    let addr = addr_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("TCP echo server failed to start within 5s");

    TcpEchoHandle {
        addr,
        shutdown,
        thread: Some(thread),
    }
}

/// Core TCP accept loop. Takes a pre-bound listener and runs until shutdown.
///
/// Used by both `start_tcp_echo_with_config` (single-worker) and
/// `TestServer` (multi-worker with SO_REUSEPORT).
pub(crate) async fn tcp_accept_loop(
    listener: compio::net::TcpListener,
    shutdown: &AtomicBool,
    config: &ServerConfig,
) {
    let stream_opts = SocketOpts::new()
        .nodelay(true)
        .recv_buffer_size(config.recv_buf_size)
        .send_buffer_size(config.send_buf_size);

    let active_connections = Rc::new(Cell::new(0u32));
    let max_connections = config.max_connections;
    let limit_logged = Cell::new(false);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("TCP accept loop shutting down");
            break;
        }

        let accept_result = compio::time::timeout(
            Duration::from_millis(100),
            listener.accept_with_options(&stream_opts),
        )
        .await;

        match accept_result {
            Ok(Ok((stream, _peer))) => {
                if max_connections > 0 && active_connections.get() as usize >= max_connections {
                    if !limit_logged.get() {
                        tracing::warn!(
                            "TCP connection limit reached ({}), dropping new connections",
                            max_connections
                        );
                        limit_logged.set(true);
                    }
                    drop(stream);
                    continue;
                }

                active_connections.set(active_connections.get() + 1);
                let counter = Rc::clone(&active_connections);
                compio::runtime::spawn(async move {
                    handle_tcp_connection(stream).await;
                    counter.set(counter.get() - 1);
                })
                .detach();
            }
            Ok(Err(e)) => {
                tracing::warn!("TCP accept error: {}", e);
            }
            Err(_timeout) => {
                continue;
            }
        }
    }
}

async fn handle_tcp_connection(mut stream: compio::net::TcpStream) {
    use compio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

    // Read initial data into a large buffer. This avoids blocking on
    // read_exact when the client sends fewer than HEADER_SIZE bytes (which
    // would deadlock with echo clients waiting for a response).
    let buf = vec![0u8; 65536];
    let BufResult(result, buf) = stream.read(buf).await;
    let n = match result {
        Ok(0) => return, // EOF immediately
        Ok(n) => n,
        Err(_) => return,
    };

    // Quick check: is the first byte a valid protocol mode?
    let first_byte = buf[0];
    let could_be_protocol = matches!(
        first_byte,
        protocol::MODE_RR | protocol::MODE_SINK | protocol::MODE_SOURCE | protocol::MODE_BIDIR
    );

    if could_be_protocol && n >= protocol::HEADER_SIZE {
        // We have enough bytes to parse the header right away.
        if let Some(header) = protocol::parse_header(&buf[..protocol::HEADER_SIZE]) {
            // Any extra bytes beyond the header are the start of payload data.
            let leftover = buf[protocol::HEADER_SIZE..n].to_vec();
            dispatch_protocol(&mut stream, header, leftover).await;
            return;
        }
        // parse_header returned None despite valid first byte — fall through to echo.
    } else if could_be_protocol && n < protocol::HEADER_SIZE {
        // First byte looks like a mode byte but we don't have 8 bytes yet.
        // Accumulate the rest of the header before deciding.
        let mut header_bytes = buf[..n].to_vec();
        while header_bytes.len() < protocol::HEADER_SIZE {
            let remaining = protocol::HEADER_SIZE - header_bytes.len();
            let tmp = vec![0u8; remaining];
            let result = stream.read_exact(tmp).await;
            match result.0 {
                Ok(_) => {
                    header_bytes.extend_from_slice(&result.1);
                }
                Err(_) => {
                    // Connection closed before full header — echo what we got.
                    let data = header_bytes;
                    let BufResult(result, _) = stream.write_all(data).await;
                    if result.is_err() {
                        return;
                    }
                    handle_echo_loop(&mut stream).await;
                    return;
                }
            }
        }
        if let Some(header) = protocol::parse_header(&header_bytes) {
            dispatch_protocol(&mut stream, header, Vec::new()).await;
            return;
        }
        // Not a valid header after all — echo back the accumulated bytes.
        let BufResult(result, _) = stream.write_all(header_bytes).await;
        if result.is_err() {
            return;
        }
        handle_echo_loop(&mut stream).await;
        return;
    }

    // Not a protocol header — echo back what we received, then continue
    // in echo loop.
    let data = buf[..n].to_vec();
    let BufResult(result, _) = stream.write_all(data).await;
    if result.is_err() {
        return;
    }
    handle_echo_loop(&mut stream).await;
}

/// Dispatch to the appropriate protocol mode handler.
async fn dispatch_protocol(
    stream: &mut compio::net::TcpStream,
    header: protocol::ProtocolHeader,
    leftover: Vec<u8>,
) {
    match header.mode {
        protocol::MODE_RR => {
            handle_rr(stream, header.request_size, header.response_size, leftover).await;
        }
        protocol::MODE_SINK => {
            handle_sink(stream).await;
        }
        protocol::MODE_SOURCE => {
            handle_source(stream, header.response_size).await;
        }
        protocol::MODE_BIDIR => {
            handle_bidir(stream, header.request_size, header.response_size, leftover).await;
        }
        _ => {}
    }
}

/// RR mode: read request_size bytes, write response_size bytes, repeat.
///
/// `leftover` contains any bytes read beyond the protocol header that belong
/// to the first request payload.
async fn handle_rr(
    stream: &mut compio::net::TcpStream,
    request_size: u16,
    response_size: u32,
    leftover: Vec<u8>,
) {
    use compio::io::{AsyncReadExt, AsyncWriteExt};

    let response_buf = vec![0u8; response_size as usize]; // pre-allocated
    let mut first = true;

    loop {
        // Read exactly request_size bytes
        if request_size > 0 {
            if first && !leftover.is_empty() {
                first = false;
                // We already have some bytes from the initial read. If we
                // have the full request, consume it. Otherwise read the rest.
                let need = request_size as usize;
                if leftover.len() < need {
                    let remaining = need - leftover.len();
                    let tmp = vec![0u8; remaining];
                    let result = stream.read_exact(tmp).await;
                    if result.0.is_err() {
                        break;
                    }
                }
                // leftover.len() >= need: extra bytes are lost (unlikely in
                // a well-behaved client that sends exactly request_size).
            } else {
                first = false;
                let req_buf = vec![0u8; request_size as usize];
                let result = stream.read_exact(req_buf).await;
                if result.0.is_err() {
                    break;
                }
            }
        } else {
            first = false;
        }

        // Write exactly response_size bytes
        if response_size > 0 {
            // write_all consumes the buffer, so clone for each iteration
            let BufResult(result, _) = stream.write_all(response_buf.clone()).await;
            if result.is_err() {
                break;
            }
        }
    }
}

/// SINK mode: read and discard all incoming data.
async fn handle_sink(stream: &mut compio::net::TcpStream) {
    use compio::io::AsyncRead;

    let mut buf = vec![0u8; 65536];
    loop {
        let BufResult(result, b) = stream.read(buf).await;
        buf = b;
        match result {
            Ok(0) => break,    // EOF
            Ok(_) => continue, // discard
            Err(_) => break,
        }
    }
}

/// SOURCE mode: write response_size-byte chunks continuously.
async fn handle_source(stream: &mut compio::net::TcpStream, chunk_size: u32) {
    use compio::io::AsyncWriteExt;

    let chunk = vec![0u8; chunk_size as usize]; // pre-allocated

    loop {
        // write_all consumes the buffer, so clone for each iteration
        let BufResult(result, _) = stream.write_all(chunk.clone()).await;
        if result.is_err() {
            break;
        }
    }
}

/// BIDIR mode: read and write simultaneously.
///
/// For v1: alternate read/write like RR. A full implementation would use
/// `try_clone()` for concurrent read/write.
async fn handle_bidir(
    stream: &mut compio::net::TcpStream,
    request_size: u16,
    response_size: u32,
    leftover: Vec<u8>,
) {
    handle_rr(stream, request_size, response_size, leftover).await;
}

/// Echo loop: read data, write it back. Used after the initial bytes have
/// already been echoed for non-protocol connections.
async fn handle_echo_loop(stream: &mut compio::net::TcpStream) {
    use compio::io::{AsyncRead, AsyncWriteExt};

    let mut buf = vec![0u8; 65536];
    loop {
        let BufResult(result, b) = stream.read(buf).await;
        buf = b;
        match result {
            Ok(0) => break,
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
