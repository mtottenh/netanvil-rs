//! Generic value distribution types.
//!
//! `ValueDistribution<T>` describes how to draw values of type `T` from a
//! statistical distribution.  The enum is pure data — it carries no RNG state
//! and has no runtime dependencies beyond `serde`.  Sampling is provided by
//! the `Sample` extension trait in `netanvil-core`, which owns the `rand`
//! dependency.
//!
//! # Variants
//!
//! | Variant       | Description                                            |
//! |---------------|--------------------------------------------------------|
//! | `Fixed`       | Always returns the same value (zero-cost at call site) |
//! | `Uniform`     | Uniform random in `[min, max]`                         |
//! | `Normal`      | Gaussian, rounded and clamped to `>= 1`                |
//! | `Weighted`    | Weighted random selection from discrete values         |
//! | `Exponential` | Memoryless; natural for connection lifetimes            |
//! | `LogNormal`   | Right-skewed, always positive; natural for sizes        |
//! | `Pareto`      | Heavy-tailed power-law; natural for object/file sizes   |
//! | `Zipf`        | Rank-frequency; natural for key/index selection         |

use serde::{Deserialize, Serialize};

/// A distribution over values of type `T`.
///
/// Used to model realistic per-request variation in packet sizes, connection
/// lifetimes, transactions per connection, and similar quantities.
///
/// `Fixed(v)` compiles to a simple return — no branch, no allocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "params")]
pub enum ValueDistribution<T> {
    /// Always returns the same value.
    Fixed(T),
    /// Uniform random in `[min, max]` (inclusive).
    Uniform { min: T, max: T },
    /// Gaussian distribution. The sampled `f64` is rounded and clamped to
    /// `>= 1` before conversion to `T`.
    Normal { mean: f64, stddev: f64 },
    /// Weighted random selection from a discrete set of values.
    ///
    /// Weights are relative (not required to sum to 1.0).
    /// Example: `[(200, 0.3), (1200, 0.5), (1500, 0.2)]` means 30%/50%/20%.
    Weighted(Vec<WeightedValue<T>>),

    /// Exponential distribution with the given mean.
    ///
    /// Memoryless: P(X > s+t | X > s) = P(X > t). The natural model for
    /// connection lifetimes (each request has an independent probability of
    /// being the last) and inter-arrival/think times. Already used internally
    /// by `PoissonScheduler` for inter-arrival intervals.
    ///
    /// Sampled as `f64`, rounded, clamped to `>= 1`.
    Exponential { mean: f64 },

    /// Log-normal distribution.
    ///
    /// Always positive and right-skewed — strictly better than `Normal` for
    /// modeling sizes (packet payloads, HTTP responses, file objects) where
    /// values are multiplicative and cannot be negative.
    ///
    /// `mu` and `sigma` are the mean and standard deviation of the underlying
    /// normal distribution on the log scale. The resulting distribution has
    /// median `exp(mu)` and is more skewed as `sigma` increases.
    ///
    /// Sampled as `f64`, rounded, clamped to `>= 1`.
    LogNormal { mu: f64, sigma: f64 },

    /// Pareto (power-law) distribution.
    ///
    /// Models heavy-tailed quantities: many small values, few very large ones.
    /// The 80/20 rule is a Pareto distribution. Natural for CDN object sizes,
    /// web page sizes, and file size distributions.
    ///
    /// `scale` (x_m) is the minimum possible value. `shape` (α) controls the
    /// tail weight — smaller α means heavier tails.
    ///
    /// Sampled as `f64`, rounded, clamped to `>= 1`.
    Pareto { scale: f64, shape: f64 },

    /// Zipf (zeta) distribution over ranks `1..=n`.
    ///
    /// Rank `k` has probability proportional to `1/k^exponent`. The standard
    /// model for key/index access patterns in caches, databases, and content
    /// systems. Without Zipfian key selection, Redis/cache benchmarks produce
    /// unrealistically uniform hit rates.
    ///
    /// Typical exponents: 0.99 (YCSB default), 1.0 (classic Zipf), 1.2+
    /// (highly skewed).
    Zipf { n: u64, exponent: f64 },
}

/// A value with an associated relative weight for `ValueDistribution::Weighted`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WeightedValue<T> {
    pub value: T,
    pub weight: f64,
}

impl<T> WeightedValue<T> {
    pub fn new(value: T, weight: f64) -> Self {
        Self { value, weight }
    }
}

impl<T: Default> Default for ValueDistribution<T> {
    fn default() -> Self {
        ValueDistribution::Fixed(T::default())
    }
}

/// Backward-compatible alias: `CountDistribution` is `ValueDistribution<u32>`.
pub type CountDistribution = ValueDistribution<u32>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_serde_roundtrip() {
        let d: ValueDistribution<u32> = ValueDistribution::Fixed(42);
        let json = serde_json::to_string(&d).unwrap();
        let parsed: ValueDistribution<u32> = serde_json::from_str(&json).unwrap();
        assert_eq!(d, parsed);
    }

    #[test]
    fn uniform_serde_roundtrip() {
        let d: ValueDistribution<u16> = ValueDistribution::Uniform { min: 64, max: 1500 };
        let json = serde_json::to_string(&d).unwrap();
        let parsed: ValueDistribution<u16> = serde_json::from_str(&json).unwrap();
        assert_eq!(d, parsed);
    }

    #[test]
    fn weighted_serde_roundtrip() {
        let d: ValueDistribution<u16> = ValueDistribution::Weighted(vec![
            WeightedValue::new(200, 0.3),
            WeightedValue::new(1200, 0.5),
            WeightedValue::new(1500, 0.2),
        ]);
        let json = serde_json::to_string(&d).unwrap();
        let parsed: ValueDistribution<u16> = serde_json::from_str(&json).unwrap();
        assert_eq!(d, parsed);
    }

    #[test]
    fn count_distribution_alias() {
        let d: CountDistribution = CountDistribution::Fixed(100);
        assert_eq!(d, ValueDistribution::Fixed(100u32));
    }
}
