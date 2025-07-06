use std::net::SocketAddr;

use netanvil_types::{RequestContext, RequestGenerator};

use crate::spec::UdpRequestSpec;

/// Simple round-robin UDP request generator.
///
/// Cycles through the configured target addresses, producing a `UdpRequestSpec`
/// with the same payload for each request.
pub struct SimpleUdpGenerator {
    targets: Vec<SocketAddr>,
    payload: Vec<u8>,
    expect_response: bool,
    response_max_bytes: usize,
    index: usize,
}

impl SimpleUdpGenerator {
    pub fn new(targets: Vec<SocketAddr>, payload: Vec<u8>, expect_response: bool) -> Self {
        Self {
            targets,
            payload,
            expect_response,
            response_max_bytes: 65536,
            index: 0,
        }
    }

    /// Set the maximum response datagram size in bytes (default: 65536).
    pub fn with_response_max_bytes(mut self, max_bytes: usize) -> Self {
        self.response_max_bytes = max_bytes;
        self
    }
}

impl RequestGenerator for SimpleUdpGenerator {
    type Spec = UdpRequestSpec;

    fn generate(&mut self, _context: &RequestContext) -> UdpRequestSpec {
        let target = self.targets[self.index % self.targets.len()];
        self.index += 1;
        UdpRequestSpec {
            target,
            payload: self.payload.clone(),
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
