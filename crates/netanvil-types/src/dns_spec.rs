//! DNS protocol specification types.
//!
//! These live in netanvil-types so that plugin crates can implement conversion
//! traits for `DnsRequestSpec` without depending on the executor.

use std::net::SocketAddr;

use crate::request::ProtocolSpec;

/// Protocol-specific request specification for DNS load testing.
#[derive(Debug, Clone)]
pub struct DnsRequestSpec {
    /// Target DNS server address.
    pub server: SocketAddr,
    /// Domain name to query.
    pub query_name: String,
    /// Query type (A, AAAA, MX, etc.).
    pub query_type: DnsQueryType,
    /// Whether to request recursion (RD flag).
    pub recursion: bool,
    /// Whether to set the DNSSEC OK (DO) flag.
    pub dnssec: bool,
    /// Transaction ID (auto-generated if 0).
    pub txid: u16,
}

impl ProtocolSpec for DnsRequestSpec {}

/// DNS query record types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DnsQueryType {
    #[default]
    A, // 1
    AAAA,  // 28
    MX,    // 15
    CNAME, // 5
    TXT,   // 16
    NS,    // 2
    SOA,   // 6
    PTR,   // 12
    SRV,   // 33
    ANY,   // 255
}

impl DnsQueryType {
    /// Return the DNS wire format value for this query type.
    pub fn wire_value(self) -> u16 {
        match self {
            Self::A => 1,
            Self::AAAA => 28,
            Self::MX => 15,
            Self::CNAME => 5,
            Self::TXT => 16,
            Self::NS => 2,
            Self::SOA => 6,
            Self::PTR => 12,
            Self::SRV => 33,
            Self::ANY => 255,
        }
    }

    /// Parse from a string like "A", "AAAA", "MX", etc.
    pub fn from_str_name(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "A" => Some(Self::A),
            "AAAA" => Some(Self::AAAA),
            "MX" => Some(Self::MX),
            "CNAME" => Some(Self::CNAME),
            "TXT" => Some(Self::TXT),
            "NS" => Some(Self::NS),
            "SOA" => Some(Self::SOA),
            "PTR" => Some(Self::PTR),
            "SRV" => Some(Self::SRV),
            "ANY" => Some(Self::ANY),
            _ => None,
        }
    }
}
