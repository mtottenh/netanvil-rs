//! Simple round-robin TCP request generator.

use std::net::SocketAddr;

use netanvil_types::{RequestContext, RequestGenerator};

use crate::spec::{TcpFraming, TcpRequestSpec};

/// Generates identical TCP requests, cycling through targets round-robin.
pub struct SimpleTcpGenerator {
    targets: Vec<SocketAddr>,
    payload: Vec<u8>,
    framing: TcpFraming,
    expect_response: bool,
    response_max_bytes: usize,
    index: usize,
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
            index: 0,
        }
    }

    pub fn with_response_max_bytes(mut self, max: usize) -> Self {
        self.response_max_bytes = max;
        self
    }
}

impl RequestGenerator for SimpleTcpGenerator {
    type Spec = TcpRequestSpec;

    fn generate(&mut self, _context: &RequestContext) -> TcpRequestSpec {
        let target = self.targets[self.index % self.targets.len()];
        self.index += 1;
        TcpRequestSpec {
            target,
            payload: self.payload.clone(),
            framing: self.framing.clone(),
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
