//! Per-core TCP connection pool.
//!
//! `ConnectionPool` is `!Send` by design: each I/O worker core owns its own
//! pool instance, matching the thread-per-core architecture. The pool is
//! accessed via `RefCell` from the executor, with borrows never held across
//! await points.

use std::collections::VecDeque;
use std::time::Instant;

/// A single pooled TCP connection with bookkeeping metadata.
pub struct PooledConnection {
    /// The underlying TCP stream.
    pub stream: compio::net::TcpStream,
    /// Whether the protocol header has been sent on this connection.
    pub header_sent: bool,
    /// Number of operations completed on this connection.
    pub request_count: u32,
    /// When this connection was established.
    pub created_at: Instant,
    /// Whether a background reader task has been spawned for BIDIR mode.
    /// Once spawned, the reader task runs until the connection is closed (EOF/error).
    pub reader_spawned: bool,
}

/// Per-core TCP connection pool. `!Send` by design (Rc-based sharing).
///
/// The pool tracks idle (available) connections and the count of active
/// (checked-out) connections. The `max_connections` limit covers both:
/// `active + idle.len() <= max_connections`.
pub struct ConnectionPool {
    idle: VecDeque<PooledConnection>,
    active: usize,
    max_connections: usize,
}

impl ConnectionPool {
    /// Create a new pool. `max_connections = 0` means unlimited.
    pub fn new(max_connections: usize) -> Self {
        Self {
            idle: VecDeque::new(),
            active: 0,
            max_connections,
        }
    }

    /// Take an idle connection if one is available.
    ///
    /// This is a brief, synchronous operation safe for `RefCell` borrowing.
    pub fn take_idle(&mut self) -> Option<PooledConnection> {
        if let Some(conn) = self.idle.pop_front() {
            self.active += 1;
            Some(conn)
        } else {
            None
        }
    }

    /// Check if we can open a new connection without exceeding the limit.
    pub fn can_open_new(&self) -> bool {
        self.max_connections == 0 || (self.active + self.idle.len()) < self.max_connections
    }

    /// Record that a new connection was opened (it becomes active immediately).
    pub fn record_opened(&mut self) {
        self.active += 1;
    }

    /// Return a connection to the idle pool.
    pub fn return_idle(&mut self, conn: PooledConnection) {
        if self.active > 0 {
            self.active -= 1;
        }
        self.idle.push_back(conn);
    }

    /// Record that a connection was dropped (not returned to pool).
    pub fn record_dropped(&mut self) {
        if self.active > 0 {
            self.active -= 1;
        }
    }

    /// Number of idle connections.
    pub fn idle_count(&self) -> usize {
        self.idle.len()
    }

    /// Number of active (in-use) connections.
    pub fn active_count(&self) -> usize {
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_pool_is_empty() {
        let pool = ConnectionPool::new(10);
        assert_eq!(pool.idle_count(), 0);
        assert_eq!(pool.active_count(), 0);
    }

    #[test]
    fn take_idle_from_empty_returns_none() {
        let mut pool = ConnectionPool::new(10);
        assert!(pool.take_idle().is_none());
    }

    #[test]
    fn can_open_new_respects_limit() {
        let mut pool = ConnectionPool::new(2);
        assert!(pool.can_open_new());

        pool.record_opened();
        assert!(pool.can_open_new()); // 1 of 2

        pool.record_opened();
        assert!(!pool.can_open_new()); // 2 of 2
    }

    #[test]
    fn unlimited_pool_always_allows_new() {
        let mut pool = ConnectionPool::new(0);
        for _ in 0..100 {
            pool.record_opened();
        }
        assert!(pool.can_open_new());
    }

    #[test]
    fn record_dropped_decrements_active() {
        let mut pool = ConnectionPool::new(10);
        pool.record_opened();
        pool.record_opened();
        assert_eq!(pool.active_count(), 2);

        pool.record_dropped();
        assert_eq!(pool.active_count(), 1);
    }

    #[test]
    fn record_dropped_does_not_underflow() {
        let mut pool = ConnectionPool::new(10);
        pool.record_dropped(); // no-op, active is already 0
        assert_eq!(pool.active_count(), 0);
    }
}
