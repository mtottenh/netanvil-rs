//! Simple round-robin TCP request generator with distribution-driven sizes.

use std::net::SocketAddr;

use netanvil_sampling::Sampler;
use netanvil_types::distribution::ValueDistribution;
use netanvil_types::{RequestContext, RequestGenerator};
use rand::rngs::SmallRng;
use rand::SeedableRng;

use crate::spec::{TcpFraming, TcpRequestSpec, TcpTestMode};

/// Generates TCP requests cycling through targets round-robin.
///
/// `request_size` and `response_size` are sampled per-request from
/// pre-compiled [`Sampler`]s, enabling packet size variation (fixed, uniform,
/// normal, exponential, log-normal, pareto, zipf, or weighted distributions).
pub struct SimpleTcpGenerator {
    targets: Vec<SocketAddr>,
    payload: Vec<u8>,
    framing: TcpFraming,
    expect_response: bool,
    response_max_bytes: usize,
    mode: TcpTestMode,
    request_size_sampler: Sampler<u16>,
    response_size_sampler: Sampler<u32>,
    index: usize,
    rng: SmallRng,
    latency_us: Option<u32>,
    error_rate: Option<u32>,
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
            request_size_sampler: Sampler::new(&ValueDistribution::Fixed(0)),
            response_size_sampler: Sampler::new(&ValueDistribution::Fixed(0)),
            index: 0,
            rng: SmallRng::from_entropy(),
            latency_us: None,
            error_rate: None,
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
        self.request_size_sampler = Sampler::new(&ValueDistribution::Fixed(size));
        self
    }

    /// Set a fixed response size (backward-compatible convenience).
    pub fn with_response_size(mut self, size: u32) -> Self {
        self.response_size_sampler = Sampler::new(&ValueDistribution::Fixed(size));
        self
    }

    /// Set request size distribution.
    pub fn with_request_size_dist(mut self, dist: ValueDistribution<u16>) -> Self {
        self.request_size_sampler = Sampler::new(&dist);
        self
    }

    /// Set response size distribution.
    pub fn with_response_size_dist(mut self, dist: ValueDistribution<u32>) -> Self {
        self.response_size_sampler = Sampler::new(&dist);
        self
    }

    /// Set server-side latency injection in microseconds (v2 protocol header).
    pub fn with_latency_us(mut self, us: u32) -> Self {
        self.latency_us = Some(us);
        self
    }

    /// Set server-side error injection rate per 10,000 requests (v2 protocol header).
    pub fn with_error_rate(mut self, rate: u32) -> Self {
        self.error_rate = Some(rate);
        self
    }
}

impl RequestGenerator for SimpleTcpGenerator {
    type Spec = TcpRequestSpec;

    fn generate(&mut self, _context: &RequestContext) -> TcpRequestSpec {
        let target = self.targets[self.index % self.targets.len()];
        self.index += 1;

        let request_size = self.request_size_sampler.sample(&mut self.rng);
        let response_size = self.response_size_sampler.sample(&mut self.rng);

        TcpRequestSpec {
            target,
            payload: self.payload.clone(),
            framing: self.framing.clone(),
            expect_response: self.expect_response,
            response_max_bytes: self.response_max_bytes,
            mode: self.mode,
            request_size,
            response_size,
            latency_us: self.latency_us,
            error_rate: self.error_rate,
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
