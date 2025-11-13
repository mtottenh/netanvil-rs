//! UDP executor for the netanvil-rs load testing framework.
//!
//! This crate provides a UDP datagram executor, generator, and transformer
//! that implement the core netanvil-types traits. The executor creates a new
//! `compio::net::UdpSocket` per request, which is cheap for UDP and avoids
//! sharing issues when the executor is `Rc`-shared across concurrent tasks.
//!
//! # Example
//!
//! ```no_run
//! use std::net::SocketAddr;
//! use netanvil_udp::{UdpExecutor, SimpleUdpGenerator, UdpNoopTransformer};
//!
//! let executor = UdpExecutor::new();
//! let generator = SimpleUdpGenerator::new(
//!     vec!["127.0.0.1:9000".parse().unwrap()],
//!     b"hello".to_vec(),
//!     false, // fire-and-forget
//! );
//! let transformer = UdpNoopTransformer;
//! ```

pub mod executor;
pub mod generator;
pub mod loss_tracker;
pub mod spec;
pub mod transformer;

pub use executor::UdpExecutor;
pub use generator::SimpleUdpGenerator;
pub use loss_tracker::{LossTracker, UdpPacketSource};
pub use spec::UdpRequestSpec;
pub use transformer::UdpNoopTransformer;
