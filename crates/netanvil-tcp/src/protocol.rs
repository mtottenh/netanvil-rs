//! Test server protocol header encode/decode (v2 format).
//!
//! Every non-Echo connection begins with a variable-length v2 header:
//!
//! ```text
//! Offset  Type    Field           Description
//! 0       u8      version         0xF0 (sentinel)
//! 1       u8      header_len      total header length (min 10)
//! 2       u8      mode            0x01=RR, 0x02=SINK, 0x03=SOURCE, 0x04=BIDIR, 0x05=CRR
//! 3       u8      flags           bit 1: latency injection, bit 2: error injection
//! 4..6    u16 BE  request_size    Client payload size per operation
//! 6..10   u32 BE  response_size   Server payload size per operation
//! 10..14  u32 BE  latency_us      (optional, if flag bit 1)
//! 14..18  u32 BE  error_rate      (optional, if flag bit 2, per 10000)
//! ```

use crate::spec::{TcpRequestSpec, TcpTestMode};

/// Mode byte for request-response.
pub const MODE_RR: u8 = 0x01;
/// Mode byte for client-to-server sink.
pub const MODE_SINK: u8 = 0x02;
/// Mode byte for server-to-client source.
pub const MODE_SOURCE: u8 = 0x03;
/// Mode byte for bidirectional.
pub const MODE_BIDIR: u8 = 0x04;
/// Mode byte for connect-request-response-close.
pub const MODE_CRR: u8 = 0x05;

/// Protocol header sentinel byte.
pub const VERSION_V2: u8 = 0xF0;

/// Flag: latency injection enabled (bit 1).
const FLAG_LATENCY: u8 = 0x02;
/// Flag: error injection enabled (bit 2).
const FLAG_ERROR: u8 = 0x04;

/// Minimum header size: version(1) + len(1) + mode(1) + flags(1) + req_size(2) + resp_size(4) = 10.
const HEADER_MIN: usize = 10;

/// Encode a protocol header from the spec.
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
        TcpTestMode::CRR => MODE_CRR,
        TcpTestMode::Echo => panic!("Echo mode does not use protocol header"),
    };

    let mut flags: u8 = 0;
    if spec.latency_us.is_some() {
        flags |= FLAG_LATENCY;
    }
    if spec.error_rate.is_some() {
        flags |= FLAG_ERROR;
    }

    let header_len = HEADER_MIN
        + if spec.latency_us.is_some() { 4 } else { 0 }
        + if spec.error_rate.is_some() { 4 } else { 0 };

    let mut header = Vec::with_capacity(header_len);
    header.push(VERSION_V2);
    header.push(header_len as u8);
    header.push(mode_byte);
    header.push(flags);
    header.extend_from_slice(&spec.request_size.to_be_bytes());
    header.extend_from_slice(&spec.response_size.to_be_bytes());

    if let Some(us) = spec.latency_us {
        header.extend_from_slice(&us.to_be_bytes());
    }
    if let Some(rate) = spec.error_rate {
        header.extend_from_slice(&rate.to_be_bytes());
    }

    header
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_rr_header() {
        let spec = TcpRequestSpec {
            target: "127.0.0.1:9000".parse().unwrap(),
            payload: vec![],
            framing: crate::spec::TcpFraming::Raw,
            expect_response: true,
            response_max_bytes: 0,
            mode: TcpTestMode::RR,
            request_size: 64,
            response_size: 1024,
            latency_us: None,
            error_rate: None,
        };
        let header = encode_header(&spec);
        assert_eq!(header.len(), HEADER_MIN);
        assert_eq!(header[0], VERSION_V2);
        assert_eq!(header[1], HEADER_MIN as u8);
        assert_eq!(header[2], MODE_RR);
        assert_eq!(header[3], 0); // no flags

        let req_size = u16::from_be_bytes([header[4], header[5]]);
        let resp_size = u32::from_be_bytes([header[6], header[7], header[8], header[9]]);
        assert_eq!(req_size, 64);
        assert_eq!(resp_size, 1024);
    }

    #[test]
    fn encode_with_latency_injection() {
        let spec = TcpRequestSpec {
            target: "127.0.0.1:9000".parse().unwrap(),
            payload: vec![],
            framing: crate::spec::TcpFraming::Raw,
            expect_response: true,
            response_max_bytes: 0,
            mode: TcpTestMode::RR,
            request_size: 64,
            response_size: 256,
            latency_us: Some(5000),
            error_rate: None,
        };
        let header = encode_header(&spec);
        assert_eq!(header.len(), HEADER_MIN + 4);
        assert_eq!(header[3], FLAG_LATENCY);
        let latency = u32::from_be_bytes([header[10], header[11], header[12], header[13]]);
        assert_eq!(latency, 5000);
    }

    #[test]
    fn encode_with_both_injections() {
        let spec = TcpRequestSpec {
            target: "127.0.0.1:9000".parse().unwrap(),
            payload: vec![],
            framing: crate::spec::TcpFraming::Raw,
            expect_response: true,
            response_max_bytes: 0,
            mode: TcpTestMode::CRR,
            request_size: 64,
            response_size: 128,
            latency_us: Some(1000),
            error_rate: Some(500),
        };
        let header = encode_header(&spec);
        assert_eq!(header.len(), HEADER_MIN + 8);
        assert_eq!(header[2], MODE_CRR);
        assert_eq!(header[3], FLAG_LATENCY | FLAG_ERROR);
        let latency = u32::from_be_bytes([header[10], header[11], header[12], header[13]]);
        let error_rate = u32::from_be_bytes([header[14], header[15], header[16], header[17]]);
        assert_eq!(latency, 1000);
        assert_eq!(error_rate, 500);
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
            latency_us: None,
            error_rate: None,
        };
        encode_header(&spec);
    }
}
