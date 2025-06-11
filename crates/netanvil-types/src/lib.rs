//! Core traits and types for the netanvil-rs load testing framework.
//!
//! This crate defines the contracts between all other crates.
//! It has no runtime dependency (no compio, no tokio).

pub mod command;
pub mod config;
pub mod error;
pub mod metrics;
pub mod request;
pub mod traits;

pub use command::WorkerCommand;
pub use config::{
    ConnectionConfig, PidTarget, RateConfig, SchedulerConfig, TargetMetric, TestConfig,
};
pub use error::{NetAnvilError, Result};
pub use metrics::{MetricsSnapshot, MetricsSummary, RateDecision};
pub use request::{ExecutionError, ExecutionResult, RequestContext, RequestSpec, TimingBreakdown};
pub use traits::{
    MetricsCollector, RateController, RequestExecutor, RequestGenerator, RequestScheduler,
    RequestTransformer,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    #[test]
    fn request_context_construction() {
        let now = Instant::now();
        let ctx = RequestContext {
            request_id: 42,
            intended_time: now,
            actual_time: now,
            core_id: 0,
            is_sampled: false,
            session_id: None,
        };
        assert_eq!(ctx.request_id, 42);
        assert_eq!(ctx.core_id, 0);
        assert!(!ctx.is_sampled);
    }

    #[test]
    fn request_spec_clone() {
        let spec = RequestSpec {
            method: http::Method::POST,
            url: "http://example.com".to_string(),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: Some(b"{}".to_vec()),
        };
        let clone = spec.clone();
        assert_eq!(clone.url, "http://example.com");
        assert_eq!(clone.headers.len(), 1);
    }

    #[test]
    fn test_config_serde_roundtrip() {
        let config = TestConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: TestConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.targets, config.targets);
        assert_eq!(parsed.num_cores, config.num_cores);
    }

    #[test]
    fn test_config_initial_rps() {
        let mut config = TestConfig::default();
        config.rate = RateConfig::Static { rps: 500.0 };
        assert_eq!(config.initial_rps(), 500.0);

        config.rate = RateConfig::Step {
            steps: vec![
                (Duration::from_secs(0), 100.0),
                (Duration::from_secs(5), 200.0),
            ],
        };
        assert_eq!(config.initial_rps(), 100.0);
    }

    #[test]
    fn metrics_summary_default() {
        let s = MetricsSummary::default();
        assert_eq!(s.total_requests, 0);
        assert_eq!(s.error_rate, 0.0);
    }

    // Compile test: traits have no Send bound, so Rc<dyn Trait> must compile.
    // If traits required Send, this would fail because Rc is !Send.
    fn _assert_rc_scheduler_compiles(s: Rc<dyn RequestScheduler>) {
        let _ = s;
    }

    fn _assert_rc_generator_compiles(g: Rc<dyn RequestGenerator>) {
        let _ = g;
    }
}
