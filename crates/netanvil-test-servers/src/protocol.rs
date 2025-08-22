//! Test server wire protocol.
//!
//! Every TCP connection begins with an 8-byte header from the client:
//!   [0]    mode:           0x01=RR, 0x02=SINK, 0x03=SOURCE, 0x04=BIDIR
//!   [1]    flags:          reserved (0)
//!   [2..4] request_size:   u16 BE — client payload per operation
//!   [4..8] response_size:  u32 BE — server payload per operation

/// Size of the protocol header in bytes.
pub const HEADER_SIZE: usize = 8;

/// Request-response mode: read request_size bytes, write response_size bytes, repeat.
pub const MODE_RR: u8 = 0x01;

/// Sink mode: server reads and discards all incoming data.
pub const MODE_SINK: u8 = 0x02;

/// Source mode: server writes response_size-byte chunks continuously.
pub const MODE_SOURCE: u8 = 0x03;

/// Bidirectional mode: concurrent read and write.
pub const MODE_BIDIR: u8 = 0x04;

/// Parsed protocol header from a client connection.
pub struct ProtocolHeader {
    pub mode: u8,
    pub request_size: u16,
    pub response_size: u32,
}

/// Try to parse an 8-byte buffer as a protocol header.
///
/// Returns `None` if the buffer is too small or the mode byte is not a valid
/// protocol mode (0x01..=0x04). This allows backward-compatible fallback to
/// echo mode when a client sends non-protocol data.
pub fn parse_header(buf: &[u8]) -> Option<ProtocolHeader> {
    if buf.len() < HEADER_SIZE {
        return None;
    }
    let mode = buf[0];
    if !matches!(mode, MODE_RR | MODE_SINK | MODE_SOURCE | MODE_BIDIR) {
        return None;
    }
    let request_size = u16::from_be_bytes([buf[2], buf[3]]);
    let response_size = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    Some(ProtocolHeader {
        mode,
        request_size,
        response_size,
    })
}
