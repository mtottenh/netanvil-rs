use netanvil_types::{RequestContext, RequestTransformer};

use crate::spec::UdpRequestSpec;

/// No-op transformer that passes UDP requests through unchanged.
pub struct UdpNoopTransformer;

impl RequestTransformer for UdpNoopTransformer {
    type Spec = UdpRequestSpec;

    fn transform(&self, spec: UdpRequestSpec, _context: &RequestContext) -> UdpRequestSpec {
        spec
    }
}
