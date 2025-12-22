//! TCP request executor implementing [`RequestExecutor`].
//!
//! The executor manages a per-core connection pool and dispatches I/O based
//! on the [`TcpTestMode`] in each request spec.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use compio::buf::BufResult;
use compio::io::{AsyncReadExt, AsyncWriteExt};
use netanvil_types::{
    ConnectionPolicy, ExecutionError, ExecutionResult, RequestContext, RequestExecutor,
    TimingBreakdown,
};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use crate::framing;
use crate::pool::{ConnectionPool, PooledConnection};
use crate::protocol;
use crate::spec::{TcpRequestSpec, TcpTestMode};

/// TCP executor with connection pooling and multi-mode support.
///
/// Each I/O worker core owns one `TcpExecutor` instance. The internal
/// [`ConnectionPool`] is wrapped in `RefCell` because the executor is
/// `Rc`-shared across concurrent spawned tasks on a single core.
/// All `RefCell` borrows are brief and never held across await points.
pub struct TcpExecutor {
    request_timeout: Duration,
    pool: RefCell<ConnectionPool>,
    policy: ConnectionPolicy,
    /// Lifecycle counter for Mixed policy with connection_lifetime.
    /// Properly samples from the distribution (replaces the old broken
    /// `should_keep_connection` that used max/mean as deterministic limits).
    lifecycle: RefCell<Option<netanvil_core_lifecycle::LifecycleCounter>>,
    rng: RefCell<SmallRng>,
}

// LifecycleCounter lives in netanvil-core, but netanvil-tcp can't depend on
// netanvil-core (circular). We replicate the minimal lifecycle logic here.
mod netanvil_core_lifecycle {
    use netanvil_types::distribution::ValueDistribution;
    use rand::rngs::SmallRng;
    use rand::{Rng, SeedableRng};
    use rand_distr::{Distribution, Normal};

    /// Counts events and signals when a distribution-sampled limit is reached.
    pub struct LifecycleCounter {
        distribution: ValueDistribution<u32>,
        count: u32,
        limit: u32,
        rng: SmallRng,
    }

    impl LifecycleCounter {
        pub fn new(distribution: ValueDistribution<u32>) -> Self {
            let mut rng = SmallRng::from_entropy();
            let limit = sample(&distribution, &mut rng);
            Self {
                distribution,
                count: 0,
                limit,
                rng,
            }
        }

        #[inline]
        pub fn tick(&mut self) -> bool {
            self.count += 1;
            self.count > self.limit
        }

        pub fn reset(&mut self) {
            self.count = 0;
            self.limit = sample(&self.distribution, &mut self.rng);
        }
    }

    fn sample(dist: &ValueDistribution<u32>, rng: &mut SmallRng) -> u32 {
        match dist {
            ValueDistribution::Fixed(n) => *n,
            ValueDistribution::Uniform { min, max } => rng.gen_range(*min..=*max),
            ValueDistribution::Normal { mean, stddev } => {
                let normal = Normal::new(*mean, *stddev)
                    .unwrap_or_else(|_| Normal::new(1.0, 0.0).unwrap());
                let s: f64 = normal.sample(rng);
                s.round().max(1.0) as u32
            }
            ValueDistribution::Weighted(entries) => {
                let total: f64 = entries.iter().map(|e| e.weight).sum();
                let roll: f64 = rng.gen_range(0.0..total);
                let mut cumulative = 0.0;
                for entry in entries {
                    cumulative += entry.weight;
                    if roll < cumulative {
                        return entry.value;
                    }
                }
                entries.last().unwrap().value
            }
        }
    }
}

impl Default for TcpExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl TcpExecutor {
    pub fn new() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            pool: RefCell::new(ConnectionPool::new(0)), // unlimited
            policy: ConnectionPolicy::KeepAlive,
            lifecycle: RefCell::new(None),
            rng: RefCell::new(SmallRng::from_entropy()),
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            request_timeout: timeout,
            pool: RefCell::new(ConnectionPool::new(0)),
            policy: ConnectionPolicy::KeepAlive,
            lifecycle: RefCell::new(None),
            rng: RefCell::new(SmallRng::from_entropy()),
        }
    }

    pub fn with_pool(timeout: Duration, max_connections: usize, policy: ConnectionPolicy) -> Self {
        let lifecycle = match &policy {
            ConnectionPolicy::Mixed {
                connection_lifetime: Some(dist),
                ..
            } => Some(netanvil_core_lifecycle::LifecycleCounter::new(dist.clone())),
            _ => None,
        };

        Self {
            request_timeout: timeout,
            pool: RefCell::new(ConnectionPool::new(max_connections)),
            policy,
            lifecycle: RefCell::new(lifecycle),
            rng: RefCell::new(SmallRng::from_entropy()),
        }
    }
}

impl RequestExecutor for TcpExecutor {
    type Spec = TcpRequestSpec;
    type PacketSource = netanvil_types::NoopPacketSource;

    async fn execute(&self, spec: &TcpRequestSpec, context: &RequestContext) -> ExecutionResult {
        let start = Instant::now();

        let result = compio::time::timeout(self.request_timeout, async {
            self.do_execute(spec, start).await
        })
        .await;

        let bytes_sent = match spec.mode {
            TcpTestMode::Echo => spec.payload.len() as u64,
            TcpTestMode::RR => spec.request_size as u64,
            TcpTestMode::Sink | TcpTestMode::Bidir => spec.payload.len() as u64,
            TcpTestMode::Source => 0,
        };

        match result {
            Ok(Ok((response_size, timing))) => ExecutionResult {
                request_id: context.request_id,
                intended_time: context.intended_time,
                actual_time: context.actual_time,
                timing,
                status: None,
                bytes_sent,
                response_size,
                error: None,
                response_headers: None,
                response_body: None,
            },
            Ok(Err(err)) => ExecutionResult {
                request_id: context.request_id,
                intended_time: context.intended_time,
                actual_time: context.actual_time,
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

impl TcpExecutor {
    async fn do_execute(
        &self,
        spec: &TcpRequestSpec,
        start: Instant,
    ) -> Result<(u64, TimingBreakdown), ExecutionError> {
        // 1. Acquire connection from pool or open new
        let (mut conn, tcp_connect) = self.acquire_connection(spec.target, start).await?;

        // 2. Send protocol header on fresh connections (non-Echo modes)
        if !conn.header_sent && spec.mode != TcpTestMode::Echo {
            let header = protocol::encode_header(spec);
            let BufResult(result, _) = conn.stream.write_all(header).await;
            result.map_err(|e| {
                self.pool.borrow_mut().record_dropped();
                ExecutionError::Protocol(format!("header write: {e}"))
            })?;
            conn.header_sent = true;
        }

        // 3. Mode-specific I/O
        let io_result = match spec.mode {
            TcpTestMode::Echo => self.do_echo(&mut conn, spec).await,
            TcpTestMode::RR => self.do_rr(&mut conn, spec).await,
            TcpTestMode::Sink => self.do_sink(&mut conn, spec).await,
            TcpTestMode::Source => self.do_source(&mut conn, spec).await,
            TcpTestMode::Bidir => self.do_bidir(&mut conn, spec).await,
        };

        let (response_size, ttfb) = match io_result {
            Ok(val) => val,
            Err(e) => {
                self.pool.borrow_mut().record_dropped();
                return Err(e);
            }
        };

        conn.request_count += 1;

        let total = start.elapsed();
        let content_transfer = total.saturating_sub(tcp_connect).saturating_sub(ttfb);

        // 4. Return to pool or drop based on policy
        if self.should_keep_connection() {
            self.pool.borrow_mut().return_idle(conn);
        } else {
            self.pool.borrow_mut().record_dropped();
        }

        Ok((
            response_size,
            TimingBreakdown {
                dns_lookup: Duration::ZERO,
                tcp_connect,
                tls_handshake: Duration::ZERO,
                time_to_first_byte: ttfb,
                content_transfer,
                total,
            },
        ))
    }

    async fn acquire_connection(
        &self,
        target: std::net::SocketAddr,
        start: Instant,
    ) -> Result<(PooledConnection, Duration), ExecutionError> {
        // Try pool first (brief borrow, released before any await)
        let from_pool = self.pool.borrow_mut().take_idle();
        if let Some(conn) = from_pool {
            return Ok((conn, Duration::ZERO)); // reused, no connect time
        }

        // Check if we can open a new one (brief borrow)
        if !self.pool.borrow().can_open_new() {
            return Err(ExecutionError::Connect("connection pool exhausted".into()));
        }

        // Record that we are opening a connection (brief borrow)
        self.pool.borrow_mut().record_opened();

        // Open new connection (async, no borrow held)
        let stream = compio::net::TcpStream::connect(target).await.map_err(|e| {
            self.pool.borrow_mut().record_dropped();
            ExecutionError::Connect(e.to_string())
        })?;
        let tcp_connect = start.elapsed();

        Ok((
            PooledConnection {
                stream,
                header_sent: false,
                request_count: 0,
                created_at: Instant::now(),
                reader_spawned: false,
            },
            tcp_connect,
        ))
    }

    async fn do_echo(
        &self,
        conn: &mut PooledConnection,
        spec: &TcpRequestSpec,
    ) -> Result<(u64, Duration), ExecutionError> {
        // Same as original: encode with framing, write, optionally read
        let encoded = framing::encode_frame(&spec.payload, &spec.framing);
        let BufResult(result, _) = conn.stream.write_all(encoded).await;
        result.map_err(|e| ExecutionError::Protocol(format!("write: {e}")))?;

        if spec.expect_response {
            let read_start = Instant::now();
            let data =
                framing::read_framed(&mut conn.stream, &spec.framing, spec.response_max_bytes)
                    .await
                    .map_err(|e| ExecutionError::Protocol(format!("read: {e}")))?;
            let ttfb = read_start.elapsed();
            Ok((data.len() as u64, ttfb))
        } else {
            Ok((0, Duration::ZERO))
        }
    }

    async fn do_rr(
        &self,
        conn: &mut PooledConnection,
        spec: &TcpRequestSpec,
    ) -> Result<(u64, Duration), ExecutionError> {
        // Write exactly request_size bytes (pad or truncate payload)
        let payload = if spec.payload.len() >= spec.request_size as usize {
            spec.payload[..spec.request_size as usize].to_vec()
        } else {
            let mut buf = spec.payload.clone();
            buf.resize(spec.request_size as usize, 0);
            buf
        };

        if !payload.is_empty() {
            let BufResult(result, _) = conn.stream.write_all(payload).await;
            result.map_err(|e| ExecutionError::Protocol(format!("write: {e}")))?;
        }

        // Read exactly response_size bytes
        if spec.response_size > 0 {
            let read_start = Instant::now();
            let buf = vec![0u8; spec.response_size as usize];
            let BufResult(result, _) = conn.stream.read_exact(buf).await;
            result.map_err(|e| ExecutionError::Protocol(format!("read: {e}")))?;
            let ttfb = read_start.elapsed();
            Ok((spec.response_size as u64, ttfb))
        } else {
            Ok((0, Duration::ZERO))
        }
    }

    async fn do_sink(
        &self,
        conn: &mut PooledConnection,
        spec: &TcpRequestSpec,
    ) -> Result<(u64, Duration), ExecutionError> {
        // Write a chunk, no response expected
        let BufResult(result, _) = conn.stream.write_all(spec.payload.clone()).await;
        result.map_err(|e| ExecutionError::Protocol(format!("write: {e}")))?;

        Ok((0, Duration::ZERO))
    }

    async fn do_source(
        &self,
        conn: &mut PooledConnection,
        spec: &TcpRequestSpec,
    ) -> Result<(u64, Duration), ExecutionError> {
        // Read a chunk from the server
        let read_start = Instant::now();
        let buf = vec![0u8; spec.response_size.max(1) as usize];
        let BufResult(result, _) = conn.stream.read_exact(buf).await;
        result.map_err(|e| ExecutionError::Protocol(format!("read: {e}")))?;
        let ttfb = read_start.elapsed();

        Ok((spec.response_size as u64, ttfb))
    }

    async fn do_bidir(
        &self,
        conn: &mut PooledConnection,
        spec: &TcpRequestSpec,
    ) -> Result<(u64, Duration), ExecutionError> {
        // Spawn a background reader on first use of this connection for BIDIR.
        // compio's TcpStream derives Clone, which shares the same underlying fd.
        // One handle is used for writes (driven by fire events), the other for
        // reads (background task). Both are !Send and stay on the same core.
        if !conn.reader_spawned {
            let mut reader_stream = conn.stream.clone();
            compio::runtime::spawn(async move {
                use compio::io::AsyncRead;
                let mut buf = vec![0u8; 65536];
                loop {
                    let BufResult(result, b) = reader_stream.read(buf).await;
                    buf = b;
                    match result {
                        Ok(0) => break,    // EOF
                        Ok(_) => continue, // discard read data (throughput measured by write side)
                        Err(_) => break,
                    }
                }
            })
            .detach();
            conn.reader_spawned = true;
        }

        // Each fire event writes a chunk (reads happen in the background task)
        let BufResult(result, _) = conn.stream.write_all(spec.payload.clone()).await;
        result.map_err(|e| ExecutionError::Protocol(format!("bidir write: {e}")))?;

        Ok((0, Duration::ZERO))
    }

    /// Determine whether the current connection should be kept based on policy.
    ///
    /// For `Mixed` policy: uses a [`LifecycleCounter`] that properly samples
    /// from the distribution, and respects `persistent_ratio`.
    fn should_keep_connection(&self) -> bool {
        match &self.policy {
            ConnectionPolicy::KeepAlive => true,
            ConnectionPolicy::AlwaysNew => false,
            ConnectionPolicy::NoReuse => false,
            ConnectionPolicy::Mixed {
                persistent_ratio, ..
            } => {
                let mut lifecycle = self.lifecycle.borrow_mut();
                if let Some(ref mut lc) = *lifecycle {
                    if lc.tick() {
                        lc.reset();
                        return false; // lifecycle limit reached — close
                    }
                }
                // Random keep/close decision based on persistent_ratio
                self.rng.borrow_mut().gen_bool(*persistent_ratio)
            }
        }
    }
}
