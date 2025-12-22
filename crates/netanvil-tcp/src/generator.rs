//! Simple round-robin TCP request generator with distribution-driven sizes.

use std::net::SocketAddr;

use netanvil_types::distribution::ValueDistribution;
use netanvil_types::{RequestContext, RequestGenerator};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Distribution, Normal};

use crate::spec::{TcpFraming, TcpRequestSpec, TcpTestMode};

/// Generates TCP requests cycling through targets round-robin.
///
/// `request_size` and `response_size` are sampled per-request from
/// [`ValueDistribution`], enabling packet size variation (fixed, uniform,
/// normal, or weighted distributions).
pub struct SimpleTcpGenerator {
    targets: Vec<SocketAddr>,
    payload: Vec<u8>,
    framing: TcpFraming,
    expect_response: bool,
    response_max_bytes: usize,
    mode: TcpTestMode,
    request_size_dist: ValueDistribution<u16>,
    response_size_dist: ValueDistribution<u32>,
    index: usize,
    rng: SmallRng,
}

impl SimpleTcpGenerator {
    pub fn new(
        targets: Vec<SocketAddr>,
        payload: Vec<u8>,
        framing: TcpFraming,
        expect_response: bool,
    ) -> Self {
        Self {
            targets,
            payload,
            framing,
            expect_response,
            response_max_bytes: 65536,
            mode: TcpTestMode::Echo,
            request_size_dist: ValueDistribution::Fixed(0),
            response_size_dist: ValueDistribution::Fixed(0),
            index: 0,
            rng: SmallRng::from_entropy(),
        }
    }

    pub fn with_response_max_bytes(mut self, max: usize) -> Self {
        self.response_max_bytes = max;
        self
    }

    pub fn with_mode(mut self, mode: TcpTestMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set a fixed request size (backward-compatible convenience).
    pub fn with_request_size(mut self, size: u16) -> Self {
        self.request_size_dist = ValueDistribution::Fixed(size);
        self
    }

    /// Set a fixed response size (backward-compatible convenience).
    pub fn with_response_size(mut self, size: u32) -> Self {
        self.response_size_dist = ValueDistribution::Fixed(size);
        self
    }

    /// Set request size distribution (supports Fixed, Uniform, Normal, Weighted).
    pub fn with_request_size_dist(mut self, dist: ValueDistribution<u16>) -> Self {
        self.request_size_dist = dist;
        self
    }

    /// Set response size distribution (supports Fixed, Uniform, Normal, Weighted).
    pub fn with_response_size_dist(mut self, dist: ValueDistribution<u32>) -> Self {
        self.response_size_dist = dist;
        self
    }
}

impl RequestGenerator for SimpleTcpGenerator {
    type Spec = TcpRequestSpec;

    fn generate(&mut self, _context: &RequestContext) -> TcpRequestSpec {
        let target = self.targets[self.index % self.targets.len()];
        self.index += 1;

        let request_size = sample_u16(&self.request_size_dist, &mut self.rng);
        let response_size = sample_u32(&self.response_size_dist, &mut self.rng);

        TcpRequestSpec {
            target,
            payload: self.payload.clone(),
            framing: self.framing.clone(),
            expect_response: self.expect_response,
            response_max_bytes: self.response_max_bytes,
            mode: self.mode,
            request_size,
            response_size,
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

// ---------------------------------------------------------------------------
// Sampling helpers (duplicated from netanvil-core/lifecycle to avoid circular dep)
// ---------------------------------------------------------------------------

fn sample_u16(dist: &ValueDistribution<u16>, rng: &mut SmallRng) -> u16 {
    match dist {
        ValueDistribution::Fixed(n) => *n,
        ValueDistribution::Uniform { min, max } => rng.gen_range(*min..=*max),
        ValueDistribution::Normal { mean, stddev } => {
            let normal = Normal::new(*mean, *stddev)
                .unwrap_or_else(|_| Normal::new(1.0, 0.0).unwrap());
            let s: f64 = normal.sample(rng);
            s.round().max(1.0).min(u16::MAX as f64) as u16
        }
        ValueDistribution::Weighted(entries) => sample_weighted_val(entries, rng),
    }
}

fn sample_u32(dist: &ValueDistribution<u32>, rng: &mut SmallRng) -> u32 {
    match dist {
        ValueDistribution::Fixed(n) => *n,
        ValueDistribution::Uniform { min, max } => rng.gen_range(*min..=*max),
        ValueDistribution::Normal { mean, stddev } => {
            let normal = Normal::new(*mean, *stddev)
                .unwrap_or_else(|_| Normal::new(1.0, 0.0).unwrap());
            let s: f64 = normal.sample(rng);
            s.round().max(1.0) as u32
        }
        ValueDistribution::Weighted(entries) => sample_weighted_val(entries, rng),
    }
}

fn sample_weighted_val<T: Copy>(
    entries: &[netanvil_types::distribution::WeightedValue<T>],
    rng: &mut SmallRng,
) -> T {
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
