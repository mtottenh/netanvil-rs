//! Compio-based test servers for netanvil-rs integration testing.
//!
//! Provides embeddable TCP and UDP echo servers that use the same
//! thread-per-core, io_uring architecture as the load generator.

pub mod tcp;
pub mod udp;
