use netanvil_types::{RequestContext, RequestSpec, RequestTransformer};

/// Transformer that passes requests through unchanged.
pub struct NoopTransformer;

impl RequestTransformer for NoopTransformer {
    fn transform(&self, spec: RequestSpec, _context: &RequestContext) -> RequestSpec {
        spec
    }
}

/// Transformer that adds configured headers to every request.
pub struct HeaderTransformer {
    headers: Vec<(String, String)>,
}

impl HeaderTransformer {
    pub fn new(headers: Vec<(String, String)>) -> Self {
        Self { headers }
    }
}

impl RequestTransformer for HeaderTransformer {
    fn transform(&self, mut spec: RequestSpec, _context: &RequestContext) -> RequestSpec {
        spec.headers.extend(self.headers.iter().cloned());
        spec
    }
}
