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
    /// Total bytes received from clients.
    pub bytes_received: AtomicU64,
    /// Total bytes sent to clients.
    pub bytes_sent: AtomicU64,
    /// Total TCP connections accepted.
    pub connections_accepted: AtomicU64,
    /// Total TCP connections that have closed.
    pub connections_closed: AtomicU64,
    /// Total request-response cycles completed (TCP RR/BIDIR mode).
    pub requests_completed: AtomicU64,
    /// Total I/O errors encountered.
    pub errors: AtomicU64,
    /// Total UDP/DNS datagrams received.
    pub datagrams_received: AtomicU64,
    /// Total UDP/DNS datagrams sent.
    pub datagrams_sent: AtomicU64,
    /// Total UDP/DNS datagrams dropped (send failures).
    pub datagrams_dropped: AtomicU64,
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
        }
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
}

impl ServerMetricsSummary {
    /// Aggregate multiple worker snapshots into a summary.
    pub fn from_snapshots(snapshots: &[WorkerSnapshot]) -> Self {
        let mut summary = Self::default();
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
        }
        summary.connections_active = summary
            .connections_accepted
            .saturating_sub(summary.connections_closed);
        summary
    }
}
