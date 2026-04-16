//! Server-side metrics collection.
//!
//! Per-worker counters using `AtomicU64` with `Relaxed` ordering — same cost
//! as `Cell<u64>` on x86 (no memory barrier for relaxed loads/stores), but
//! `Sync` so the control thread can read them for aggregation.

use std::sync::atomic::{AtomicU64, Ordering};

/// Per-worker metrics counters.
///
/// Each worker thread gets its own `WorkerMetrics` wrapped in `Arc`, shared
/// between the worker (writes) and the `TestServer` handle (reads via
/// `metrics_snapshot()`). All counters use `Relaxed` ordering — no
/// cross-counter consistency guarantees, but individual counters are
/// monotonically non-decreasing.
pub struct WorkerMetrics {
    // -- Throughput --
    /// Total bytes received from clients.
    pub bytes_received: AtomicU64,
    /// Total bytes sent to clients.
    pub bytes_sent: AtomicU64,

    // -- TCP connections --
    /// Total TCP connections accepted.
    pub connections_accepted: AtomicU64,
    /// Total TCP connections that have closed.
    pub connections_closed: AtomicU64,

    // -- Requests --
    /// Total request-response cycles completed (TCP RR/BIDIR mode).
    pub requests_completed: AtomicU64,
    /// Total I/O errors encountered.
    pub errors: AtomicU64,

    // -- UDP/DNS --
    /// Total UDP/DNS datagrams received.
    pub datagrams_received: AtomicU64,
    /// Total UDP/DNS datagrams sent.
    pub datagrams_sent: AtomicU64,
    /// Total UDP/DNS datagrams dropped (send failures).
    pub datagrams_dropped: AtomicU64,

    // -- TCP health (from getsockopt TCP_INFO on connection close) --
    /// Sum of smoothed RTT observations (microseconds).
    pub tcp_rtt_sum_us: AtomicU64,
    /// Number of TCP_INFO RTT observations.
    pub tcp_rtt_count: AtomicU64,
    /// Maximum smoothed RTT observed (microseconds).
    pub tcp_rtt_max_us: AtomicU64,
    /// Total TCP retransmits across all sampled connections.
    pub tcp_retransmits: AtomicU64,
    /// Total TCP lost segments across all sampled connections.
    pub tcp_lost: AtomicU64,
}

impl Default for WorkerMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerMetrics {
    pub fn new() -> Self {
        Self {
            bytes_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            connections_accepted: AtomicU64::new(0),
            connections_closed: AtomicU64::new(0),
            requests_completed: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            datagrams_received: AtomicU64::new(0),
            datagrams_sent: AtomicU64::new(0),
            datagrams_dropped: AtomicU64::new(0),
            tcp_rtt_sum_us: AtomicU64::new(0),
            tcp_rtt_count: AtomicU64::new(0),
            tcp_rtt_max_us: AtomicU64::new(0),
            tcp_retransmits: AtomicU64::new(0),
            tcp_lost: AtomicU64::new(0),
        }
    }

    /// Read all counters into a snapshot.
    pub fn snapshot(&self) -> WorkerSnapshot {
        WorkerSnapshot {
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            connections_accepted: self.connections_accepted.load(Ordering::Relaxed),
            connections_closed: self.connections_closed.load(Ordering::Relaxed),
            requests_completed: self.requests_completed.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            datagrams_received: self.datagrams_received.load(Ordering::Relaxed),
            datagrams_sent: self.datagrams_sent.load(Ordering::Relaxed),
            datagrams_dropped: self.datagrams_dropped.load(Ordering::Relaxed),
            tcp_rtt_sum_us: self.tcp_rtt_sum_us.load(Ordering::Relaxed),
            tcp_rtt_count: self.tcp_rtt_count.load(Ordering::Relaxed),
            tcp_rtt_max_us: self.tcp_rtt_max_us.load(Ordering::Relaxed),
            tcp_retransmits: self.tcp_retransmits.load(Ordering::Relaxed),
            tcp_lost: self.tcp_lost.load(Ordering::Relaxed),
        }
    }

    /// Record a `getsockopt(TCP_INFO)` observation.
    ///
    /// Mirrors the field extraction from `netanvil_types::TcpHealthCounters::record()`.
    #[inline]
    pub fn record_tcp_info(&self, info: &libc::tcp_info) {
        let rtt = info.tcpi_rtt as u64;
        self.tcp_rtt_sum_us.fetch_add(rtt, Ordering::Relaxed);
        self.tcp_rtt_count.fetch_add(1, Ordering::Relaxed);
        self.tcp_rtt_max_us.fetch_max(rtt, Ordering::Relaxed);
        self.tcp_retransmits
            .fetch_add(info.tcpi_total_retrans as u64, Ordering::Relaxed);
        self.tcp_lost
            .fetch_add(info.tcpi_lost as u64, Ordering::Relaxed);
    }

    #[inline]
    pub fn add_bytes_received(&self, n: u64) {
        self.bytes_received.fetch_add(n, Ordering::Relaxed);
    }

    #[inline]
    pub fn add_bytes_sent(&self, n: u64) {
        self.bytes_sent.fetch_add(n, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_connections_accepted(&self) {
        self.connections_accepted.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_connections_closed(&self) {
        self.connections_closed.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_requests_completed(&self) {
        self.requests_completed.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_errors(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_datagrams_received(&self) {
        self.datagrams_received.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_datagrams_sent(&self) {
        self.datagrams_sent.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_datagrams_dropped(&self) {
        self.datagrams_dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// Point-in-time snapshot of a single worker's counters.
#[derive(Debug, Clone, Default)]
pub struct WorkerSnapshot {
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub connections_accepted: u64,
    pub connections_closed: u64,
    pub requests_completed: u64,
    pub errors: u64,
    pub datagrams_received: u64,
    pub datagrams_sent: u64,
    pub datagrams_dropped: u64,
    pub tcp_rtt_sum_us: u64,
    pub tcp_rtt_count: u64,
    pub tcp_rtt_max_us: u64,
    pub tcp_retransmits: u64,
    pub tcp_lost: u64,
}

/// Aggregated metrics across all workers at a point in time.
#[derive(Debug, Clone, Default)]
pub struct ServerMetricsSummary {
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub connections_accepted: u64,
    /// Computed: `connections_accepted - connections_closed`.
    pub connections_active: u64,
    pub connections_closed: u64,
    pub requests_completed: u64,
    pub errors: u64,
    pub datagrams_received: u64,
    pub datagrams_sent: u64,
    pub datagrams_dropped: u64,
    // -- TCP health --
    /// Average smoothed RTT in microseconds (0 if no observations).
    pub tcp_rtt_avg_us: f64,
    /// Maximum smoothed RTT in microseconds.
    pub tcp_rtt_max_us: u64,
    /// Total TCP retransmits.
    pub tcp_retransmits: u64,
    /// Total TCP lost segments.
    pub tcp_lost: u64,
}

impl ServerMetricsSummary {
    /// Aggregate multiple worker snapshots into a summary.
    pub fn from_snapshots(snapshots: &[WorkerSnapshot]) -> Self {
        let mut summary = Self::default();
        let mut rtt_sum: u64 = 0;
        let mut rtt_count: u64 = 0;
        for snap in snapshots {
            summary.bytes_received += snap.bytes_received;
            summary.bytes_sent += snap.bytes_sent;
            summary.connections_accepted += snap.connections_accepted;
            summary.connections_closed += snap.connections_closed;
            summary.requests_completed += snap.requests_completed;
            summary.errors += snap.errors;
            summary.datagrams_received += snap.datagrams_received;
            summary.datagrams_sent += snap.datagrams_sent;
            summary.datagrams_dropped += snap.datagrams_dropped;
            rtt_sum += snap.tcp_rtt_sum_us;
            rtt_count += snap.tcp_rtt_count;
            if snap.tcp_rtt_max_us > summary.tcp_rtt_max_us {
                summary.tcp_rtt_max_us = snap.tcp_rtt_max_us;
            }
            summary.tcp_retransmits += snap.tcp_retransmits;
            summary.tcp_lost += snap.tcp_lost;
        }
        summary.connections_active = summary
            .connections_accepted
            .saturating_sub(summary.connections_closed);
        if rtt_count > 0 {
            summary.tcp_rtt_avg_us = rtt_sum as f64 / rtt_count as f64;
        }
        summary
    }
}
