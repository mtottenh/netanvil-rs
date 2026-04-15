//! Per-worker buffer pool for hot-path allocation reuse.
//!
//! compio's completion-based I/O takes ownership of buffers during operations.
//! Rather than allocating fresh `Vec<u8>` per operation and cloning response
//! buffers on every write, the pool recycles buffers returned after I/O completes.
//!
//! Buffers are pre-filled with PRNG data (not zeros) to avoid compression
//! artifacts and zero-page kernel optimizations that would distort measurements.
//!
//! Not `Send` — one pool per worker thread, no cross-thread sharing.

use std::cell::RefCell;

use rand::rngs::SmallRng;
use rand::{RngCore, SeedableRng};

/// Deterministic seed for reproducible buffer contents across runs.
const PRNG_SEED: u64 = 0xDEAD_BEEF_CAFE_F00D;

/// Per-worker buffer pool for hot-path allocation reuse.
pub struct BufPool {
    free: RefCell<Vec<Vec<u8>>>,
    buf_size: usize,
    high_water: usize,
    rng: RefCell<SmallRng>,
}

impl BufPool {
    /// Create a pool that fills buffers with deterministic PRNG data.
    ///
    /// Pre-allocates `high_water` buffers of `buf_size` bytes, each filled
    /// with PRNG data seeded from a fixed constant. Two test runs with the
    /// same config see identical buffer contents.
    pub fn new(buf_size: usize, high_water: usize) -> Self {
        Self {
            free: RefCell::new(Vec::with_capacity(high_water)),
            buf_size,
            high_water,
            rng: RefCell::new(SmallRng::seed_from_u64(PRNG_SEED)),
        }
    }

    /// Create a pool that tiles a pattern across each buffer.
    ///
    /// The pattern is repeated to fill `buf_size` bytes. Useful for custom
    /// fill data loaded from a file via `--fill-data`.
    pub fn with_pattern(buf_size: usize, high_water: usize, pattern: &[u8]) -> Self {
        let pattern = pattern.to_vec();
        let mut free = Vec::with_capacity(high_water);
        for _ in 0..high_water {
            free.push(tile_pattern(&pattern, buf_size));
        }
        Self {
            free: RefCell::new(free),
            buf_size,
            high_water,
            rng: RefCell::new(SmallRng::seed_from_u64(PRNG_SEED)),
        }
    }

    /// Take a buffer from the pool, or allocate a new PRNG-filled one.
    ///
    /// Fresh buffers have `len == buf_size`. Recycled buffers may have any
    /// `len` (from a prior truncate or compio operation) but always have
    /// `capacity >= buf_size`.
    ///
    /// - For **writes**: truncate/resize to the desired size.
    /// - For **reads**: pass as-is — compio reads into `0..capacity`.
    pub fn take(&self) -> Vec<u8> {
        self.free.borrow_mut().pop().unwrap_or_else(|| {
            let mut buf = vec![0u8; self.buf_size];
            self.rng.borrow_mut().fill_bytes(&mut buf);
            buf
        })
    }

    /// Buffer size this pool was created with.
    pub fn buf_size(&self) -> usize {
        self.buf_size
    }

    /// Return a buffer to the pool for reuse.
    ///
    /// If the pool is already at `high_water`, the buffer is dropped.
    pub fn give(&self, buf: Vec<u8>) {
        let mut free = self.free.borrow_mut();
        if free.len() < self.high_water {
            free.push(buf);
        }
    }

    /// Pre-allocate up to `count` buffers if the pool has fewer.
    pub fn pre_allocate(&self, count: usize) {
        let mut free = self.free.borrow_mut();
        let mut rng = self.rng.borrow_mut();
        while free.len() < count {
            let mut buf = vec![0u8; self.buf_size];
            rng.fill_bytes(&mut buf);
            free.push(buf);
        }
    }
}

/// Tile `pattern` to fill exactly `size` bytes.
fn tile_pattern(pattern: &[u8], size: usize) -> Vec<u8> {
    if pattern.is_empty() {
        return vec![0u8; size];
    }
    let mut buf = Vec::with_capacity(size);
    while buf.len() < size {
        let remaining = size - buf.len();
        let chunk = remaining.min(pattern.len());
        buf.extend_from_slice(&pattern[..chunk]);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_take_returns_correct_size() {
        let pool = BufPool::new(1024, 4);
        let buf = pool.take();
        assert_eq!(buf.len(), 1024);
    }

    #[test]
    fn test_take_returns_nonzero_data() {
        let pool = BufPool::new(1024, 4);
        let buf = pool.take();
        // PRNG fill should produce non-zero data
        let nonzero_count = buf.iter().filter(|&&b| b != 0).count();
        assert!(
            nonzero_count > 900,
            "expected high entropy, got {} nonzero bytes out of 1024",
            nonzero_count
        );
    }

    #[test]
    fn test_give_and_take_recycles() {
        let pool = BufPool::new(64, 2);
        let b1 = pool.take();
        pool.give(b1);
        // Should get it back from the free list
        let b2 = pool.take();
        assert_eq!(b2.len(), 64);
        pool.give(b2);
    }

    #[test]
    fn test_high_water_limit() {
        let pool = BufPool::new(64, 2);
        let a = pool.take();
        let b = pool.take();
        let c = pool.take();
        // Give all 3 back — only 2 should be kept
        pool.give(a);
        pool.give(b);
        pool.give(c); // dropped
        assert_eq!(pool.free.borrow().len(), 2);
    }

    #[test]
    fn test_deterministic_prng() {
        let pool1 = BufPool::new(256, 1);
        let pool2 = BufPool::new(256, 1);
        let b1 = pool1.take();
        let b2 = pool2.take();
        assert_eq!(b1, b2, "same seed should produce identical buffers");
    }

    #[test]
    fn test_with_pattern() {
        let pattern = b"ABCD";
        let pool = BufPool::with_pattern(10, 1, pattern);
        let buf = pool.take();
        assert_eq!(&buf[..10], b"ABCDABCDAB");
    }

    #[test]
    fn test_take_preserves_capacity_after_truncate() {
        let pool = BufPool::new(128, 2);
        let mut buf = pool.take();
        assert_eq!(buf.len(), 128);
        buf.truncate(10); // simulate post-write state
        assert_eq!(buf.capacity(), 128);
        pool.give(buf);
        let buf2 = pool.take();
        // take() returns buf as-is; capacity is preserved for compio reads
        assert_eq!(buf2.capacity(), 128);
    }

    #[test]
    fn test_pre_allocate() {
        let pool = BufPool::new(64, 16);
        assert_eq!(pool.free.borrow().len(), 0);
        pool.pre_allocate(8);
        assert_eq!(pool.free.borrow().len(), 8);
        // All pre-allocated buffers should be PRNG-filled
        let buf = pool.take();
        let nonzero = buf.iter().filter(|&&b| b != 0).count();
        assert!(nonzero > 50, "pre-allocated buffers should be PRNG-filled");
    }
}
