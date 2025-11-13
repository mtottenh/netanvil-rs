//! Redis request executor implementing [`RequestExecutor`].
//!
//! The executor manages a per-core connection pool and dispatches Redis
//! commands encoded as RESP protocol over TCP.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use compio::buf::BufResult;
use compio::io::AsyncWriteExt;
use netanvil_types::redis_spec::RedisRequestSpec;
use netanvil_types::request::{ExecutionError, ExecutionResult, RequestContext, TimingBreakdown};
use netanvil_types::RequestExecutor;

use crate::resp;

/// A single pooled Redis TCP connection with bookkeeping metadata.
#[allow(dead_code)]
struct PooledConnection {
    /// The underlying TCP stream.
    stream: compio::net::TcpStream,
    /// Whether AUTH/SELECT have been sent on this connection.
    initialized: bool,
    /// Number of commands completed on this connection.
    request_count: u32,
    /// When this connection was established.
    created_at: Instant,
}

/// Redis executor with per-core connection pooling.
///
/// Each I/O worker core owns one `RedisExecutor` instance. The internal
/// connection pool is wrapped in `RefCell` because the executor is
/// `Rc`-shared across concurrent spawned tasks on a single core.
/// All `RefCell` borrows are brief and never held across await points.
pub struct RedisExecutor {
    request_timeout: Duration,
    pool: RefCell<VecDeque<PooledConnection>>,
    /// Optional AUTH password (sent on new connections).
    password: Option<String>,
    /// Optional database number (SELECT on new connections).
    db: Option<u16>,
}

impl RedisExecutor {
    pub fn new(timeout: Duration) -> Self {
        Self {
            request_timeout: timeout,
            pool: RefCell::new(VecDeque::new()),
            password: None,
            db: None,
        }
    }

    pub fn with_auth(timeout: Duration, password: Option<String>, db: Option<u16>) -> Self {
        Self {
            request_timeout: timeout,
            pool: RefCell::new(VecDeque::new()),
            password,
            db,
        }
    }

    /// Acquire a connection from the pool or open a new one.
    async fn acquire_connection(
        &self,
        target: std::net::SocketAddr,
    ) -> Result<(PooledConnection, Duration), ExecutionError> {
        // Try pool first (brief borrow, released before any await)
        let from_pool = self.pool.borrow_mut().pop_front();
        if let Some(conn) = from_pool {
            return Ok((conn, Duration::ZERO));
        }

        // Open new connection
        let connect_start = Instant::now();
        let stream = compio::net::TcpStream::connect(target)
            .await
            .map_err(|e| ExecutionError::Connect(e.to_string()))?;
        let connect_time = connect_start.elapsed();

        let mut conn = PooledConnection {
            stream,
            initialized: false,
            request_count: 0,
            created_at: Instant::now(),
        };

        // Send AUTH if configured
        if let Some(ref pw) = self.password {
            self.send_command(&mut conn, "AUTH", &[pw.clone()]).await?;
        }

        // Send SELECT if configured
        if let Some(db) = self.db {
            self.send_command(&mut conn, "SELECT", &[db.to_string()])
                .await?;
        }

        conn.initialized = true;
        Ok((conn, connect_time))
    }

    /// Send a command and read the response, returning an error if the
    /// server returns a RESP error.
    async fn send_command(
        &self,
        conn: &mut PooledConnection,
        command: &str,
        args: &[String],
    ) -> Result<(), ExecutionError> {
        let cmd_bytes = resp::encode_command(command, args);
        let BufResult(result, _) = conn.stream.write_all(cmd_bytes).await;
        result.map_err(|e| ExecutionError::Protocol(format!("{command} write: {e}")))?;

        let (val, _) = resp::read_response(&mut conn.stream, 65536)
            .await
            .map_err(|e| ExecutionError::Protocol(format!("{command} read: {e}")))?;

        if let resp::RespValue::Error(msg) = val {
            return Err(ExecutionError::Protocol(format!("{command}: {msg}")));
        }

        Ok(())
    }
}

impl RequestExecutor for RedisExecutor {
    type Spec = RedisRequestSpec;
    type PacketSource = netanvil_types::NoopPacketSource;

    async fn execute(&self, spec: &RedisRequestSpec, ctx: &RequestContext) -> ExecutionResult {
        let start = Instant::now();

        let result = compio::time::timeout(self.request_timeout, async {
            self.do_execute(spec, start).await
        })
        .await;

        match result {
            Ok(Ok((response_size, timing, status, error))) => ExecutionResult {
                request_id: ctx.request_id,
                intended_time: ctx.intended_time,
                actual_time: ctx.actual_time,
                timing,
                status,
                bytes_sent: resp::encode_command(&spec.command, &spec.args).len() as u64,
                response_size,
                error,
                response_headers: None,
                response_body: None,
            },
            Ok(Err(err)) => ExecutionResult {
                request_id: ctx.request_id,
                intended_time: ctx.intended_time,
                actual_time: ctx.actual_time,
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
                request_id: ctx.request_id,
                intended_time: ctx.intended_time,
                actual_time: ctx.actual_time,
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

impl RedisExecutor {
    async fn do_execute(
        &self,
        spec: &RedisRequestSpec,
        start: Instant,
    ) -> Result<(u64, TimingBreakdown, Option<u16>, Option<ExecutionError>), ExecutionError> {
        // 1. Acquire connection
        let (mut conn, tcp_connect) = self.acquire_connection(spec.target).await?;

        // 2. Encode and send command
        let cmd_bytes = resp::encode_command(&spec.command, &spec.args);
        let BufResult(result, _) = conn.stream.write_all(cmd_bytes).await;
        if let Err(e) = result {
            // Connection broken, don't return to pool
            return Err(ExecutionError::Protocol(format!("write: {e}")));
        }

        // 3. Read response
        let read_start = Instant::now();
        let resp_result = resp::read_response(&mut conn.stream, 1024 * 1024).await;
        let ttfb = read_start.elapsed();

        let (resp_value, response_bytes) = match resp_result {
            Ok((val, bytes)) => (val, bytes as u64),
            Err(e) => {
                // Connection broken, don't return to pool
                return Err(ExecutionError::Protocol(format!("read: {e}")));
            }
        };

        conn.request_count += 1;

        let total = start.elapsed();

        // 4. Map RESP response to status/error
        //    0 = success (SimpleString, Integer, BulkString, Array)
        //    1 = nil (null bulk string -- key not found)
        //    2 = server error (-ERR ...)
        let (status, error) = match &resp_value {
            resp::RespValue::SimpleString(_)
            | resp::RespValue::Integer(_)
            | resp::RespValue::Array(_) => (Some(0u16), None),
            resp::RespValue::BulkString(Some(_)) => (Some(0u16), None),
            resp::RespValue::BulkString(None) => (Some(1u16), None),
            resp::RespValue::Error(msg) => (Some(2u16), Some(ExecutionError::Other(msg.clone()))),
        };

        // 5. Return connection to pool
        self.pool.borrow_mut().push_back(conn);

        Ok((
            response_bytes,
            TimingBreakdown {
                dns_lookup: Duration::ZERO,
                tcp_connect,
                tls_handshake: Duration::ZERO,
                time_to_first_byte: ttfb,
                content_transfer: total.saturating_sub(tcp_connect).saturating_sub(ttfb),
                total,
            },
            status,
            error,
        ))
    }
}
