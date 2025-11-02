//! Redis protocol specification types.
//!
//! These live in netanvil-types so that plugin crates can implement conversion
//! traits for `RedisRequestSpec` without depending on the compio-based executor.

use std::net::SocketAddr;

use crate::request::ProtocolSpec;

/// A Redis command to execute.
#[derive(Debug, Clone)]
pub struct RedisRequestSpec {
    /// Target Redis server address.
    pub target: SocketAddr,
    /// Redis command (e.g., "SET", "GET", "LPUSH").
    pub command: String,
    /// Command arguments (e.g., ["key", "value"]).
    pub args: Vec<String>,
}

impl ProtocolSpec for RedisRequestSpec {}
