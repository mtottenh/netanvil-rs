//! Serializable mirror types for crossing the plugin boundary.
//!
//! These types mirror netanvil_types::RequestContext and RequestSpec but are
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

/// Serializable version of RequestSpec returned by plugins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRequestSpec {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl PluginRequestSpec {
    /// Convert to the real RequestSpec type.
    pub fn into_request_spec(self) -> netanvil_types::RequestSpec {
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
        netanvil_types::RequestSpec {
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
