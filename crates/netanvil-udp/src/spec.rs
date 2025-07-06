use std::net::SocketAddr;

use netanvil_types::ProtocolSpec;

/// A UDP datagram to send.
#[derive(Debug, Clone)]
pub struct UdpRequestSpec {
    /// Target socket address.
    pub target: SocketAddr,
    /// Payload bytes to send.
    pub payload: Vec<u8>,
    /// Whether to wait for a response datagram.
    pub expect_response: bool,
    /// Maximum response datagram size in bytes.
    pub response_max_bytes: usize,
}

impl ProtocolSpec for UdpRequestSpec {}
