//! TCP protocol specification types.
//!
//! These live in netanvil-types (rather than netanvil-tcp) so that plugin crates
//! can implement conversion traits for `TcpRequestSpec` without depending on
//! the compio-based executor.

use std::net::SocketAddr;

use crate::request::ProtocolSpec;

/// Test mode for TCP connections.
///
/// Determines the protocol behavior on the wire: whether a protocol header is
/// sent, and how data flows between client and server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TcpTestMode {
    /// No protocol header. Send payload, optionally read response.
    /// Backward-compatible with plain echo servers.
    #[default]
    Echo,
    /// Protocol header 0x01. Fixed-size request/response per transaction.
    RR,
    /// Protocol header 0x02. Client sends chunks, server discards.
    Sink,
    /// Protocol header 0x03. Server sends chunks, client reads.
    Source,
    /// Protocol header 0x04. Both sides send/receive simultaneously.
    Bidir,
    /// Protocol header 0x05. Connect-Request-Response-Close per transaction.
    CRR,
}

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
    /// Test mode controlling protocol behavior on this connection.
    pub mode: TcpTestMode,
    /// Request payload size in bytes (used in protocol header for RR/Sink/Bidir).
    pub request_size: u16,
    /// Response payload size in bytes (used in protocol header for RR/Source/Bidir).
    pub response_size: u32,
    /// Server-side latency injection in microseconds (v2 protocol header).
    pub latency_us: Option<u32>,
    /// Server-side error injection rate per 10,000 requests (v2 protocol header).
    pub error_rate: Option<u32>,
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
