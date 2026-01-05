//! Pre-compiled distribution sampling for netanvil-rs.
//!
//! [`ValueDistribution<T>`](netanvil_types::distribution::ValueDistribution) in
//! `netanvil-types` is pure serializable config data with no runtime
//! dependencies. This crate provides the runtime counterpart:
//!
//! - [`Sampler<T>`] — constructed once from a `ValueDistribution<T>`,
//!   pre-builds the `rand_distr` objects so repeated `.sample()` calls pay
//!   only the RNG cost, not parameter validation and object construction.
//! - [`LifecycleCounter`] — stateful counter that fires when a
//!   distribution-sampled limit is reached. Used for connection lifetime
//!   management and similar event-counting patterns.
//! - [`SampleOutput`] — trait for converting sampled `f64` values to the
//!   target integer type with appropriate clamping.

mod convert;
mod lifecycle;
mod sampler;

pub use convert::SampleOutput;
pub use lifecycle::LifecycleCounter;
pub use sampler::Sampler;
