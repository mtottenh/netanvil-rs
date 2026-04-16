//! Test server wire protocol (v2 only).
//!
//! Variable-length header (min 10 bytes):
//!   [0]    version:        0xF0 (sentinel — distinguishes from non-protocol data)
//!   [1]    header_len:     u8 (total header length, min 10)
//!   [2]    mode:           0x01=RR, 0x02=SINK, 0x03=SOURCE, 0x04=BIDIR, 0x05=CRR
//!   [3]    flags:          u8 bitfield (bit 1: latency injection, bit 2: error injection)
//!   [4..6] request_size:   u16 BE — client payload per operation
//!   [6..10] response_size: u32 BE — server payload per operation
//!   [10..14] latency_us:   u32 BE (if flag bit 1)
//!   [14..18] error_rate:   u32 BE (if flag bit 2, errors per 10000)
//!
//! If the first byte is not the v2 sentinel, the server falls back to echo mode.

/// Request-response mode: read request_size bytes, write response_size bytes, repeat.
pub const MODE_RR: u8 = 0x01;

/// Sink mode: server reads and discards all incoming data.
pub const MODE_SINK: u8 = 0x02;

/// Source mode: server writes response_size-byte chunks continuously.
pub const MODE_SOURCE: u8 = 0x03;

/// Bidirectional mode: concurrent read and write.
pub const MODE_BIDIR: u8 = 0x04;

/// CRR mode: connect-request-response-close (one transaction per connection).
pub const MODE_CRR: u8 = 0x05;

/// Protocol header sentinel byte.
pub const VERSION_V2: u8 = 0xF0;

/// Flag: latency injection enabled (bit 1).
pub const FLAG_LATENCY: u8 = 0x02;

/// Flag: error injection enabled (bit 2).
pub const FLAG_ERROR: u8 = 0x04;

/// Minimum header size: version(1) + len(1) + mode(1) + flags(1) + req_size(2) + resp_size(4) = 10.
pub const HEADER_MIN: usize = 10;

/// Parsed protocol header.
pub struct ParsedHeader {
    pub mode: u8,
    pub request_size: u16,
    pub response_size: u32,
    pub latency_us: Option<u32>,
    pub error_rate: Option<u32>,
}

/// Parse a protocol header from a buffer.
///
/// The buffer must start with the `VERSION_V2` sentinel and be at least
/// `header_len` bytes (as specified in byte[1]).
pub fn parse_header(buf: &[u8]) -> Option<ParsedHeader> {
    if buf.len() < HEADER_MIN {
        return None;
    }
    if buf[0] < VERSION_V2 {
        return None;
    }
    let header_len = buf[1] as usize;
    if header_len < HEADER_MIN || buf.len() < header_len {
        return None;
    }

    let mode = buf[2];
    if !matches!(
        mode,
        MODE_RR | MODE_SINK | MODE_SOURCE | MODE_BIDIR | MODE_CRR
    ) {
        return None;
    }

    let flags = buf[3];
    let request_size = u16::from_be_bytes([buf[4], buf[5]]);
    let response_size = u32::from_be_bytes([buf[6], buf[7], buf[8], buf[9]]);

    let mut offset = HEADER_MIN;

    let latency_us = if flags & FLAG_LATENCY != 0 {
        if offset + 4 > header_len {
            return None;
        }
        let val = u32::from_be_bytes([
            buf[offset],
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
        ]);
        offset += 4;
        Some(val)
    } else {
        None
    };

    let error_rate = if flags & FLAG_ERROR != 0 {
        if offset + 4 > header_len {
            return None;
        }
        let val = u32::from_be_bytes([
            buf[offset],
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
        ]);
        Some(val)
    } else {
        None
    };

    Some(ParsedHeader {
        mode,
        request_size,
        response_size,
        latency_us,
        error_rate,
    })
}

/// Returns true if `first_byte` could be the start of a protocol header.
pub fn is_protocol_sentinel(first_byte: u8) -> bool {
    first_byte >= VERSION_V2
}
