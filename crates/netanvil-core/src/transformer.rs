use std::cell::RefCell;

use netanvil_types::{ConnectionPolicy, HttpRequestSpec, RequestContext, RequestTransformer};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use crate::lifecycle::LifecycleCounter;

/// Transformer that passes requests through unchanged.
pub struct NoopTransformer;

impl RequestTransformer for NoopTransformer {
    type Spec = HttpRequestSpec;

    fn transform(&self, spec: HttpRequestSpec, _context: &RequestContext) -> HttpRequestSpec {
        spec
    }
}

/// Transformer that adds configured headers to every request.
///
/// Uses `RefCell` for interior mutability so headers can be updated
/// mid-test via `update_metadata(&self, ...)` while the transformer
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
    type Spec = HttpRequestSpec;

    fn transform(&self, mut spec: HttpRequestSpec, _context: &RequestContext) -> HttpRequestSpec {
        spec.headers.extend(self.headers.borrow().iter().cloned());
        spec
    }

    fn update_metadata(&self, headers: Vec<(String, String)>) {
        *self.headers.borrow_mut() = headers;
    }
}

/// Transformer that controls connection lifecycle by conditionally adding
/// `Connection: close` headers based on the configured policy.
///
/// Wraps an inner transformer so headers and connection policy compose.
///
/// For the `Mixed` policy with a `connection_lifetime`, a [`LifecycleCounter`]
/// tracks requests per simulated connection and signals when to close.
pub struct ConnectionPolicyTransformer {
    inner: Box<dyn RequestTransformer<Spec = HttpRequestSpec>>,
    policy: ConnectionPolicy,
    /// Lifecycle counter for Mixed policy with connection_lifetime.
    /// `None` when the policy is not Mixed or has no lifetime distribution.
    lifecycle: RefCell<Option<LifecycleCounter>>,
    rng: RefCell<SmallRng>,
}

impl ConnectionPolicyTransformer {
    pub fn new(
        inner: Box<dyn RequestTransformer<Spec = HttpRequestSpec>>,
        policy: ConnectionPolicy,
    ) -> Self {
        let lifecycle = match &policy {
            ConnectionPolicy::Mixed {
                connection_lifetime: Some(dist),
                ..
            } => Some(LifecycleCounter::new(dist.clone())),
            _ => None,
        };

        Self {
            inner,
            policy,
            lifecycle: RefCell::new(lifecycle),
            rng: RefCell::new(SmallRng::from_entropy()),
        }
    }

    /// Create with a deterministic seed for reproducible behavior in tests.
    /// Production code should use `new()` which seeds from OS entropy.
    pub fn with_seed(
        inner: Box<dyn RequestTransformer<Spec = HttpRequestSpec>>,
        policy: ConnectionPolicy,
        seed: u64,
    ) -> Self {
        let lifecycle = match &policy {
            ConnectionPolicy::Mixed {
                connection_lifetime: Some(dist),
                ..
            } => Some(LifecycleCounter::with_seed(dist.clone(), seed)),
            _ => None,
        };

        Self {
            inner,
            policy,
            lifecycle: RefCell::new(lifecycle),
            rng: RefCell::new(SmallRng::seed_from_u64(seed)),
        }
    }
}

impl RequestTransformer for ConnectionPolicyTransformer {
    type Spec = HttpRequestSpec;

    fn transform(&self, spec: HttpRequestSpec, context: &RequestContext) -> HttpRequestSpec {
        // Apply inner transformer first (headers, auth, etc.)
        let mut spec = self.inner.transform(spec, context);

        let close = match &self.policy {
            ConnectionPolicy::KeepAlive => false,
            ConnectionPolicy::AlwaysNew => true,
            ConnectionPolicy::NoReuse => false, // Don't send Connection: close — leave connections dangling
            ConnectionPolicy::Mixed {
                persistent_ratio, ..
            } => {
                let mut lifecycle = self.lifecycle.borrow_mut();
                if let Some(ref mut lc) = *lifecycle {
                    if lc.tick() {
                        // This request closes the old connection. Count it as
                        // request #1 of the new connection (matches HTTP/1.1
                        // behavior where the close request opens a fresh conn).
                        lc.reset();
                        lc.tick();
                        true
                    } else {
                        // Random decision based on persistent ratio
                        !self.rng.borrow_mut().gen_bool(*persistent_ratio)
                    }
                } else {
                    // No lifetime distribution — just use persistent_ratio
                    !self.rng.borrow_mut().gen_bool(*persistent_ratio)
                }
            }
        };

        if close {
            spec.headers.push(("Connection".into(), "close".into()));
        }

        spec
    }

    fn update_metadata(&self, headers: Vec<(String, String)>) {
        self.inner.update_metadata(headers);
    }
}
