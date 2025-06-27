//! Example: Custom Request Generator
//!
//! This example demonstrates how to write a custom `RequestGenerator` that
//! creates varied requests for more realistic load testing. The generator
//! produces requests with:
//!
//! - Cache-busting query parameters (unique per request)
//! - JSON request bodies with randomized data
//! - Per-core state partitioning (each core generates disjoint IDs)
//!
//! # Running
//!
//! Start a target server (e.g., httpbin or any HTTP server), then:
//!
//! ```bash
//! cargo run --example custom_generator --release -- http://localhost:8080/post
//! ```
//!
//! # Architecture Notes
//!
//! - `RequestGenerator::generate(&mut self, ...)` takes `&mut self`, giving you
//!   exclusive mutable access to your state. No synchronization needed.
//! - The generator is `!Send` — it lives on a single I/O worker core and is
//!   never moved between threads.
//! - The `generator_factory` closure receives `core_id` so you can partition
//!   state: different user ID ranges, different URL prefixes, etc.
//! - One generator instance is created per core. With 4 cores, you get 4
//!   independent generator instances, each producing ~25% of total requests.

use std::time::Duration;

use netanvil_core::TestBuilder;
use netanvil_http::HttpExecutor;
use netanvil_types::{RateConfig, RequestContext, RequestGenerator, RequestSpec, TestConfig};

// ---------------------------------------------------------------------------
// Custom generator: simulates an API client making varied POST requests
// ---------------------------------------------------------------------------

/// Generates POST requests with JSON bodies, simulating an API workload.
///
/// Each request has:
/// - A unique user ID (partitioned by core to avoid collisions)
/// - A cache-busting query parameter
/// - A JSON body with request-specific data
struct ApiWorkloadGenerator {
    /// Base URL to send requests to
    base_url: String,
    /// Starting user ID for this core (each core gets a disjoint range)
    user_id_base: u64,
    /// Per-request counter
    counter: u64,
}

impl ApiWorkloadGenerator {
    fn new(base_url: String, core_id: usize) -> Self {
        Self {
            base_url,
            // Each core gets IDs in a separate range:
            // Core 0: 0..999_999, Core 1: 1_000_000..1_999_999, etc.
            user_id_base: core_id as u64 * 1_000_000,
            counter: 0,
        }
    }
}

impl RequestGenerator for ApiWorkloadGenerator {
    fn generate(&mut self, ctx: &RequestContext) -> RequestSpec {
        let user_id = self.user_id_base + (self.counter % 1000);
        let request_num = self.counter;
        self.counter += 1;

        // Build a URL with cache-busting parameter
        let url = format!(
            "{}?user_id={}&seq={}&core={}",
            self.base_url, user_id, request_num, ctx.core_id
        );

        // Build a JSON body
        let body = format!(
            r#"{{"user_id":{},"action":"test","seq":{},"timestamp_ns":{}}}"#,
            user_id,
            request_num,
            ctx.intended_time.elapsed().as_nanos(),
        );

        RequestSpec {
            method: http::Method::POST,
            url,
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-Request-ID".into(), format!("{}", ctx.request_id)),
            ],
            body: Some(body.into_bytes()),
        }
    }

    fn update_targets(&mut self, targets: Vec<String>) {
        if let Some(url) = targets.into_iter().next() {
            self.base_url = url;
        }
    }
}

// ---------------------------------------------------------------------------
// Main: wire everything together using TestBuilder
// ---------------------------------------------------------------------------

fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    // Get target URL from command line
    let target_url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://localhost:8080/post".into());

    let config = TestConfig {
        targets: vec![target_url.clone()],
        method: "POST".into(), // default method (overridden by generator)
        duration: Duration::from_secs(10),
        rate: RateConfig::Static { rps: 500.0 },
        num_cores: 2,
        ..Default::default()
    };

    tracing::info!(
        url = %target_url,
        rps = 500.0,
        duration = "10s",
        cores = 2,
        "starting custom generator load test"
    );

    // Use TestBuilder with a custom generator factory.
    //
    // The factory is called once per core. Each core gets its own
    // ApiWorkloadGenerator with a disjoint user ID range.
    let result = TestBuilder::new(
        config,
        // Executor factory: one HTTP client per core
        || HttpExecutor::with_timeout(Duration::from_secs(30)),
    )
    .generator_factory(move |core_id| {
        Box::new(ApiWorkloadGenerator::new(target_url.clone(), core_id))
            as Box<dyn RequestGenerator>
    })
    .on_progress(|update| {
        eprint!(
            "\r  [{:.1}s] {:.0}/{:.0} RPS | {} requests | {} errors | p99={:.1}ms",
            update.elapsed.as_secs_f64(),
            update.current_rps,
            update.target_rps,
            update.total_requests,
            update.total_errors,
            update.window.latency_p99_ns as f64 / 1_000_000.0,
        );
    })
    .run()
    .expect("load test failed");

    eprintln!();
    eprintln!();
    eprintln!("Results:");
    eprintln!("  Total requests: {}", result.total_requests);
    eprintln!("  Total errors:   {}", result.total_errors);
    eprintln!("  Duration:       {:.1}s", result.duration.as_secs_f64());
    eprintln!("  Avg RPS:        {:.1}", result.request_rate);
    eprintln!(
        "  Latency p50:    {:.1}ms",
        result.latency_p50.as_secs_f64() * 1000.0
    );
    eprintln!(
        "  Latency p99:    {:.1}ms",
        result.latency_p99.as_secs_f64() * 1000.0
    );
}
