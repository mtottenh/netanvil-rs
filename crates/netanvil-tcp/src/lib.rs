//! TCP request-response executor for the netanvil-rs load testing framework.
//!
//! Provides a [`TcpExecutor`] that connects to a TCP target, sends a framed
//! payload, and reads the response according to the configured [`TcpFraming`]
//! strategy (raw, length-prefixed, delimiter, or fixed-size).
//!
//! # Modules
//!
//! - [`spec`] -- [`TcpRequestSpec`] and [`TcpFraming`] types.
//! - [`framing`] -- Encode/decode helpers for each framing mode.
//! - [`executor`] -- [`TcpExecutor`] implementing [`RequestExecutor`].
//! - [`generator`] -- [`SimpleTcpGenerator`] implementing [`RequestGenerator`].
//! - [`transformer`] -- [`TcpNoopTransformer`] implementing [`RequestTransformer`].

pub mod executor;
pub mod framing;
pub mod generator;
pub mod spec;
pub mod transformer;

pub use executor::TcpExecutor;
pub use framing::{encode_frame, read_framed};
pub use generator::SimpleTcpGenerator;
pub use spec::{TcpFraming, TcpRequestSpec};
pub use transformer::TcpNoopTransformer;
