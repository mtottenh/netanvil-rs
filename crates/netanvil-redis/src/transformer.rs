//! No-op request transformer for Redis.

use netanvil_types::redis_spec::RedisRequestSpec;
use netanvil_types::{RequestContext, RequestTransformer};

/// Pass-through transformer that returns the spec unchanged.
///
/// For Redis load tests that don't need per-request mutation, use this as the
/// default transformer.
pub struct RedisNoopTransformer;

impl RequestTransformer for RedisNoopTransformer {
    type Spec = RedisRequestSpec;

    fn transform(&self, spec: RedisRequestSpec, _context: &RequestContext) -> RedisRequestSpec {
        spec
    }
}
