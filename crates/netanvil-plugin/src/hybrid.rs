//! Hybrid plugin approach: script for configuration, native Rust for hot path.
//!
//! The plugin is called once at setup to produce a `GeneratorConfig` that
//! describes URL patterns, header distributions, and body templates. The hot
//! path then executes in pure Rust using this configuration — no per-request
//! plugin overhead.
//!
//! This approach trades flexibility (can't run arbitrary logic per-request)
//! for performance (native Rust speed on the hot path).

use serde::{Deserialize, Serialize};

use netanvil_types::{HttpRequestSpec, RequestContext, RequestGenerator};

/// Configuration produced by a plugin at setup time.
///
/// Describes the request generation pattern declaratively.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorConfig {
    /// URL patterns with optional weight for weighted random selection.
    /// Patterns support `{seq}`, `{core_id}`, `{request_id}` placeholders.
    pub url_patterns: Vec<WeightedPattern>,
    /// HTTP method to use.
    pub method: String,
    /// Static headers to add to every request.
    pub headers: Vec<(String, String)>,
    /// Body template with placeholders. None for no body.
    pub body_template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightedPattern {
    pub pattern: String,
    pub weight: f64,
}

/// A native Rust generator driven by a plugin-produced configuration.
///
/// After the plugin produces `GeneratorConfig`, this generator executes
/// at native speed with zero plugin overhead per request.
pub struct HybridGenerator {
    config: GeneratorConfig,
    method: http::Method,
    counter: u64,
    /// Cumulative weights for weighted selection.
    cumulative_weights: Vec<f64>,
    total_weight: f64,
}

impl HybridGenerator {
    pub fn new(config: GeneratorConfig) -> Self {
        let method = match config.method.to_uppercase().as_str() {
            "GET" => http::Method::GET,
            "POST" => http::Method::POST,
            "PUT" => http::Method::PUT,
            "DELETE" => http::Method::DELETE,
            "PATCH" => http::Method::PATCH,
            other => http::Method::from_bytes(other.as_bytes()).unwrap_or(http::Method::GET),
        };

        let mut cumulative = Vec::with_capacity(config.url_patterns.len());
        let mut total = 0.0;
        for p in &config.url_patterns {
            total += p.weight;
            cumulative.push(total);
        }

        Self {
            config,
            method,
            counter: 0,
            cumulative_weights: cumulative,
            total_weight: total,
        }
    }

    /// Select a URL pattern index based on counter (deterministic round-robin
    /// weighted selection using modular arithmetic).
    fn select_pattern(&self) -> usize {
        if self.config.url_patterns.len() == 1 {
            return 0;
        }
        // Simple round-robin for equal weights, weighted for unequal
        let position = (self.counter as f64 % self.total_weight) + 0.5;
        self.cumulative_weights
            .iter()
            .position(|&w| position <= w)
            .unwrap_or(0)
    }

    /// Expand placeholders in a pattern string.
    fn expand(&self, pattern: &str, ctx: &RequestContext) -> String {
        pattern
            .replace("{seq}", &self.counter.to_string())
            .replace("{core_id}", &ctx.core_id.to_string())
            .replace("{request_id}", &ctx.request_id.to_string())
    }
}

impl RequestGenerator for HybridGenerator {
    type Spec = HttpRequestSpec;

    fn generate(&mut self, context: &RequestContext) -> HttpRequestSpec {
        let idx = self.select_pattern();
        let pattern = &self.config.url_patterns[idx].pattern;
        let url = self.expand(pattern, context);

        let body = self
            .config
            .body_template
            .as_ref()
            .map(|t| self.expand(t, context).into_bytes());

        self.counter += 1;

        HttpRequestSpec {
            method: self.method.clone(),
            url,
            headers: self.config.headers.clone(),
            body,
        }
    }
}

// `config_from_lua()` is provided by `netanvil-plugin-luajit` and `netanvil-plugin-lua54`
// crates, which can parse hybrid configuration scripts using their respective Lua runtimes.
