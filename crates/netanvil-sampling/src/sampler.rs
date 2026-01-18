//! Pre-compiled distribution sampler.
//!
//! [`Sampler<T>`] is the runtime counterpart to
//! [`ValueDistribution<T>`](netanvil_types::distribution::ValueDistribution).
//! It is constructed once from a `ValueDistribution<T>`, pre-building the
//! `rand_distr` objects so that repeated `.sample()` calls only pay the RNG
//! cost.

use netanvil_types::distribution::{ValueDistribution, WeightedValue};
use rand::rngs::SmallRng;
use rand::Rng;
use rand_distr::{Distribution, Exp, LogNormal, Normal, Pareto, Zipf};

use crate::convert::SampleOutput;

/// A pre-compiled sampler over values of type `T`.
///
/// Constructed from a [`ValueDistribution<T>`] via [`Sampler::new`].
/// The `rand_distr` distribution object (if any) is built once at
/// construction time and reused on every [`sample`](Sampler::sample) call.
///
/// The RNG is **not** owned by the sampler — callers pass `&mut SmallRng`
/// because they typically share one RNG across multiple samplers and other
/// random choices (e.g., target selection).
#[derive(Debug, Clone)]
pub struct Sampler<T: SampleOutput> {
    kind: SamplerKind<T>,
}

#[derive(Debug, Clone)]
enum SamplerKind<T> {
    /// Always returns the same value. Zero-cost.
    Fixed(T),
    /// Integer uniform in `[min, max]`. No float intermediary.
    Uniform { min: T, max: T },
    /// Gaussian.
    Normal(Normal<f64>),
    /// Memoryless — natural for connection lifetimes and inter-arrival times.
    Exponential(Exp<f64>),
    /// Right-skewed, always positive — natural for sizes.
    LogNormal(LogNormal<f64>),
    /// Heavy-tailed power-law — natural for file/object sizes.
    Pareto(Pareto<f64>),
    /// Rank-frequency — natural for key/index selection.
    Zipf(Zipf<f64>),
    /// Weighted discrete selection with pre-computed cumulative weights.
    Weighted {
        values: Vec<T>,
        cumulative_weights: Vec<f64>,
        total: f64,
    },
}

impl<T: SampleOutput> Sampler<T> {
    /// Build a sampler from a distribution config.
    ///
    /// Pre-builds the internal `rand_distr` object and pre-computes
    /// cumulative weights for `Weighted` distributions. If distribution
    /// parameters are invalid (e.g., negative stddev), falls back to a
    /// safe default matching the existing behavior.
    pub fn new(dist: &ValueDistribution<T>) -> Self {
        let kind = match dist {
            ValueDistribution::Fixed(v) => SamplerKind::Fixed(v.clone()),

            ValueDistribution::Uniform { min, max } => SamplerKind::Uniform {
                min: min.clone(),
                max: max.clone(),
            },

            ValueDistribution::Normal { mean, stddev } => {
                let d =
                    Normal::new(*mean, *stddev).unwrap_or_else(|_| Normal::new(1.0, 0.0).unwrap());
                SamplerKind::Normal(d)
            }

            ValueDistribution::Exponential { mean } => {
                let d = Exp::new(1.0 / *mean).unwrap_or_else(|_| Exp::new(1.0).unwrap());
                SamplerKind::Exponential(d)
            }

            ValueDistribution::LogNormal { mu, sigma } => {
                let d = LogNormal::new(*mu, *sigma)
                    .unwrap_or_else(|_| LogNormal::new(0.0, 1.0).unwrap());
                SamplerKind::LogNormal(d)
            }

            ValueDistribution::Pareto { scale, shape } => {
                let d =
                    Pareto::new(*scale, *shape).unwrap_or_else(|_| Pareto::new(1.0, 1.0).unwrap());
                SamplerKind::Pareto(d)
            }

            ValueDistribution::Zipf { n, exponent } => {
                let d = Zipf::new(*n, *exponent).unwrap_or_else(|_| Zipf::new(1, 1.0).unwrap());
                SamplerKind::Zipf(d)
            }

            ValueDistribution::Weighted(entries) => build_weighted(entries),
        };

        Self { kind }
    }

    /// Draw a single sample from the distribution.
    #[inline]
    pub fn sample(&self, rng: &mut SmallRng) -> T {
        match &self.kind {
            SamplerKind::Fixed(v) => v.clone(),
            SamplerKind::Uniform { min, max } => T::uniform_sample(min, max, rng),
            SamplerKind::Normal(d) => T::from_f64_clamped(d.sample(rng)),
            SamplerKind::Exponential(d) => T::from_f64_clamped(d.sample(rng)),
            SamplerKind::LogNormal(d) => T::from_f64_clamped(d.sample(rng)),
            SamplerKind::Pareto(d) => T::from_f64_clamped(d.sample(rng)),
            SamplerKind::Zipf(d) => T::from_f64_clamped(d.sample(rng)),
            SamplerKind::Weighted {
                values,
                cumulative_weights,
                total,
            } => sample_weighted(values, cumulative_weights, *total, rng),
        }
    }
}

/// Pre-compute cumulative weights at construction time.
fn build_weighted<T: SampleOutput>(entries: &[WeightedValue<T>]) -> SamplerKind<T> {
    debug_assert!(
        !entries.is_empty(),
        "weighted distribution must have entries"
    );

    let mut cumulative_weights = Vec::with_capacity(entries.len());
    let mut total = 0.0;
    let mut values = Vec::with_capacity(entries.len());

    for entry in entries {
        total += entry.weight;
        cumulative_weights.push(total);
        values.push(entry.value.clone());
    }

    SamplerKind::Weighted {
        values,
        cumulative_weights,
        total,
    }
}

/// Sample from pre-computed cumulative weights via linear scan.
/// For typical use (3-10 entries) linear scan beats binary search.
fn sample_weighted<T: Clone>(
    values: &[T],
    cumulative_weights: &[f64],
    total: f64,
    rng: &mut SmallRng,
) -> T {
    let roll: f64 = rng.gen_range(0.0..total);
    for (i, &cw) in cumulative_weights.iter().enumerate() {
        if roll < cw {
            return values[i].clone();
        }
    }
    // Floating-point edge case: return last entry.
    values.last().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn fixed_always_returns_same_value() {
        let s = Sampler::new(&ValueDistribution::Fixed(42u32));
        let mut rng = SmallRng::seed_from_u64(0);
        for _ in 0..100 {
            assert_eq!(s.sample(&mut rng), 42);
        }
    }

    #[test]
    fn uniform_stays_in_range() {
        let s = Sampler::new(&ValueDistribution::Uniform {
            min: 10u32,
            max: 20,
        });
        let mut rng = SmallRng::seed_from_u64(0);
        for _ in 0..1000 {
            let v = s.sample(&mut rng);
            assert!((10..=20).contains(&v), "got {v}");
        }
    }

    #[test]
    fn normal_clamps_to_at_least_1() {
        let s: Sampler<u32> = Sampler::new(&ValueDistribution::Normal {
            mean: 1.0,
            stddev: 10.0,
        });
        let mut rng = SmallRng::seed_from_u64(0);
        for _ in 0..1000 {
            let v = s.sample(&mut rng);
            assert!(v >= 1, "got {v}");
        }
    }

    #[test]
    fn exponential_positive_and_reasonable() {
        let s: Sampler<u32> = Sampler::new(&ValueDistribution::Exponential { mean: 100.0 });
        let mut rng = SmallRng::seed_from_u64(42);
        let mut sum = 0u64;
        let n = 10_000;
        for _ in 0..n {
            let v = s.sample(&mut rng);
            assert!(v >= 1, "got {v}");
            sum += v as u64;
        }
        // Mean should be roughly 100
        let mean = sum as f64 / n as f64;
        assert!(
            (50.0..200.0).contains(&mean),
            "expected mean ~100, got {mean}"
        );
    }

    #[test]
    fn lognormal_positive() {
        let s: Sampler<u32> = Sampler::new(&ValueDistribution::LogNormal {
            mu: 5.0,
            sigma: 0.5,
        });
        let mut rng = SmallRng::seed_from_u64(42);
        for _ in 0..1000 {
            let v = s.sample(&mut rng);
            assert!(v >= 1, "got {v}");
        }
    }

    #[test]
    fn pareto_at_least_scale() {
        let s: Sampler<u32> = Sampler::new(&ValueDistribution::Pareto {
            scale: 10.0,
            shape: 2.0,
        });
        let mut rng = SmallRng::seed_from_u64(42);
        for _ in 0..1000 {
            let v = s.sample(&mut rng);
            assert!(v >= 10, "got {v}");
        }
    }

    #[test]
    fn zipf_in_range() {
        let s: Sampler<u32> = Sampler::new(&ValueDistribution::Zipf {
            n: 1000,
            exponent: 1.0,
        });
        let mut rng = SmallRng::seed_from_u64(42);
        for _ in 0..1000 {
            let v = s.sample(&mut rng);
            assert!(v >= 1 && v <= 1000, "got {v}");
        }
    }

    #[test]
    fn zipf_is_skewed() {
        let s: Sampler<u32> = Sampler::new(&ValueDistribution::Zipf {
            n: 100,
            exponent: 1.0,
        });
        let mut rng = SmallRng::seed_from_u64(42);
        let mut low_count = 0u32;
        let n = 10_000;
        for _ in 0..n {
            if s.sample(&mut rng) <= 10 {
                low_count += 1;
            }
        }
        // Top-10 out of 100 should get >50% of hits with exponent=1.0
        let ratio = low_count as f64 / n as f64;
        assert!(
            ratio > 0.4,
            "expected top-10 to get >40% of hits, got {ratio:.2}"
        );
    }

    #[test]
    fn weighted_respects_proportions() {
        let s = Sampler::new(&ValueDistribution::Weighted(vec![
            WeightedValue::new(1u32, 1.0),
            WeightedValue::new(2u32, 3.0),
        ]));
        let mut rng = SmallRng::seed_from_u64(42);
        let mut counts = [0u32; 3];
        for _ in 0..10_000 {
            let v = s.sample(&mut rng);
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
    fn u16_uniform_stays_in_range() {
        let s = Sampler::new(&ValueDistribution::Uniform {
            min: 200u16,
            max: 1500,
        });
        let mut rng = SmallRng::seed_from_u64(0);
        for _ in 0..1000 {
            let v = s.sample(&mut rng);
            assert!((200..=1500).contains(&v), "got {v}");
        }
    }

    #[test]
    fn usize_weighted_works() {
        let s = Sampler::new(&ValueDistribution::Weighted(vec![
            WeightedValue::new(200usize, 0.3),
            WeightedValue::new(1200usize, 0.5),
            WeightedValue::new(1500usize, 0.2),
        ]));
        let mut rng = SmallRng::seed_from_u64(42);
        let v = s.sample(&mut rng);
        assert!([200, 1200, 1500].contains(&v));
    }
}
