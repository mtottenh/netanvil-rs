//! Minimal DNS wire format encoder/decoder (RFC 1035).
//!
//! Just enough to build valid queries and parse response RCODEs.
//! Full RFC compliance is not needed for a load generator.

use netanvil_types::dns_spec::{DnsQueryType, DnsRequestSpec};

/// Encode a DNS query into wire format.
pub fn encode_query(spec: &DnsRequestSpec) -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);

    // Header (12 bytes)
    let txid = if spec.txid != 0 {
        spec.txid
    } else {
        rand::random()
    };
    buf.extend_from_slice(&txid.to_be_bytes()); // ID
    let flags: u16 = if spec.recursion { 0x0100 } else { 0x0000 }; // RD bit
    buf.extend_from_slice(&flags.to_be_bytes()); // Flags
    buf.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT = 1
    buf.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT = 0
    buf.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT = 0

    // ARCOUNT: 1 if DNSSEC (for EDNS OPT record), else 0
    let arcount: u16 = if spec.dnssec { 1 } else { 0 };
    buf.extend_from_slice(&arcount.to_be_bytes());

    // Question section
    encode_domain_name(&mut buf, &spec.query_name);
    buf.extend_from_slice(&spec.query_type.wire_value().to_be_bytes()); // QTYPE
    buf.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN

    // EDNS OPT record for DNSSEC
    if spec.dnssec {
        buf.push(0); // NAME = root
        buf.extend_from_slice(&41u16.to_be_bytes()); // TYPE = OPT
        buf.extend_from_slice(&4096u16.to_be_bytes()); // UDP payload size
        buf.push(0); // Extended RCODE
        buf.push(0); // EDNS version
        buf.extend_from_slice(&0x8000u16.to_be_bytes()); // DO bit set
        buf.extend_from_slice(&0u16.to_be_bytes()); // RDLENGTH = 0
    }

    buf
}

/// Parse RCODE from a DNS response header.
///
/// Returns the 4-bit RCODE (0=NOERROR, 2=SERVFAIL, 3=NXDOMAIN, etc.)
/// or 0xFF if the response is too short.
pub fn parse_rcode(response: &[u8]) -> u8 {
    if response.len() < 4 {
        return 0xFF;
    }
    response[3] & 0x0F
}

/// Parse the transaction ID from a DNS response.
pub fn parse_txid(response: &[u8]) -> Option<u16> {
    if response.len() < 2 {
        return None;
    }
    Some(u16::from_be_bytes([response[0], response[1]]))
}

/// Encode a domain name in DNS wire format (length-prefixed labels).
fn encode_domain_name(buf: &mut Vec<u8>, name: &str) {
    for label in name.split('.') {
        if label.is_empty() {
            continue;
        }
        buf.push(label.len() as u8);
        buf.extend_from_slice(label.as_bytes());
    }
    buf.push(0); // root label terminator
}

/// Parse a query type from its string name.
pub fn parse_query_type(s: &str) -> Option<DnsQueryType> {
    DnsQueryType::from_str_name(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_domain_name() {
        let mut buf = Vec::new();
        encode_domain_name(&mut buf, "example.com");
        // 7 "example" 3 "com" 0
        assert_eq!(
            buf,
            vec![7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 3, b'c', b'o', b'm', 0]
        );
    }

    #[test]
    fn test_encode_query_basic() {
        let spec = DnsRequestSpec {
            server: "8.8.8.8:53".parse().unwrap(),
            query_name: "example.com".to_string(),
            query_type: DnsQueryType::A,
            recursion: true,
            dnssec: false,
            txid: 0x1234,
        };
        let buf = encode_query(&spec);

        // Check header
        assert_eq!(buf[0..2], [0x12, 0x34]); // Transaction ID
        assert_eq!(buf[2..4], [0x01, 0x00]); // Flags (RD=1)
        assert_eq!(buf[4..6], [0x00, 0x01]); // QDCOUNT = 1
        assert_eq!(buf[6..8], [0x00, 0x00]); // ANCOUNT = 0

        // Check question section starts after 12-byte header
        assert_eq!(buf[12], 7); // "example" label length
    }

    #[test]
    fn test_parse_rcode() {
        // NOERROR response (RCODE = 0)
        let response = [0x12, 0x34, 0x81, 0x80]; // standard response, no error
        assert_eq!(parse_rcode(&response), 0);

        // NXDOMAIN response (RCODE = 3)
        let response = [0x12, 0x34, 0x81, 0x83];
        assert_eq!(parse_rcode(&response), 3);

        // Too short
        assert_eq!(parse_rcode(&[0x12]), 0xFF);
    }

    #[test]
    fn test_parse_txid() {
        assert_eq!(parse_txid(&[0x12, 0x34, 0x00, 0x00]), Some(0x1234));
        assert_eq!(parse_txid(&[0x00]), None);
    }
}
