//! Simple round-robin DNS request generator.

use std::net::SocketAddr;

use netanvil_types::dns_spec::{DnsQueryType, DnsRequestSpec};
use netanvil_types::{RequestContext, RequestGenerator};

/// Generates DNS queries cycling through servers and domains round-robin.
pub struct SimpleDnsGenerator {
    servers: Vec<SocketAddr>,
    domains: Vec<String>,
    query_type: DnsQueryType,
    recursion: bool,
    index: usize,
}

impl SimpleDnsGenerator {
    pub fn new(
        servers: Vec<SocketAddr>,
        domains: Vec<String>,
        query_type: DnsQueryType,
        recursion: bool,
    ) -> Self {
        Self {
            servers,
            domains,
            query_type,
            recursion,
            index: 0,
        }
    }
}

impl RequestGenerator for SimpleDnsGenerator {
    type Spec = DnsRequestSpec;

    fn generate(&mut self, _ctx: &RequestContext) -> DnsRequestSpec {
        let server = self.servers[self.index % self.servers.len()];
        let domain = self.domains[self.index % self.domains.len()].clone();
        self.index += 1;

        DnsRequestSpec {
            server,
            query_name: domain,
            query_type: self.query_type,
            recursion: self.recursion,
            dnssec: false,
            txid: 0,
        }
    }

    fn update_targets(&mut self, targets: Vec<String>) {
        let parsed: Vec<SocketAddr> = targets.iter().filter_map(|s| s.parse().ok()).collect();
        if !parsed.is_empty() {
            self.servers = parsed;
            self.index = 0;
        }
    }
}
