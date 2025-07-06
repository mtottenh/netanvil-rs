use std::net::SocketAddr;

use netanvil_types::request::ProtocolSpec;

/// Protocol-specific request specification for raw TCP load testing.
#[derive(Debug, Clone)]
pub struct TcpRequestSpec {
    /// Target address to connect to.
    pub target: SocketAddr,
    /// Raw payload bytes to send.
    pub payload: Vec<u8>,
    /// Framing strategy for encoding the outgoing payload and decoding the
    /// incoming response.
    pub framing: TcpFraming,
    /// Whether to wait for a response after sending.
    pub expect_response: bool,
    /// Maximum number of bytes to read in a response.
    pub response_max_bytes: usize,
}

impl ProtocolSpec for TcpRequestSpec {}

/// Framing mode for TCP payloads.
#[derive(Debug, Clone, Default)]
pub enum TcpFraming {
    /// Send raw bytes, read until timeout or connection close.
    #[default]
    Raw,
    /// Length-prefixed: first N bytes (big-endian) encode payload length.
    LengthPrefixed { width: u8 },
    /// Read until delimiter sequence is found (e.g., `b"\r\n"` for Redis).
    Delimiter(Vec<u8>),
    /// Fixed-size messages.
    FixedSize(usize),
}
