use std::cell::RefCell;

use netanvil_types::{RequestContext, RequestSpec, RequestTransformer};

/// Transformer that passes requests through unchanged.
pub struct NoopTransformer;

impl RequestTransformer for NoopTransformer {
    fn transform(&self, spec: RequestSpec, _context: &RequestContext) -> RequestSpec {
        spec
    }
}

/// Transformer that adds configured headers to every request.
///
/// Uses `RefCell` for interior mutability so headers can be updated
/// mid-test via `update_headers(&self, ...)` while the transformer
/// is `Rc`-shared with spawned tasks.
pub struct HeaderTransformer {
    headers: RefCell<Vec<(String, String)>>,
}

impl HeaderTransformer {
    pub fn new(headers: Vec<(String, String)>) -> Self {
        Self {
            headers: RefCell::new(headers),
        }
    }
}

impl RequestTransformer for HeaderTransformer {
    fn transform(&self, mut spec: RequestSpec, _context: &RequestContext) -> RequestSpec {
        spec.headers.extend(self.headers.borrow().iter().cloned());
        spec
    }

    fn update_headers(&self, headers: Vec<(String, String)>) {
        *self.headers.borrow_mut() = headers;
    }
}
