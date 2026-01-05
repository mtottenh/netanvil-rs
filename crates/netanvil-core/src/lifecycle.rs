//! Distribution sampling and lifecycle counting.
//!
//! This module re-exports from [`netanvil_sampling`], which is the single
//! source of truth for distribution sampling. Prior to extraction, this
//! module contained the `SampleDistribution` trait and sampling
//! implementations; those are now replaced by [`Sampler<T>`].

pub use netanvil_sampling::{LifecycleCounter, SampleOutput, Sampler};
