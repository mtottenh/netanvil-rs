//! TCP stream-level read throttling for modem speed simulation.
//!
//! Provides [`ThrottledStream`] which wraps any compio `AsyncRead + AsyncWrite`
//! stream and rate-limits reads using a token bucket algorithm. Writes pass
//! through unthrottled.
//!
//! Combined with OS-level socket tuning (`SO_RCVBUF`, `TCP_WINDOW_CLAMP`), this
//! creates real TCP backpressure — the server sees the client's receive window
//! shrinking, causing its sends to slow down.

use std::io;
use std::time::{Duration, Instant};

use compio::buf::{IoBuf, IoBufMut, IoVectoredBuf};
use compio::io::{AsyncRead, AsyncWrite};
use compio::BufResult;

// ---------------------------------------------------------------------------
// Token bucket
// ---------------------------------------------------------------------------

/// Simple token bucket for rate limiting byte reads.
///
/// Tokens represent bytes. Tokens accumulate at `rate_bytes_per_sec` up to
/// `capacity`. Reading N bytes consumes N tokens. When tokens go negative,
/// the caller sleeps to pay back the debt.
struct TokenBucket {
    /// Current token count (can go negative after a burst read).
    tokens: f64,
    /// Maximum burst size in bytes.
    capacity: f64,
    /// Refill rate.
    rate_bytes_per_sec: f64,
    /// Last time tokens were refilled.
    last_refill: Instant,
}

impl TokenBucket {
    fn new(rate_bps: u64) -> Self {
        let rate_bytes_per_sec = rate_bps as f64 / 8.0;
        // Burst capacity: 50ms worth of data or 1 MTU, whichever is larger.
        let capacity = (rate_bytes_per_sec * 0.05).max(1500.0);
        Self {
            tokens: capacity, // start full — first read is instant
            capacity,
            rate_bytes_per_sec,
            last_refill: Instant::now(),
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate_bytes_per_sec).min(self.capacity);
        self.last_refill = now;
    }

    /// Consume `n` tokens. Returns the sleep duration needed to pay back any
    /// debt (zero if tokens were available).
    fn consume(&mut self, n: usize) -> Duration {
        self.refill();
        self.tokens -= n as f64;
        if self.tokens < 0.0 {
            let debt = -self.tokens;
            Duration::from_secs_f64(debt / self.rate_bytes_per_sec)
        } else {
            Duration::ZERO
        }
    }

    /// How long to wait before at least 1 token is available.
    fn wait_time(&mut self) -> Duration {
        self.refill();
        if self.tokens >= 1.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / self.rate_bytes_per_sec)
        }
    }
}

// ---------------------------------------------------------------------------
// ThrottledStream
// ---------------------------------------------------------------------------

/// Rate-limited stream wrapper. Delays reads to enforce a target bitrate.
///
/// Writes pass through unthrottled (we only simulate slow *clients*, not
/// slow uploads).
pub struct ThrottledStream<S> {
    inner: S,
    bucket: TokenBucket,
}

impl<S> ThrottledStream<S> {
    /// Create a new throttled stream.
    ///
    /// `rate_bps` is the target read rate in **bits** per second.
    pub fn new(inner: S, rate_bps: u64) -> Self {
        Self {
            inner,
            bucket: TokenBucket::new(rate_bps),
        }
    }

    /// Get a reference to the inner stream.
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S: AsyncRead> AsyncRead for ThrottledStream<S> {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        // Wait for tokens if we're in debt from a previous read
        let wait = self.bucket.wait_time();
        if wait > Duration::ZERO {
            compio::time::sleep(wait).await;
        }

        // Read from inner stream at full speed
        let BufResult(result, buf) = self.inner.read(buf).await;

        // Consume tokens and sleep to pay back debt
        if let Ok(n) = &result {
            if *n > 0 {
                let debt_sleep = self.bucket.consume(*n);
                if debt_sleep > Duration::ZERO {
                    compio::time::sleep(debt_sleep).await;
                }
            }
        }

        BufResult(result, buf)
    }
}

impl<S: AsyncWrite> AsyncWrite for ThrottledStream<S> {
    async fn write<T: IoBuf>(&mut self, buf: T) -> BufResult<usize, T> {
        self.inner.write(buf).await
    }

    async fn write_vectored<T: IoVectoredBuf>(&mut self, buf: T) -> BufResult<usize, T> {
        self.inner.write_vectored(buf).await
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().await
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        self.inner.shutdown().await
    }
}

// ThrottledStream is Unpin if S is Unpin (needed for MaybeTlsStream bounds)
impl<S: Unpin> Unpin for ThrottledStream<S> {}

// ---------------------------------------------------------------------------
// MaybeThrottled — zero-overhead enum dispatch
// ---------------------------------------------------------------------------

/// Enum wrapper providing zero overhead when throttling is disabled.
///
/// The `Direct` variant passes through to the inner stream with no
/// additional logic — just an enum discriminant check per I/O operation.
pub enum MaybeThrottled<S> {
    /// No throttling — direct passthrough.
    Direct(S),
    /// Throttled via token bucket.
    Throttled(ThrottledStream<S>),
}

impl<S: AsyncRead> AsyncRead for MaybeThrottled<S> {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        match self {
            Self::Direct(s) => s.read(buf).await,
            Self::Throttled(s) => s.read(buf).await,
        }
    }
}

impl<S: AsyncWrite> AsyncWrite for MaybeThrottled<S> {
    async fn write<T: IoBuf>(&mut self, buf: T) -> BufResult<usize, T> {
        match self {
            Self::Direct(s) => s.write(buf).await,
            Self::Throttled(s) => s.write(buf).await,
        }
    }

    async fn write_vectored<T: IoVectoredBuf>(&mut self, buf: T) -> BufResult<usize, T> {
        match self {
            Self::Direct(s) => s.write_vectored(buf).await,
            Self::Throttled(s) => s.write_vectored(buf).await,
        }
    }

    async fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Direct(s) => s.flush().await,
            Self::Throttled(s) => s.flush().await,
        }
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        match self {
            Self::Direct(s) => s.shutdown().await,
            Self::Throttled(s) => s.shutdown().await,
        }
    }
}

impl<S: Unpin> Unpin for MaybeThrottled<S> {}

// ---------------------------------------------------------------------------
// OS-level socket tuning (per-socket, NOT global sysctls)
// ---------------------------------------------------------------------------

/// Set per-socket TCP receive buffer size via `setsockopt(SO_RCVBUF)`.
///
/// This is a **per-socket** operation — it does NOT affect other sockets or
/// global kernel settings. The kernel doubles the value internally for
/// bookkeeping overhead, so requesting 4096 yields ~8 KiB effective buffer.
///
/// On Linux, the minimum is `net.ipv4.tcp_rmem[0]` (typically 4096).
#[cfg(target_os = "linux")]
pub fn set_rcvbuf(fd: std::os::unix::io::RawFd, size: usize) -> io::Result<()> {
    let val = size as libc::c_int;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &val as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Set per-socket TCP window clamp via `setsockopt(TCP_WINDOW_CLAMP)`.
///
/// This is a **per-socket** operation. It sets the maximum advertised
/// receive window for this specific TCP connection. The server will not
/// send more than this many bytes without receiving an ACK.
///
/// Linux-specific: `TCP_WINDOW_CLAMP` (value 10) is a Linux extension.
#[cfg(target_os = "linux")]
pub fn set_window_clamp(fd: std::os::unix::io::RawFd, size: usize) -> io::Result<()> {
    const TCP_WINDOW_CLAMP: libc::c_int = 10;
    let val = size as libc::c_int;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            TCP_WINDOW_CLAMP,
            &val as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Compute recommended socket option values from target bitrate.
///
/// Returns `(so_rcvbuf, tcp_window_clamp)`.
pub fn compute_socket_opts(rate_bps: u64) -> (usize, usize) {
    let rate_bytes_per_sec = rate_bps as f64 / 8.0;

    // SO_RCVBUF: 100ms worth of data, clamped to [4096, 65536].
    // The kernel doubles this internally for bookkeeping.
    let rcvbuf = (rate_bytes_per_sec * 0.1).clamp(4096.0, 65536.0) as usize;

    // TCP_WINDOW_CLAMP: 50ms worth of data, clamped to [1024, 65536].
    let clamp = (rate_bytes_per_sec * 0.05).clamp(1024.0, 65536.0) as usize;

    (rcvbuf, clamp)
}
