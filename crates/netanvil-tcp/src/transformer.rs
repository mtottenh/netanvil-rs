//! No-op request transformer for TCP.

use netanvil_types::{RequestContext, RequestTransformer};

use crate::spec::TcpRequestSpec;

/// Pass-through transformer that returns the spec unchanged.
///
/// For TCP load tests that don't need per-request mutation, use this as the
/// default transformer.
pub struct TcpNoopTransformer;

impl RequestTransformer for TcpNoopTransformer {
    type Spec = TcpRequestSpec;

    fn transform(&self, spec: TcpRequestSpec, _context: &RequestContext) -> TcpRequestSpec {
        spec
    }
}
