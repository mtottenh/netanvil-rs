//! Redis RESP protocol executor for the netanvil-rs load testing framework.
//!
//! Provides a TCP-based Redis executor with connection pooling, a simple
//! round-robin generator, and RESP wire format encoding/decoding.
//!
//! The [`RedisRequestSpec`] type lives in `netanvil-types` so that plugin
//! crates can implement conversion traits without depending on compio.
//!
//! # Modules
//!
//! - [`resp`] -- RESP wire format encoder/decoder.
//! - [`executor`] -- [`RedisExecutor`] implementing [`RequestExecutor`].
//! - [`generator`] -- [`SimpleRedisGenerator`] implementing [`RequestGenerator`].
//! - [`transformer`] -- [`RedisNoopTransformer`] implementing [`RequestTransformer`].

pub mod executor;
pub mod generator;
pub mod resp;
pub mod transformer;

pub use executor::RedisExecutor;
pub use generator::SimpleRedisGenerator;
pub use netanvil_types::redis_spec::RedisRequestSpec;
pub use resp::{encode_command, read_response, RespValue};
pub use transformer::RedisNoopTransformer;
