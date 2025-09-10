//! Serializable mirror types for crossing the plugin boundary.
//!
//! These types mirror netanvil_types::RequestContext and HttpRequestSpec but are
//! fully serializable (no Instant fields). The plugin runtimes convert
//! between these and the real types at the boundary.

use serde::{Deserialize, Serialize};

/// Serializable version of RequestContext for passing to plugins.
/// Instant fields are replaced with elapsed_ns (nanoseconds since test start).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRequestContext {
    pub request_id: u64,
    pub core_id: usize,
    pub is_sampled: bool,
    pub session_id: Option<u64>,
}

/// Serializable version of HttpRequestSpec returned by plugins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHttpRequestSpec {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl PluginHttpRequestSpec {
    /// Convert to the real HttpRequestSpec type.
    pub fn into_http_request_spec(self) -> netanvil_types::HttpRequestSpec {
        let method = match self.method.to_uppercase().as_str() {
            "GET" => http::Method::GET,
            "POST" => http::Method::POST,
            "PUT" => http::Method::PUT,
            "DELETE" => http::Method::DELETE,
            "PATCH" => http::Method::PATCH,
            "HEAD" => http::Method::HEAD,
            "OPTIONS" => http::Method::OPTIONS,
            other => http::Method::from_bytes(other.as_bytes()).unwrap_or(http::Method::GET),
        };
        netanvil_types::HttpRequestSpec {
            method,
            url: self.url,
            headers: self.headers,
            body: self.body,
        }
    }
}

impl From<&netanvil_types::RequestContext> for PluginRequestContext {
    fn from(ctx: &netanvil_types::RequestContext) -> Self {
        Self {
            request_id: ctx.request_id,
            core_id: ctx.core_id,
            is_sampled: ctx.is_sampled,
            session_id: ctx.session_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Binary protocol types for zero-copy plugin communication.
// ---------------------------------------------------------------------------

/// Fixed-layout context for zero-copy passing to WASM plugin runtimes.
///
/// Layout is deterministic on all targets (`repr(C)`, explicit padding).
/// The host writes this directly into WASM linear memory; the guest reads
/// it via pointer cast. No serialization overhead.
///
/// Flags byte: bit 0 = `is_sampled`, bit 1 = `has_session_id`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawContext {
    pub request_id: u64, // offset 0
    pub core_id: u32,    // offset 8
    pub flags: u8,       // offset 12
    pub _pad: [u8; 3],   // offset 13 — align session_id
    pub session_id: u64, // offset 16
}
// Total: 24 bytes

const _: () = assert!(std::mem::size_of::<RawContext>() == 24);

impl RawContext {
    pub fn from_context(ctx: &netanvil_types::RequestContext) -> Self {
        let mut flags = 0u8;
        if ctx.is_sampled {
            flags |= 0x01;
        }
        let session_id = match ctx.session_id {
            Some(id) => {
                flags |= 0x02;
                id
            }
            None => 0,
        };
        Self {
            request_id: ctx.request_id,
            core_id: ctx.core_id as u32,
            flags,
            _pad: [0; 3],
            session_id,
        }
    }

    /// View as raw bytes for direct memory copy.
    pub fn as_bytes(&self) -> &[u8; 24] {
        // Safety: RawContext is repr(C) with no uninitialized padding
        // (_pad is explicitly zeroed in from_context).
        unsafe { &*(self as *const Self as *const [u8; 24]) }
    }
}

// ---------------------------------------------------------------------------
// Protocol-generic plugin output conversion.
// ---------------------------------------------------------------------------

/// Construct a protocol-specific spec from postcard-encoded bytes (WASM output).
///
/// Each protocol implements this for its spec type. The WASM generator calls
/// `S::from_postcard_bytes()` instead of hardcoding `PluginHttpRequestSpec`.
pub trait FromPostcard: netanvil_types::ProtocolSpec + Sized {
    fn from_postcard_bytes(bytes: &[u8]) -> std::result::Result<Self, crate::error::PluginError>;
    fn fallback() -> Self;
}

/// Serializable version of TcpRequestSpec returned by plugins.
///
/// Plugins only control the payload — target, framing, and mode come from
/// test configuration and are filled in by the generator or transformer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginTcpSpec {
    /// Payload bytes or UTF-8 string to send.
    #[serde(default)]
    pub payload: Vec<u8>,
    /// Payload as a UTF-8 string (alternative to raw bytes).
    /// If both `payload` and `payload_str` are set, `payload_str` takes priority.
    #[serde(default)]
    pub payload_str: Option<String>,
}

impl FromPostcard for netanvil_types::TcpRequestSpec {
    fn from_postcard_bytes(bytes: &[u8]) -> std::result::Result<Self, crate::error::PluginError> {
        let plugin_spec: PluginTcpSpec = postcard::from_bytes(bytes)
            .map_err(|e| crate::error::PluginError::InvalidResponse(format!("postcard: {e}")))?;

        let payload = match plugin_spec.payload_str {
            Some(s) => s.into_bytes(),
            None => plugin_spec.payload,
        };

        Ok(netanvil_types::TcpRequestSpec {
            target: "0.0.0.0:0".parse().unwrap(),
            payload,
            framing: netanvil_types::TcpFraming::Raw,
            expect_response: true,
            response_max_bytes: 65536,
            mode: netanvil_types::TcpTestMode::Echo,
            request_size: 0,
            response_size: 0,
        })
    }

    fn fallback() -> Self {
        netanvil_types::TcpRequestSpec {
            target: "127.0.0.1:0".parse().unwrap(),
            payload: vec![],
            framing: netanvil_types::TcpFraming::Raw,
            expect_response: true,
            response_max_bytes: 65536,
            mode: netanvil_types::TcpTestMode::Echo,
            request_size: 0,
            response_size: 0,
        }
    }
}

/// Serializable DNS query spec returned by plugins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDnsSpec {
    pub query_name: String,
    #[serde(default = "default_query_type")]
    pub query_type: String,
    #[serde(default = "default_true")]
    pub recursion: bool,
    #[serde(default)]
    pub dnssec: bool,
}

fn default_query_type() -> String {
    "A".into()
}

fn default_true() -> bool {
    true
}

impl FromPostcard for netanvil_types::DnsRequestSpec {
    fn from_postcard_bytes(bytes: &[u8]) -> std::result::Result<Self, crate::error::PluginError> {
        let plugin: PluginDnsSpec = postcard::from_bytes(bytes)
            .map_err(|e| crate::error::PluginError::InvalidResponse(format!("postcard: {e}")))?;

        let query_type = netanvil_types::DnsQueryType::from_str_name(&plugin.query_type)
            .unwrap_or(netanvil_types::DnsQueryType::A);

        Ok(netanvil_types::DnsRequestSpec {
            server: "0.0.0.0:0".parse().unwrap(),
            query_name: plugin.query_name,
            query_type,
            recursion: plugin.recursion,
            dnssec: plugin.dnssec,
            txid: 0,
        })
    }

    fn fallback() -> Self {
        netanvil_types::DnsRequestSpec {
            server: "127.0.0.1:53".parse().unwrap(),
            query_name: "error.invalid".into(),
            query_type: netanvil_types::DnsQueryType::A,
            recursion: true,
            dnssec: false,
            txid: 0,
        }
    }
}

impl FromPostcard for netanvil_types::HttpRequestSpec {
    fn from_postcard_bytes(bytes: &[u8]) -> std::result::Result<Self, crate::error::PluginError> {
        let plugin_spec: PluginHttpRequestSpec = postcard::from_bytes(bytes)
            .map_err(|e| crate::error::PluginError::InvalidResponse(format!("postcard: {e}")))?;
        Ok(plugin_spec.into_http_request_spec())
    }

    fn fallback() -> Self {
        netanvil_types::HttpRequestSpec {
            method: http::Method::GET,
            url: "http://error.invalid".into(),
            headers: vec![],
            body: None,
        }
    }
}
