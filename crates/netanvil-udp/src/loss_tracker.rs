//! Sliding-window bitmap for detecting UDP packet loss.
//!
//! Each outbound datagram is stamped with a `u64` sequence number.  When the
//! echo response arrives, the sequence number is looked up in a fixed-size
//! ring-buffer bitmap.  Bits that fall off the back of the window without
//! being set are counted as lost.
//!
//! The window is 4096 packets wide (64 × `u64` words).  At 100 k PPS the
//! window covers ~40 ms of reorder tolerance; at 10 k PPS ~400 ms.
//!
//! All operations are O(1) amortised.  Full-word eviction counts zero-bits
//! with `POPCNT` (one instruction per 64-bit word on x86-64).

const WORDS: usize = 64;
const WINDOW: usize = WORDS * 64; // 4096

/// Per-core UDP loss tracker.  `!Send` by design (one instance per I/O
/// worker core, no interior-mutability guards needed).
pub struct LossTracker {
    /// Sequence number corresponding to bit position `head` in the ring.
    base: u64,
    /// Bit-level position of `base` in the ring (0..WINDOW-1).
    head: usize,
    /// Circular bitmap — bit *set* means "response received".
    ring: [u64; WORDS],

    // Absolute counters (monotonically increasing, never reset).
    total_sent: u64,
    total_received: u64,
    total_lost: u64,

    // Snapshot values at last `take_deltas()` call.
    snap_sent: u64,
    snap_received: u64,
    snap_lost: u64,
}

impl LossTracker {
    pub fn new() -> Self {
        Self {
            base: 0,
            head: 0,
            ring: [0; WORDS],
            total_sent: 0,
            total_received: 0,
            total_lost: 0,
            snap_sent: 0,
            snap_received: 0,
            snap_lost: 0,
        }
    }

    /// Record an outbound datagram.  Returns the sequence number to embed
    /// in the 8-byte packet header.
    #[inline]
    pub fn on_send(&mut self) -> u64 {
        let seq = self.total_sent;
        self.total_sent += 1;
        seq
    }

    /// Record a received echo response carrying the given sequence number.
    #[inline]
    pub fn on_receive(&mut self, seq: u64) {
        if seq < self.base {
            return; // ancient duplicate — behind the window
        }

        let offset = (seq - self.base) as usize;

        if offset >= WINDOW {
            self.advance(offset - WINDOW + 1);
        }

        let offset = (seq - self.base) as usize;
        let abs = (self.head + offset) % WINDOW;
        let word = abs / 64;
        let bit = abs % 64;
        let mask = 1u64 << bit;

        if self.ring[word] & mask == 0 {
            self.ring[word] |= mask;
            self.total_received += 1;
        }
    }

    /// Return `(sent, received, lost)` deltas since the last call.
    pub fn take_deltas(&mut self) -> (u64, u64, u64) {
        let ds = self.total_sent - self.snap_sent;
        let dr = self.total_received - self.snap_received;
        let dl = self.total_lost - self.snap_lost;
        self.snap_sent = self.total_sent;
        self.snap_received = self.total_received;
        self.snap_lost = self.total_lost;
        (ds, dr, dl)
    }

    /// Slide the window forward by `count` positions.  Unset bits whose
    /// sequence numbers were actually sent are counted as lost.
    fn advance(&mut self, count: usize) {
        let count = count.min(WINDOW);
        let mut pos = self.head;
        let mut seq = self.base;
        let mut remaining = count;

        while remaining > 0 {
            let word_idx = pos / 64;
            let start_bit = pos % 64;
            let bits_in_word = (64 - start_bit).min(remaining);

            if start_bit == 0 && bits_in_word == 64 {
                // Full word — bulk POPCNT.
                let set = self.ring[word_idx].count_ones() as u64;
                let expected = 64u64.min(self.total_sent.saturating_sub(seq));
                self.total_lost += expected.saturating_sub(set);
                self.ring[word_idx] = 0;
            } else {
                // Partial word — mask and count.
                let mask = if bits_in_word == 64 {
                    u64::MAX
                } else {
                    ((1u64 << bits_in_word) - 1) << start_bit
                };
                let set = (self.ring[word_idx] & mask).count_ones() as u64;
                let expected =
                    (bits_in_word as u64).min(self.total_sent.saturating_sub(seq));
                self.total_lost += expected.saturating_sub(set);
                self.ring[word_idx] &= !mask;
            }

            pos = (pos + bits_in_word) % WINDOW;
            seq += bits_in_word as u64;
            remaining -= bits_in_word;
        }

        self.head = pos;
        self.base += count as u64;
    }
}

impl Default for LossTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ── PacketCounterSource adapter ──────────────────────────────────────

use std::cell::RefCell;
use std::rc::Rc;

use netanvil_types::{PacketCounterDeltas, PacketCounterSource};

/// Packet counter source backed by the sliding-window loss tracker.
///
/// Wraps `Rc<RefCell<LossTracker>>` for shared ownership between the
/// executor (writes on send/receive) and the collector (reads at snapshot).
pub struct UdpPacketSource(pub Rc<RefCell<LossTracker>>);

impl Default for UdpPacketSource {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(LossTracker::new())))
    }
}

impl PacketCounterSource for UdpPacketSource {
    fn take_packet_deltas(&self) -> PacketCounterDeltas {
        let (sent, received, lost) = self.0.borrow_mut().take_deltas();
        PacketCounterDeltas {
            sent,
            received,
            lost,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_loss_sequential() {
        let mut t = LossTracker::new();
        for _ in 0..100 {
            let seq = t.on_send();
            t.on_receive(seq);
        }
        let (sent, received, lost) = t.take_deltas();
        assert_eq!(sent, 100);
        assert_eq!(received, 100);
        assert_eq!(lost, 0);
    }

    #[test]
    fn single_lost_packet() {
        let mut t = LossTracker::new();
        for i in 0..10u64 {
            t.on_send();
            if i != 5 {
                t.on_receive(i);
            }
        }
        // Advance window past the gap.
        for i in 10..WINDOW as u64 + 20 {
            t.on_send();
            t.on_receive(i);
        }
        let (sent, _received, lost) = t.take_deltas();
        assert_eq!(lost, 1, "exactly one packet (seq 5) should be lost");
        assert_eq!(sent, WINDOW as u64 + 20);
    }

    #[test]
    fn duplicate_response_not_double_counted() {
        let mut t = LossTracker::new();
        let seq = t.on_send();
        t.on_receive(seq);
        t.on_receive(seq);
        let (_sent, received, _lost) = t.take_deltas();
        assert_eq!(received, 1);
    }

    #[test]
    fn ancient_response_ignored() {
        let mut t = LossTracker::new();
        for i in 0..WINDOW as u64 + 100 {
            t.on_send();
            t.on_receive(i);
        }
        // Ancient seq behind the window.
        t.on_receive(0);
        let (_sent, _received, lost) = t.take_deltas();
        assert_eq!(lost, 0);
    }

    #[test]
    fn fire_and_forget_all_lost() {
        let mut t = LossTracker::new();
        for _ in 0..200 {
            t.on_send();
        }
        // Advance past everything.
        for _ in 200..WINDOW as u64 + 300 {
            t.on_send();
        }
        t.on_receive(WINDOW as u64 + 299);
        let (_sent, _received, lost) = t.take_deltas();
        assert!(lost >= 200, "at least 200 should be lost, got {lost}");
    }

    #[test]
    fn reordered_within_window() {
        let mut t = LossTracker::new();
        for _ in 0..10 {
            t.on_send();
        }
        for i in (0..10u64).rev() {
            t.on_receive(i);
        }
        let (_sent, received, lost) = t.take_deltas();
        assert_eq!(received, 10);
        assert_eq!(lost, 0);
    }

    #[test]
    fn deltas_are_incremental() {
        let mut t = LossTracker::new();
        for _ in 0..50 {
            let seq = t.on_send();
            t.on_receive(seq);
        }
        let (s1, r1, l1) = t.take_deltas();
        assert_eq!((s1, r1, l1), (50, 50, 0));

        for _ in 0..30 {
            let seq = t.on_send();
            t.on_receive(seq);
        }
        let (s2, r2, l2) = t.take_deltas();
        assert_eq!((s2, r2, l2), (30, 30, 0));
    }

    #[test]
    fn wraps_around_ring_multiple_times() {
        let mut t = LossTracker::new();
        // Send and receive 3× the window size with no loss.
        let total = WINDOW as u64 * 3;
        for _ in 0..total {
            let seq = t.on_send();
            t.on_receive(seq);
        }
        let (sent, received, lost) = t.take_deltas();
        assert_eq!(sent, total);
        assert_eq!(received, total);
        assert_eq!(lost, 0);
    }

    #[test]
    fn loss_rate_under_sustained_load() {
        let mut t = LossTracker::new();
        // 1% loss: drop every 100th packet.
        let total = 10_000u64;
        for i in 0..total {
            t.on_send();
            if i % 100 != 50 {
                t.on_receive(i);
            }
        }
        // Flush remaining window.
        for i in total..total + WINDOW as u64 {
            t.on_send();
            t.on_receive(i);
        }
        let (sent, received, lost) = t.take_deltas();
        assert_eq!(lost, 100, "1% of 10000 = 100 lost");
        assert_eq!(received, sent - lost);
    }
}
