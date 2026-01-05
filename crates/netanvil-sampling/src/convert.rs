//! Trait for converting sampled `f64` values to integer output types.

use rand::rngs::SmallRng;
use rand::Rng;

/// Conversion from a sampled `f64` to the target integer type, plus
/// direct integer-range uniform sampling (avoids float intermediary for
/// the `Uniform` variant).
///
/// Built-in implementations are provided for `u16`, `u32`, and `usize`.
pub trait SampleOutput: Clone {
    /// Convert a continuous sample to this type.
    ///
    /// The value is rounded and clamped to `>= 1` and within the type's
    /// representable range.
    fn from_f64_clamped(v: f64) -> Self;

    /// Uniform integer sample in `[min, max]` (inclusive).
    fn uniform_sample(min: &Self, max: &Self, rng: &mut SmallRng) -> Self;
}

impl SampleOutput for u32 {
    #[inline]
    fn from_f64_clamped(v: f64) -> Self {
        v.round().max(1.0) as u32
    }

    #[inline]
    fn uniform_sample(min: &Self, max: &Self, rng: &mut SmallRng) -> Self {
        rng.gen_range(*min..=*max)
    }
}

impl SampleOutput for u16 {
    #[inline]
    fn from_f64_clamped(v: f64) -> Self {
        v.round().max(1.0).min(u16::MAX as f64) as u16
    }

    #[inline]
    fn uniform_sample(min: &Self, max: &Self, rng: &mut SmallRng) -> Self {
        rng.gen_range(*min..=*max)
    }
}

impl SampleOutput for usize {
    #[inline]
    fn from_f64_clamped(v: f64) -> Self {
        v.round().max(1.0) as usize
    }

    #[inline]
    fn uniform_sample(min: &Self, max: &Self, rng: &mut SmallRng) -> Self {
        rng.gen_range(*min..=*max)
    }
}
