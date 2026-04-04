//! Event counter with distribution-sampled limits.
//!
//! [`LifecycleCounter`] counts events (requests, transactions, messages)
//! and signals when a distribution-sampled limit is reached. The caller
//! performs the lifecycle transition (close connection, end stream, etc.)
//! and calls [`reset()`](LifecycleCounter::reset) to resample a new limit.
//!
//! # Usage
//!
//! - HTTP `ConnectionPolicyTransformer`: requests per connection
//! - TCP executor: transactions per pooled connection
//! - Future HTTP/2: streams per connection, messages per stream

use netanvil_types::distribution::ValueDistribution;
use rand::rngs::SmallRng;
use rand::SeedableRng;

use crate::sampler::Sampler;

/// Counts events and signals when a distribution-sampled limit is reached.
///
/// After [`tick()`](LifecycleCounter::tick) returns `true`, the caller
/// should perform the lifecycle transition and call
/// [`reset()`](LifecycleCounter::reset) to resample a new limit.
pub struct LifecycleCounter {
    sampler: Sampler<u32>,
    count: u32,
    limit: u32,
    rng: SmallRng,
}

impl LifecycleCounter {
    /// Create a new counter, sampling the initial limit from `distribution`.
    /// Uses OS entropy for the internal RNG.
    pub fn new(distribution: ValueDistribution<u32>) -> Self {
        let sampler = Sampler::new(&distribution);
        let mut rng = SmallRng::from_entropy();
        let limit = sampler.sample(&mut rng);
        Self {
            sampler,
            count: 0,
            limit,
            rng,
        }
    }

    /// Create a counter with a deterministic seed for reproducible behavior.
    pub fn with_seed(distribution: ValueDistribution<u32>, seed: u64) -> Self {
        let sampler = Sampler::new(&distribution);
        let mut rng = SmallRng::seed_from_u64(seed);
        let limit = sampler.sample(&mut rng);
        Self {
            sampler,
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
        self.limit = self.sampler.sample(&mut self.rng);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_fires_after_limit() {
        let mut lc = LifecycleCounter::new(ValueDistribution::Fixed(3));
        assert!(!lc.tick()); // 1
        assert!(!lc.tick()); // 2
        assert!(!lc.tick()); // 3 (limit reached, fires AFTER limit)
        assert!(lc.tick()); // 4 => fires

        lc.reset();
        assert!(!lc.tick()); // 1
        assert!(!lc.tick()); // 2
        assert!(!lc.tick()); // 3
        assert!(lc.tick()); // 4 => fires
    }

    #[test]
    fn fixed_1_fires_after_one() {
        let mut lc = LifecycleCounter::new(ValueDistribution::Fixed(1));
        assert!(!lc.tick()); // 1 (the one allowed request)
        assert!(lc.tick()); // 2 => fires
    }
}
