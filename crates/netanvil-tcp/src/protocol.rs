//! Test server protocol header encode/decode.
//!
//! Every non-Echo connection begins with an 8-byte header sent by the client:
//!
//! ```text
//! Offset  Type    Field           Description
//! 0       u8      mode            0x01=RR, 0x02=SINK, 0x03=SOURCE, 0x04=BIDIR
//! 1       u8      reserved        0x00
//! 2..4    u16 BE  request_size    Client payload size per operation
//! 4..8    u32 BE  response_size   Server payload size per operation
//! ```

use crate::spec::{TcpRequestSpec, TcpTestMode};

/// Size of the protocol header in bytes.
pub const HEADER_SIZE: usize = 8;

/// Mode byte for request-response.
pub const MODE_RR: u8 = 0x01;
/// Mode byte for client-to-server sink.
pub const MODE_SINK: u8 = 0x02;
/// Mode byte for server-to-client source.
pub const MODE_SOURCE: u8 = 0x03;
/// Mode byte for bidirectional.
pub const MODE_BIDIR: u8 = 0x04;

/// Encode an 8-byte protocol header from the spec.
///
/// # Panics
///
/// Panics if called with `TcpTestMode::Echo` (Echo mode does not use a
/// protocol header).
pub fn encode_header(spec: &TcpRequestSpec) -> Vec<u8> {
    let mode_byte = match spec.mode {
        TcpTestMode::RR => MODE_RR,
        TcpTestMode::Sink => MODE_SINK,
        TcpTestMode::Source => MODE_SOURCE,
        TcpTestMode::Bidir => MODE_BIDIR,
        TcpTestMode::Echo => panic!("Echo mode does not use protocol header"),
    };
    let mut header = vec![0u8; HEADER_SIZE];
    header[0] = mode_byte;
    header[1] = 0; // reserved
    header[2..4].copy_from_slice(&spec.request_size.to_be_bytes());
    header[4..8].copy_from_slice(&spec.response_size.to_be_bytes());
    header
}

/// Parse an 8-byte protocol header.
///
/// Returns `(mode_byte, request_size, response_size)` if the first byte is a
/// recognised mode, or `None` for unknown/invalid headers.
pub fn parse_header(buf: &[u8; HEADER_SIZE]) -> Option<(u8, u16, u32)> {
    let mode = buf[0];
    if !matches!(mode, MODE_RR | MODE_SINK | MODE_SOURCE | MODE_BIDIR) {
        return None;
    }
    let request_size = u16::from_be_bytes([buf[2], buf[3]]);
    let response_size = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    Some((mode, request_size, response_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_rr_header() {
        let spec = TcpRequestSpec {
            target: "127.0.0.1:9000".parse().unwrap(),
            payload: vec![],
            framing: crate::spec::TcpFraming::Raw,
            expect_response: true,
            response_max_bytes: 0,
            mode: TcpTestMode::RR,
            request_size: 64,
            response_size: 1024,
        };
        let header = encode_header(&spec);
        assert_eq!(header.len(), HEADER_SIZE);

        let buf: [u8; HEADER_SIZE] = header.try_into().unwrap();
        let (mode, req_sz, resp_sz) = parse_header(&buf).unwrap();
        assert_eq!(mode, MODE_RR);
        assert_eq!(req_sz, 64);
        assert_eq!(resp_sz, 1024);
    }

    #[test]
    fn roundtrip_sink_header() {
        let spec = TcpRequestSpec {
            target: "127.0.0.1:9000".parse().unwrap(),
            payload: vec![],
            framing: crate::spec::TcpFraming::Raw,
            expect_response: false,
            response_max_bytes: 0,
            mode: TcpTestMode::Sink,
            request_size: 512,
            response_size: 0,
        };
        let header = encode_header(&spec);
        let buf: [u8; HEADER_SIZE] = header.try_into().unwrap();
        let (mode, req_sz, resp_sz) = parse_header(&buf).unwrap();
        assert_eq!(mode, MODE_SINK);
        assert_eq!(req_sz, 512);
        assert_eq!(resp_sz, 0);
    }

    #[test]
    fn parse_invalid_mode_returns_none() {
        let buf = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert!(parse_header(&buf).is_none());

        let buf = [0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert!(parse_header(&buf).is_none());
    }

    #[test]
    #[should_panic(expected = "Echo mode does not use protocol header")]
    fn encode_echo_mode_panics() {
        let spec = TcpRequestSpec {
            target: "127.0.0.1:9000".parse().unwrap(),
            payload: vec![],
            framing: crate::spec::TcpFraming::Raw,
            expect_response: false,
            response_max_bytes: 0,
            mode: TcpTestMode::Echo,
            request_size: 0,
            response_size: 0,
        };
        encode_header(&spec);
    }
}
