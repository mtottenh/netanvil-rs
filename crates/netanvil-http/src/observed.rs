//! TCP stream wrapper with sampled io_uring linked getsockopt observations.

use std::cell::RefCell;
use std::rc::Rc;

use compio::buf::{BufResult, IoBufMut, IoVectoredBufMut};
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use netanvil_types::{CpuAffinityCounters, TcpHealthCounters};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

/// Configuration for creating `ObservedTcpStream` instances on a single core.
///
/// Held by the connector, cloned per-connection. The `CpuAffinityCounters`
/// are shared across all connections on the same core and with the
/// metrics collector.
///
/// Wrapped in `SendWrapper` at the connector level because hyper requires
/// `Send + Sync` on connectors, but we're in a thread-per-core model where
/// the connector never actually crosses threads.
#[derive(Clone)]
pub struct ObserveConfig {
    pub sample_rate: f64,
    pub worker_cpu: usize,
    pub counters: Rc<CpuAffinityCounters>,
    pub tcp_health: Rc<TcpHealthCounters>,
}

/// Thread-safe wrapper around `ObserveConfig` for use in the connector.
///
/// The inner `Rc<CpuAffinityCounters>` is `!Send`, but the connector
/// is always used within the compio thread-per-core model where it never
/// crosses threads. `SendWrapper` enforces this at runtime.
#[derive(Clone)]
pub struct SendObserveConfig(send_wrapper::SendWrapper<ObserveConfig>);

// SAFETY: SendObserveConfig is only accessed from the creating thread
// (enforced by SendWrapper's runtime check).
unsafe impl Sync for SendObserveConfig {}

impl SendObserveConfig {
    pub fn new(config: ObserveConfig) -> Self {
        Self(send_wrapper::SendWrapper::new(config))
    }

    pub fn get(&self) -> &ObserveConfig {
        &self.0
    }
}

/// TCP stream wrapper that samples getsockopt observations using
/// io_uring linked SQEs.
///
/// Wraps a compio `TcpStream`. On sampled reads, submits a linked
/// `Recv → GetSockOpt(SO_INCOMING_CPU)` pair. On non-sampled reads,
/// delegates directly to `TcpStream` (zero overhead).
pub struct ObservedTcpStream {
    inner: TcpStream,
    sample_rate: f64,
    rng: RefCell<SmallRng>,
    worker_cpu: usize,
    counters: Rc<CpuAffinityCounters>,
    tcp_health: Rc<TcpHealthCounters>,
}

impl ObservedTcpStream {
    pub fn new(inner: TcpStream, config: &ObserveConfig) -> Self {
        use std::os::unix::io::AsRawFd;
        let seed = inner.as_raw_fd() as u64 ^ 0xDEADBEEF_CAFEBABE;
        Self {
            inner,
            sample_rate: config.sample_rate,
            rng: RefCell::new(SmallRng::seed_from_u64(seed)),
            worker_cpu: config.worker_cpu,
            counters: config.counters.clone(),
            tcp_health: config.tcp_health.clone(),
        }
    }

    fn should_sample(&self) -> bool {
        self.sample_rate > 0.0 && self.rng.borrow_mut().gen::<f64>() < self.sample_rate
    }
}

impl AsyncRead for ObservedTcpStream {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        if !self.should_sample() {
            return (&self.inner).read(buf).await;
        }

        // Linked 3-SQE chain: Recv → GetSockOpt(SO_INCOMING_CPU) → GetSockOpt(TCP_INFO).
        // SAFETY: SO_INCOMING_CPU returns i32, TCP_INFO returns libc::tcp_info.
        let result = unsafe {
            self.inner
                .recv_observe2::<B, i32, libc::tcp_info>(
                    buf,
                    0,
                    libc::SOL_SOCKET,
                    libc::SO_INCOMING_CPU,
                    libc::IPPROTO_TCP as i32,
                    libc::TCP_INFO,
                )
                .await
        };
        match result {
            Ok((recv_result, incoming_cpu, tcp_info)) => {
                self.counters.record(incoming_cpu, self.worker_cpu);
                self.tcp_health.record(&tcp_info);
                recv_result
            }
            Err(_e) => {
                // Linked submission failed — kernel doesn't support URING_CMD.
                // This shouldn't happen because we check at startup, but
                // disable sampling and panic with a clear message.
                self.sample_rate = 0.0;
                panic!(
                    "linked recv_observe failed: {_e}. \
                     --health-sample-rate requires kernel >= 6.7 with io_uring URING_CMD support"
                );
            }
        }
    }

    async fn read_vectored<V: IoVectoredBufMut>(&mut self, buf: V) -> BufResult<usize, V> {
        (&self.inner).read_vectored(buf).await
    }
}

impl AsyncWrite for ObservedTcpStream {
    async fn write<B: compio::buf::IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        (&self.inner).write(buf).await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        (&self.inner).flush().await
    }

    async fn shutdown(&mut self) -> std::io::Result<()> {
        (&self.inner).shutdown().await
    }
}

impl std::os::unix::io::AsRawFd for ObservedTcpStream {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.inner.as_raw_fd()
    }
}
