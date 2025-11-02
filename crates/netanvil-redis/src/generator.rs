//! Simple round-robin Redis request generator.

use std::net::SocketAddr;

use netanvil_types::redis_spec::RedisRequestSpec;
use netanvil_types::{RequestContext, RequestGenerator};

/// Generates identical Redis commands, cycling through targets round-robin.
///
/// For dynamic command generation, use a plugin-backed generator (Lua, WASM, Rhai).
pub struct SimpleRedisGenerator {
    targets: Vec<SocketAddr>,
    command: String,
    args_template: Vec<String>,
    index: usize,
}

impl SimpleRedisGenerator {
    pub fn new(targets: Vec<SocketAddr>, command: String, args_template: Vec<String>) -> Self {
        Self {
            targets,
            command,
            args_template,
            index: 0,
        }
    }
}

impl RequestGenerator for SimpleRedisGenerator {
    type Spec = RedisRequestSpec;

    fn generate(&mut self, _context: &RequestContext) -> RedisRequestSpec {
        let target = self.targets[self.index % self.targets.len()];
        self.index += 1;

        RedisRequestSpec {
            target,
            command: self.command.clone(),
            args: self.args_template.clone(),
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
