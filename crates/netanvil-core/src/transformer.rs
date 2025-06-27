use std::cell::{Cell, RefCell};

use netanvil_types::{
    ConnectionPolicy, CountDistribution, RequestContext, RequestSpec, RequestTransformer,
};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Distribution, Normal};

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

/// Transformer that controls connection lifecycle by conditionally adding
/// `Connection: close` headers based on the configured policy.
///
/// Wraps an inner transformer so headers and connection policy compose.
///
/// For the `Mixed` policy with a `connection_lifetime`, the transformer
/// simulates connection lifecycles: it samples a request count from the
/// distribution, counts requests, and sends `Connection: close` when the
/// limit is reached, then resamples for the next "connection".
pub struct ConnectionPolicyTransformer {
    inner: Box<dyn RequestTransformer>,
    policy: ConnectionPolicy,
    rng: RefCell<SmallRng>,
    /// Requests sent on the current simulated connection.
    conn_request_count: Cell<u32>,
    /// How many requests until the current connection closes (sampled from distribution).
    conn_lifetime: Cell<u32>,
}

impl ConnectionPolicyTransformer {
    pub fn new(inner: Box<dyn RequestTransformer>, policy: ConnectionPolicy) -> Self {
        let mut rng = SmallRng::from_entropy();
        // Sample the initial connection lifetime
        let initial_lifetime = match &policy {
            ConnectionPolicy::Mixed {
                connection_lifetime: Some(dist),
                ..
            } => sample_count(dist, &mut rng),
            _ => u32::MAX,
        };

        Self {
            inner,
            policy,
            rng: RefCell::new(rng),
            conn_request_count: Cell::new(0),
            conn_lifetime: Cell::new(initial_lifetime),
        }
    }
}

/// Sample a positive integer from a CountDistribution.
fn sample_count(dist: &CountDistribution, rng: &mut SmallRng) -> u32 {
    match dist {
        CountDistribution::Fixed(n) => *n,
        CountDistribution::Uniform { min, max } => rng.gen_range(*min..=*max),
        CountDistribution::Normal { mean, stddev } => {
            let normal =
                Normal::new(*mean, *stddev).unwrap_or_else(|_| Normal::new(1.0, 0.0).unwrap());
            let sample: f64 = normal.sample(rng);
            // Clamp to >= 1
            sample.round().max(1.0) as u32
        }
    }
}

impl RequestTransformer for ConnectionPolicyTransformer {
    fn transform(&self, spec: RequestSpec, context: &RequestContext) -> RequestSpec {
        // Apply inner transformer first (headers, auth, etc.)
        let mut spec = self.inner.transform(spec, context);

        let close = match &self.policy {
            ConnectionPolicy::KeepAlive => false,
            ConnectionPolicy::AlwaysNew => true,
            ConnectionPolicy::Mixed {
                persistent_ratio,
                connection_lifetime,
            } => {
                let count = self.conn_request_count.get();

                // Check if the simulated connection has reached its lifetime
                let lifetime_exceeded = count >= self.conn_lifetime.get();

                if lifetime_exceeded {
                    // "Close" this connection and start a new one:
                    // reset counter (this request is #0 of the new connection)
                    // and resample lifetime
                    self.conn_request_count.set(1);
                    if let Some(dist) = connection_lifetime {
                        let new_lifetime = sample_count(dist, &mut self.rng.borrow_mut());
                        self.conn_lifetime.set(new_lifetime);
                    }
                    true
                } else {
                    self.conn_request_count.set(count + 1);
                    // Random decision based on persistent ratio
                    !self.rng.borrow_mut().gen_bool(*persistent_ratio)
                }
            }
        };

        if close {
            spec.headers.push(("Connection".into(), "close".into()));
        }

        spec
    }

    fn update_headers(&self, headers: Vec<(String, String)>) {
        self.inner.update_headers(headers);
    }
}
