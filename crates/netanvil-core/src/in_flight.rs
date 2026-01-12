//! Per-core limit on in-flight requests.
//!
//! Non-blocking: `try_acquire` returns `None` at capacity, caller counts
//! as a drop. No async `acquire` — we don't want to stall the worker,
//! we want to drop and count so the timer thread's fire channel backs up
//! and the backpressure is visible in metrics.
//!
//! Uses `Cell` (not atomic) because compio is thread-per-core — no
//! cross-thread synchronization needed within a single I/O worker.

use std::cell::Cell;
use std::rc::Rc;

/// Per-core limit on in-flight requests.
///
/// Enforces a hard upper bound on concurrent async tasks per I/O core.
/// When the target system slows down, in-flight requests accumulate.
/// Without a limit, this is unbounded (`spawn().detach()` creates unlimited
/// tasks). With the limit, excess fire events are dropped and counted,
/// creating visible backpressure.
pub struct InFlightLimit {
    count: Cell<usize>,
    max: usize,
}

/// Owned RAII permit — decrements the in-flight count on drop.
/// Holds an `Rc<InFlightLimit>` so it can be moved into spawned tasks.
pub struct OwnedPermit {
    limit: Rc<InFlightLimit>,
}

impl InFlightLimit {
    /// Create a new limit. `max = 0` means unlimited (no enforcement).
    pub fn new(max: usize) -> Self {
        Self {
            count: Cell::new(0),
            max,
        }
    }

    /// Try to acquire an owned permit (movable into async tasks).
    /// Returns `None` if at capacity.
    ///
    /// Non-blocking — caller should count the `None` case as an
    /// `in_flight_drop` for metrics.
    pub fn try_acquire(self: &Rc<Self>) -> Option<OwnedPermit> {
        if self.max > 0 && self.count.get() >= self.max {
            return None;
        }
        self.count.set(self.count.get() + 1);
        Some(OwnedPermit {
            limit: self.clone(),
        })
    }

    /// Current number of in-flight requests.
    pub fn in_flight(&self) -> usize {
        self.count.get()
    }

    /// Maximum capacity (0 = unlimited).
    pub fn capacity(&self) -> usize {
        self.max
    }
}

impl Drop for OwnedPermit {
    fn drop(&mut self) {
        let c = self.limit.count.get();
        debug_assert!(c > 0, "in-flight count underflow");
        self.limit.count.set(c - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_always_acquires() {
        let limit = Rc::new(InFlightLimit::new(0));
        for _ in 0..10000 {
            assert!(limit.try_acquire().is_some());
        }
    }

    #[test]
    fn respects_capacity() {
        let limit = Rc::new(InFlightLimit::new(3));
        let _p1 = limit.try_acquire().unwrap();
        let _p2 = limit.try_acquire().unwrap();
        let _p3 = limit.try_acquire().unwrap();
        assert_eq!(limit.in_flight(), 3);

        // At capacity — should return None
        assert!(limit.try_acquire().is_none());
        assert_eq!(limit.in_flight(), 3);
    }

    #[test]
    fn releases_on_drop() {
        let limit = Rc::new(InFlightLimit::new(2));
        let p1 = limit.try_acquire().unwrap();
        let _p2 = limit.try_acquire().unwrap();
        assert!(limit.try_acquire().is_none());

        drop(p1);
        assert_eq!(limit.in_flight(), 1);
        assert!(limit.try_acquire().is_some());
    }

    #[test]
    fn capacity_reports_correctly() {
        let limit = Rc::new(InFlightLimit::new(512));
        assert_eq!(limit.capacity(), 512);
        assert_eq!(limit.in_flight(), 0);

        let _p = limit.try_acquire().unwrap();
        assert_eq!(limit.in_flight(), 1);
    }

    #[test]
    fn permit_is_send_not_required() {
        // OwnedPermit holds Rc, which is !Send. This is intentional:
        // compio is thread-per-core, permits never cross threads.
        // This test just verifies the type compiles.
        let limit = Rc::new(InFlightLimit::new(10));
        let permit = limit.try_acquire().unwrap();
        assert_eq!(limit.in_flight(), 1);
        drop(permit);
        assert_eq!(limit.in_flight(), 0);
    }
}
