//! Rate controller implementations.
//!
//! The primary controller is the [`Arbiter`], which holds N constraints and
//! composes them via a [`composition::CompositionStrategy`] (default:
//! min-selector — lowest desired rate wins). After composition, arbiter-level
//! policies are applied: progressive ceiling, known-good floor, post-backoff
//! cooldown, and rate-of-change limits.
//!
//! ## Constraint types
//!
//! - [`threshold::ThresholdConstraint`] — AIMD control law. Compares a
//!   smoothed metric against a threshold, produces graduated hold/backoff
//!   actions based on severity.
//! - [`pid_constraint::PidConstraint`] — Setpoint PID in log-rate space.
//!   Drives a metric toward a target value with back-calculation tracking
//!   for bumpless transfer when non-binding.
//!
//! ## Entry point
//!
//! [`builder::build_rate_controller`] maps [`netanvil_types::RateConfig`] to
//! a `Box<dyn RateController>`. `Static` and `Step` are constructed directly;
//! `Adaptive` builds constraints and wraps them in an `Arbiter`.

pub(crate) mod arbiter;
pub mod autotune;
pub mod builder;
mod ceiling;
pub mod clock;
pub mod composition;
pub mod constraint;
pub(crate) mod constraints;
pub mod pid_constraint;
pub mod pid_math;
pub mod smoothing;
mod static_rate;
mod step_rate;
pub mod threshold;
pub mod trace;

pub use arbiter::{
    Arbiter, ArbiterConfig, CongestionAvoidanceConfig, IncreasePolicyConfig, RateChangeLimits,
};
pub use static_rate::StaticRateController;
pub use step_rate::StepRateController;
