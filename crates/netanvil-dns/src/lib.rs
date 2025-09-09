//! DNS protocol executor for the netanvil-rs load testing framework.
//!
//! Provides a UDP-based DNS executor, a simple round-robin generator,
//! and minimal DNS wire format encoding/decoding.
//!
//! The [`DnsRequestSpec`] type lives in `netanvil-types` so that plugin
//! crates can implement conversion traits without depending on compio.

pub mod executor;
pub mod generator;
pub mod transformer;
pub mod wire;

pub use executor::DnsExecutor;
pub use generator::SimpleDnsGenerator;
pub use netanvil_types::dns_spec::{DnsQueryType, DnsRequestSpec};
pub use transformer::DnsNoopTransformer;
