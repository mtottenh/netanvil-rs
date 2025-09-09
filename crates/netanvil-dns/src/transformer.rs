//! No-op DNS request transformer.

use netanvil_types::dns_spec::DnsRequestSpec;
use netanvil_types::{RequestContext, RequestTransformer};

/// Pass-through transformer for DNS requests.
pub struct DnsNoopTransformer;

impl RequestTransformer for DnsNoopTransformer {
    type Spec = DnsRequestSpec;

    fn transform(&self, spec: DnsRequestSpec, _ctx: &RequestContext) -> DnsRequestSpec {
        spec
    }
}
