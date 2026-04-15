//! Compio-based test servers for netanvil-rs integration testing.
//!
//! Provides embeddable TCP, UDP, and DNS echo servers that use the same
//! thread-per-core, io_uring architecture as the load generator.

pub mod dns;
pub mod protocol;
pub mod tcp;
pub mod udp;

/// Configuration for test servers.
///
/// Provides socket tuning and connection management parameters with sensible
/// defaults. Use `Default::default()` for standard values, or customize
/// individual fields as needed.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Listen address (e.g. "127.0.0.1:0" for random port).
    pub addr: String,
    /// Maximum simultaneous TCP connections (0 = unlimited).
    pub max_connections: usize,
    /// SO_RCVBUF size in bytes.
    pub recv_buf_size: usize,
    /// SO_SNDBUF size in bytes.
    pub send_buf_size: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:0".to_string(),
            max_connections: 10_000,
            recv_buf_size: 256 * 1024, // 256 KiB
            send_buf_size: 256 * 1024, // 256 KiB
        }
    }
}

impl ServerConfig {
    /// Create a UDP/DNS-tuned config with 4 MiB buffers.
    pub fn udp_default() -> Self {
        Self {
            recv_buf_size: 4 * 1024 * 1024, // 4 MiB
            send_buf_size: 4 * 1024 * 1024, // 4 MiB
            ..Default::default()
        }
    }
}
