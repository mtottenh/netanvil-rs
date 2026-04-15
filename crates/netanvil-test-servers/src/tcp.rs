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

use crate::bufpool::BufPool;
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

                let pool = Rc::new(make_pool(&config, 65536, 256));
                tcp_accept_loop(listener, &shutdown_clone, &config, pool).await;
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

/// Create a BufPool from config, using pattern fill or PRNG.
pub(crate) fn make_pool(config: &ServerConfig, buf_size: usize, high_water: usize) -> BufPool {
    if let Some(ref pattern) = config.fill_pattern {
        BufPool::with_pattern(buf_size, high_water, pattern)
    } else {
        BufPool::new(buf_size, high_water)
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
    pool: Rc<BufPool>,
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
                let pool = Rc::clone(&pool);
                compio::runtime::spawn(async move {
                    handle_tcp_connection(stream, &pool).await;
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

async fn handle_tcp_connection(mut stream: compio::net::TcpStream, pool: &BufPool) {
    use compio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

    // Read initial data into a pooled buffer.
    // compio read() uses buf capacity, but the pool buffer has
    // capacity == len == buf_size, same as vec![0u8; 65536].
    let buf = pool.take();
    let BufResult(result, buf) = stream.read(buf).await;
    let n = match result {
        Ok(0) => {
            pool.give(buf);
            return;
        }
        Ok(n) => n,
        Err(_) => {
            pool.give(buf);
            return;
        }
    };

    // Quick check: is the first byte a valid protocol mode?
    let first_byte = buf[0];
    let could_be_protocol = matches!(
        first_byte,
        protocol::MODE_RR | protocol::MODE_SINK | protocol::MODE_SOURCE | protocol::MODE_BIDIR
    );

    if could_be_protocol && n >= protocol::HEADER_SIZE {
        if let Some(header) = protocol::parse_header(&buf[..protocol::HEADER_SIZE]) {
            let leftover = buf[protocol::HEADER_SIZE..n].to_vec();
            pool.give(buf);
            dispatch_protocol(&mut stream, header, leftover, pool).await;
            return;
        }
        pool.give(buf);
    } else if could_be_protocol && n < protocol::HEADER_SIZE {
        let mut header_bytes = buf[..n].to_vec();
        pool.give(buf);
        while header_bytes.len() < protocol::HEADER_SIZE {
            let remaining = protocol::HEADER_SIZE - header_bytes.len();
            let tmp = vec![0u8; remaining];
            let result = stream.read_exact(tmp).await;
            match result.0 {
                Ok(_) => {
                    header_bytes.extend_from_slice(&result.1);
                }
                Err(_) => {
                    let data = header_bytes;
                    let BufResult(result, _) = stream.write_all(data).await;
                    if result.is_err() {
                        return;
                    }
                    handle_echo_loop(&mut stream, pool).await;
                    return;
                }
            }
        }
        if let Some(header) = protocol::parse_header(&header_bytes) {
            dispatch_protocol(&mut stream, header, Vec::new(), pool).await;
            return;
        }
        let BufResult(result, _) = stream.write_all(header_bytes).await;
        if result.is_err() {
            return;
        }
        handle_echo_loop(&mut stream, pool).await;
        return;
    } else {
        // Not a protocol header — echo back what we received.
        let data = buf[..n].to_vec();
        pool.give(buf);
        let BufResult(result, _) = stream.write_all(data).await;
        if result.is_err() {
            return;
        }
        handle_echo_loop(&mut stream, pool).await;
    }
}

/// Dispatch to the appropriate protocol mode handler.
async fn dispatch_protocol(
    stream: &mut compio::net::TcpStream,
    header: protocol::ProtocolHeader,
    leftover: Vec<u8>,
    pool: &BufPool,
) {
    match header.mode {
        protocol::MODE_RR => {
            handle_rr(
                stream,
                header.request_size,
                header.response_size,
                leftover,
                pool,
            )
            .await;
        }
        protocol::MODE_SINK => {
            handle_sink(stream, pool).await;
        }
        protocol::MODE_SOURCE => {
            handle_source(stream, header.response_size, pool).await;
        }
        protocol::MODE_BIDIR => {
            handle_bidir(
                stream,
                header.request_size,
                header.response_size,
                leftover,
                pool,
            )
            .await;
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
    pool: &BufPool,
) {
    use compio::io::{AsyncReadExt, AsyncWriteExt};

    let resp_size = response_size as usize;

    // Take one buffer from the pool for the lifetime of this connection.
    // write_all returns the buffer unchanged, so we reuse it directly
    // without clone or per-iteration pool take/give.
    let mut resp_buf = pool.take();
    if resp_buf.len() > resp_size {
        resp_buf.truncate(resp_size);
    } else if resp_buf.len() < resp_size {
        resp_buf.resize(resp_size, 0xAA);
    }

    let mut first = true;

    loop {
        // Read exactly request_size bytes.
        // N.B. read_exact uses buf_capacity() to determine how many bytes to
        // read, so we MUST use a vec whose capacity == request_size, not a
        // truncated pool buffer (whose capacity would be 65 KiB).
        if request_size > 0 {
            if first && !leftover.is_empty() {
                first = false;
                let need = request_size as usize;
                if leftover.len() < need {
                    let remaining = need - leftover.len();
                    let tmp = vec![0u8; remaining];
                    let result = stream.read_exact(tmp).await;
                    if result.0.is_err() {
                        break;
                    }
                }
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

        // Write exactly response_size bytes. write_all returns the buffer
        // unchanged, so we reuse it directly — zero allocations per iteration.
        if response_size > 0 {
            let BufResult(result, returned) = stream.write_all(resp_buf).await;
            resp_buf = returned;
            if result.is_err() {
                break;
            }
        }
    }
    pool.give(resp_buf);
}

/// SINK mode: read and discard all incoming data.
async fn handle_sink(stream: &mut compio::net::TcpStream, pool: &BufPool) {
    use compio::io::AsyncRead;

    let mut buf = pool.take();
    loop {
        let BufResult(result, b) = stream.read(buf).await;
        buf = b;
        match result {
            Ok(0) => break,    // EOF
            Ok(_) => continue, // discard
            Err(_) => break,
        }
    }
    pool.give(buf);
}

/// SOURCE mode: write response_size-byte chunks continuously.
async fn handle_source(stream: &mut compio::net::TcpStream, chunk_size: u32, pool: &BufPool) {
    use compio::io::AsyncWriteExt;

    let size = chunk_size as usize;

    // Take one buffer, reuse across iterations.
    let mut buf = pool.take();
    if buf.len() > size {
        buf.truncate(size);
    } else if buf.len() < size {
        buf.resize(size, 0xAA);
    }

    loop {
        let BufResult(result, returned) = stream.write_all(buf).await;
        buf = returned;
        if result.is_err() {
            break;
        }
    }
    pool.give(buf);
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
    pool: &BufPool,
) {
    handle_rr(stream, request_size, response_size, leftover, pool).await;
}

/// Echo loop: read data, write it back. Used after the initial bytes have
/// already been echoed for non-protocol connections.
async fn handle_echo_loop(stream: &mut compio::net::TcpStream, pool: &BufPool) {
    use compio::io::{AsyncRead, AsyncWriteExt};

    let mut buf = pool.take();
    loop {
        let BufResult(result, b) = stream.read(buf).await;
        buf = b;
        match result {
            Ok(0) => break,
            Ok(n) => {
                // Take a write buffer from pool, copy read data into it.
                let mut write_buf = pool.take();
                write_buf.truncate(n);
                if write_buf.len() < n {
                    write_buf.resize(n, 0);
                }
                write_buf[..n].copy_from_slice(&buf[..n]);

                let BufResult(result, returned) = stream.write_all(write_buf).await;
                pool.give(returned);
                if result.is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    pool.give(buf);
}
