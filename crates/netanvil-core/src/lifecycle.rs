//! Generic distribution sampling and lifecycle counting.
//!
//! Two building blocks:
//!
//! - [`SampleDistribution`] — extension trait that adds `.sample(rng)` to
//!   [`ValueDistribution<T>`].  Lives here (not in `netanvil-types`) because
//!   `netanvil-types` has zero runtime deps and sampling needs `rand`.
//!
//! - [`LifecycleCounter`] — stateful counter that fires when a
//!   distribution-sampled limit is reached.  Replaces hand-rolled counting
//!   in `ConnectionPolicyTransformer`, TCP executor, and future HTTP/2
//!   stream lifecycle management.

use netanvil_types::distribution::{ValueDistribution, WeightedValue};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Distribution, Normal};

// ---------------------------------------------------------------------------
// SampleDistribution trait
// ---------------------------------------------------------------------------

/// Extension trait: sample a value from a [`ValueDistribution`].
///
/// Implemented for concrete numeric types used in the codebase.
pub trait SampleDistribution<T> {
    fn sample(&self, rng: &mut SmallRng) -> T;
}

// ── impl for u32 (connection lifetimes, transaction counts) ──────────────

impl SampleDistribution<u32> for ValueDistribution<u32> {
    fn sample(&self, rng: &mut SmallRng) -> u32 {
        match self {
            ValueDistribution::Fixed(n) => *n,
            ValueDistribution::Uniform { min, max } => rng.gen_range(*min..=*max),
            ValueDistribution::Normal { mean, stddev } => {
                let normal = Normal::new(*mean, *stddev)
                    .unwrap_or_else(|_| Normal::new(1.0, 0.0).unwrap());
                let s: f64 = normal.sample(rng);
                s.round().max(1.0) as u32
            }
            ValueDistribution::Weighted(entries) => sample_weighted(entries, rng),
        }
    }
}

// ── impl for u16 (TCP request sizes) ─────────────────────────────────────

impl SampleDistribution<u16> for ValueDistribution<u16> {
    fn sample(&self, rng: &mut SmallRng) -> u16 {
        match self {
            ValueDistribution::Fixed(n) => *n,
            ValueDistribution::Uniform { min, max } => rng.gen_range(*min..=*max),
            ValueDistribution::Normal { mean, stddev } => {
                let normal = Normal::new(*mean, *stddev)
                    .unwrap_or_else(|_| Normal::new(1.0, 0.0).unwrap());
                let s: f64 = normal.sample(rng);
                s.round().max(1.0).min(u16::MAX as f64) as u16
            }
            ValueDistribution::Weighted(entries) => sample_weighted(entries, rng),
        }
    }
}

// ── impl for usize (UDP payload sizes, index selection) ──────────────────

impl SampleDistribution<usize> for ValueDistribution<usize> {
    fn sample(&self, rng: &mut SmallRng) -> usize {
        match self {
            ValueDistribution::Fixed(n) => *n,
            ValueDistribution::Uniform { min, max } => rng.gen_range(*min..=*max),
            ValueDistribution::Normal { mean, stddev } => {
                let normal = Normal::new(*mean, *stddev)
                    .unwrap_or_else(|_| Normal::new(1.0, 0.0).unwrap());
                let s: f64 = normal.sample(rng);
                s.round().max(1.0) as usize
            }
            ValueDistribution::Weighted(entries) => sample_weighted(entries, rng),
        }
    }
}

/// Weighted selection: linear scan over cumulative weights.
///
/// For typical use (3-10 entries) linear scan beats binary search.
fn sample_weighted<T: Clone>(entries: &[WeightedValue<T>], rng: &mut SmallRng) -> T {
    debug_assert!(!entries.is_empty(), "weighted distribution must have entries");

    let total: f64 = entries.iter().map(|e| e.weight).sum();
    let roll: f64 = rng.gen_range(0.0..total);

    let mut cumulative = 0.0;
    for entry in entries {
        cumulative += entry.weight;
        if roll < cumulative {
            return entry.value.clone();
        }
    }
    // Floating-point edge case: return last entry.
    entries.last().unwrap().value.clone()
}

// ---------------------------------------------------------------------------
// LifecycleCounter
// ---------------------------------------------------------------------------

/// Counts events and signals when a distribution-sampled limit is reached.
///
/// After [`tick()`](LifecycleCounter::tick) returns `true`, the caller
/// performs the lifecycle transition (close connection, end stream, etc.)
/// and calls [`reset()`](LifecycleCounter::reset) to resample.
///
/// # Usage
///
/// Replaces hand-rolled `Cell<u32>` + `sample_count()` patterns:
///
/// - HTTP `ConnectionPolicyTransformer`: requests per connection
/// - TCP executor: transactions per connection
/// - Future HTTP/2: streams per connection, messages per stream
pub struct LifecycleCounter {
    distribution: ValueDistribution<u32>,
    count: u32,
    limit: u32,
    rng: SmallRng,
}

impl LifecycleCounter {
    /// Create a new counter, sampling the initial limit from `distribution`.
    pub fn new(distribution: ValueDistribution<u32>) -> Self {
        let mut rng = SmallRng::from_entropy();
        let limit = distribution.sample(&mut rng);
        Self {
            distribution,
            count: 0,
            limit,
            rng,
        }
    }

    /// Increment counter. Returns `true` when the count exceeds the sampled
    /// limit (i.e., after `limit` events have occurred).
    ///
    /// With `Fixed(10)`, the first 10 ticks return `false`, the 11th returns
    /// `true`. This matches the semantics of "N requests per connection".
    #[inline]
    pub fn tick(&mut self) -> bool {
        self.count += 1;
        self.count > self.limit
    }

    /// Reset counter and resample a fresh limit from the distribution.
    pub fn reset(&mut self) {
        self.count = 0;
        self.limit = self.distribution.sample(&mut self.rng);
    }

    /// Current count of events since last reset.
    pub fn count(&self) -> u32 {
        self.count
    }

    /// Current sampled limit.
    pub fn limit(&self) -> u32 {
        self.limit
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use netanvil_types::distribution::WeightedValue;

    #[test]
    fn fixed_always_returns_same_value() {
        let dist = ValueDistribution::Fixed(42u32);
        let mut rng = SmallRng::seed_from_u64(0);
        for _ in 0..100 {
            assert_eq!(dist.sample(&mut rng), 42);
        }
    }

    #[test]
    fn uniform_stays_in_range() {
        let dist = ValueDistribution::Uniform {
            min: 10u32,
            max: 20,
        };
        let mut rng = SmallRng::seed_from_u64(0);
        for _ in 0..1000 {
            let v = dist.sample(&mut rng);
            assert!((10..=20).contains(&v), "got {v}");
        }
    }

    #[test]
    fn normal_clamps_to_at_least_1() {
        let dist: ValueDistribution<u32> = ValueDistribution::Normal {
            mean: 1.0,
            stddev: 10.0,
        };
        let mut rng = SmallRng::seed_from_u64(0);
        for _ in 0..1000 {
            let v = dist.sample(&mut rng);
            assert!(v >= 1, "got {v}");
        }
    }

    #[test]
    fn weighted_respects_proportions() {
        let dist = ValueDistribution::Weighted(vec![
            WeightedValue::new(1u32, 1.0),
            WeightedValue::new(2u32, 3.0),
        ]);
        let mut rng = SmallRng::seed_from_u64(42);
        let mut counts = [0u32; 3];
        for _ in 0..10_000 {
            let v = dist.sample(&mut rng);
            counts[v as usize] += 1;
        }
        // ~25% should be 1, ~75% should be 2
        let ratio = counts[1] as f64 / counts[2] as f64;
        assert!(
            (0.2..0.45).contains(&ratio),
            "expected ~0.33, got {ratio} (counts: {:?})",
            counts
        );
    }

    #[test]
    fn lifecycle_counter_fixed_fires_after_limit() {
        let mut lc = LifecycleCounter::new(ValueDistribution::Fixed(3));
        assert!(!lc.tick()); // 1
        assert!(!lc.tick()); // 2
        assert!(!lc.tick()); // 3 (limit reached, but fires AFTER limit)
        assert!(lc.tick()); // 4 => fires (exceeds limit)

        lc.reset();
        assert!(!lc.tick()); // 1 again
        assert!(!lc.tick()); // 2
        assert!(!lc.tick()); // 3
        assert!(lc.tick()); // 4 => fires again
    }

    #[test]
    fn lifecycle_counter_fixed_1_fires_after_one() {
        let mut lc = LifecycleCounter::new(ValueDistribution::Fixed(1));
        assert!(!lc.tick()); // 1 (the one allowed request)
        assert!(lc.tick()); // 2 => fires (exceeds limit)
    }

    #[test]
    fn u16_uniform_stays_in_range() {
        let dist = ValueDistribution::Uniform {
            min: 200u16,
            max: 1500,
        };
        let mut rng = SmallRng::seed_from_u64(0);
        for _ in 0..1000 {
            let v: u16 = dist.sample(&mut rng);
            assert!((200..=1500).contains(&v), "got {v}");
        }
    }

    #[test]
    fn usize_weighted_works() {
        let dist = ValueDistribution::Weighted(vec![
            WeightedValue::new(200usize, 0.3),
            WeightedValue::new(1200usize, 0.5),
            WeightedValue::new(1500usize, 0.2),
        ]);
        let mut rng = SmallRng::seed_from_u64(42);
        let v: usize = dist.sample(&mut rng);
        assert!([200, 1200, 1500].contains(&v));
    }
}
