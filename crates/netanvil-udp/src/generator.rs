use std::net::SocketAddr;

use netanvil_types::distribution::ValueDistribution;
use netanvil_types::{RequestContext, RequestGenerator};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Distribution, Normal};

use crate::spec::UdpRequestSpec;

/// Simple round-robin UDP request generator with distribution-driven sizes.
///
/// Cycles through the configured target addresses, producing a `UdpRequestSpec`
/// with the same base payload for each request.  Each datagram is prefixed with
/// an 8-byte little-endian sequence number so the executor can correlate
/// responses and detect loss.
///
/// When `payload_size_dist` is set to a non-Fixed distribution, the payload is
/// truncated or padded to the sampled size on each request.
pub struct SimpleUdpGenerator {
    targets: Vec<SocketAddr>,
    payload: Vec<u8>,
    expect_response: bool,
    response_max_bytes: usize,
    payload_size_dist: Option<ValueDistribution<usize>>,
    index: usize,
    seq: u64,
    rng: SmallRng,
}

impl SimpleUdpGenerator {
    pub fn new(targets: Vec<SocketAddr>, payload: Vec<u8>, expect_response: bool) -> Self {
        Self {
            targets,
            payload,
            expect_response,
            response_max_bytes: 65536,
            payload_size_dist: None,
            index: 0,
            seq: 0,
            rng: SmallRng::from_entropy(),
        }
    }

    /// Set the maximum response datagram size in bytes (default: 65536).
    pub fn with_response_max_bytes(mut self, max_bytes: usize) -> Self {
        self.response_max_bytes = max_bytes;
        self
    }

    /// Set payload size distribution (supports Fixed, Uniform, Normal, Weighted).
    ///
    /// When set, each datagram's payload (excluding the 8-byte sequence header)
    /// is sized according to the distribution.
    pub fn with_payload_size_dist(mut self, dist: ValueDistribution<usize>) -> Self {
        self.payload_size_dist = Some(dist);
        self
    }
}

impl RequestGenerator for SimpleUdpGenerator {
    type Spec = UdpRequestSpec;

    fn generate(&mut self, _context: &RequestContext) -> UdpRequestSpec {
        let target = self.targets[self.index % self.targets.len()];
        self.index += 1;

        // Prepend 8-byte LE sequence number to the payload.
        let seq = self.seq;
        self.seq += 1;

        // Determine payload body (after sequence header)
        let body = if let Some(ref dist) = self.payload_size_dist {
            let size = sample_usize(dist, &mut self.rng);
            if self.payload.len() >= size {
                &self.payload[..size]
            } else {
                // Need to pad — use a temporary allocation
                let mut padded = self.payload.clone();
                padded.resize(size, 0);
                return UdpRequestSpec {
                    target,
                    payload: {
                        let mut wire = Vec::with_capacity(8 + size);
                        wire.extend_from_slice(&seq.to_le_bytes());
                        wire.extend_from_slice(&padded);
                        wire
                    },
                    expect_response: self.expect_response,
                    response_max_bytes: self.response_max_bytes,
                };
            }
        } else {
            &self.payload[..]
        };

        let mut wire_payload = Vec::with_capacity(8 + body.len());
        wire_payload.extend_from_slice(&seq.to_le_bytes());
        wire_payload.extend_from_slice(body);

        UdpRequestSpec {
            target,
            payload: wire_payload,
            expect_response: self.expect_response,
            response_max_bytes: self.response_max_bytes,
        }
    }

    fn update_targets(&mut self, targets: Vec<String>) {
        let parsed: Vec<SocketAddr> = targets.iter().filter_map(|s| s.parse().ok()).collect();
        if !parsed.is_empty() {
            self.targets = parsed;
            self.index = 0;
        }
    }
}

fn sample_usize(dist: &ValueDistribution<usize>, rng: &mut SmallRng) -> usize {
    match dist {
        ValueDistribution::Fixed(n) => *n,
        ValueDistribution::Uniform { min, max } => rng.gen_range(*min..=*max),
        ValueDistribution::Normal { mean, stddev } => {
            let normal = Normal::new(*mean, *stddev)
                .unwrap_or_else(|_| Normal::new(1.0, 0.0).unwrap());
            let s: f64 = normal.sample(rng);
            s.round().max(1.0) as usize
        }
        ValueDistribution::Weighted(entries) => {
            let total: f64 = entries.iter().map(|e| e.weight).sum();
            let roll: f64 = rng.gen_range(0.0..total);
            let mut cumulative = 0.0;
            for entry in entries {
                cumulative += entry.weight;
                if roll < cumulative {
                    return entry.value;
                }
            }
            entries.last().unwrap().value
        }
    }
}
