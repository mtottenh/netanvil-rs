//! Simple round-robin TCP request generator.

use std::net::SocketAddr;

use netanvil_types::{RequestContext, RequestGenerator};

use crate::spec::{TcpFraming, TcpRequestSpec, TcpTestMode};

/// Generates identical TCP requests, cycling through targets round-robin.
pub struct SimpleTcpGenerator {
    targets: Vec<SocketAddr>,
    payload: Vec<u8>,
    framing: TcpFraming,
    expect_response: bool,
    response_max_bytes: usize,
    mode: TcpTestMode,
    request_size: u16,
    response_size: u32,
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
            mode: TcpTestMode::Echo,
            request_size: 0,
            response_size: 0,
            index: 0,
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

    pub fn with_request_size(mut self, size: u16) -> Self {
        self.request_size = size;
        self
    }

    pub fn with_response_size(mut self, size: u32) -> Self {
        self.response_size = size;
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
            mode: self.mode,
            request_size: self.request_size,
            response_size: self.response_size,
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
