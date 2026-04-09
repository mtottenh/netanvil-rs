//! Core traits and types for the netanvil-rs load testing framework.
//!
//! This crate defines the contracts between all other crates.
//! It has no runtime dependency (no compio, no tokio).

pub mod command;
pub mod config;
pub mod controller;
pub mod distributed;
pub mod distribution;
pub mod dns_spec;
pub mod error;
pub mod metrics;
pub mod node;
pub mod redis_spec;
pub mod request;
pub mod tcp_spec;
pub mod traits;
pub mod udp_spec;

pub use command::{ScheduledRequest, TimerCommand, WorkerCommand};
pub use config::{
    BackoffConfig, BaselineMultiplier, BoundsConfig, CongestionAvoidanceConfig, ConnectionConfig,
    ConnectionPolicy, ConstraintClassConfig, ConstraintConfig, CooldownPolicyConfig,
    EventLogOutput, ExternalConstraintConfig, ExternalMetricRef, FloorPolicyConfig, GainsConfig,
    HttpVersion, IncreasePolicyConfig, InternalMetric, MetricRef, MissingSignalBehavior,
    PluginConfig,
    PluginType, ProtocolConfig, RateConfig, RateChangeLimitsConfig, ResponseSignalConfig,
    SchedulerConfig, SetpointConstraintConfig, SignalAggregation, SignalDirection, SmootherConfig,
    TargetMetric, TestConfig, ThresholdConstraintConfig, ThresholdSource, TlsConfig, WarmupConfig,
};
pub use controller::{ControllerInfo, ControllerType, ControllerView, HoldCommand, HoldState};
pub use distributed::{MetricsFetcher, NodeCommander, NodeDiscovery, RemoteMetrics};
pub use distribution::{CountDistribution, ValueDistribution, WeightedValue};
pub use dns_spec::{DnsQueryType, DnsRequestSpec};
pub use error::{NetAnvilError, Result};
pub use metrics::{
    CpuAffinityCounters, HealthCounters, MetricsSnapshot, MetricsSummary, PacketCounterDeltas,
    RateDecision, SaturationAssessment, SaturationInfo, TcpHealthCounters, TcpHealthSnapshot,
};
pub use node::{NodeId, NodeInfo, NodeState};
pub use redis_spec::RedisRequestSpec;
pub use request::{
    ExecutionError, ExecutionResult, HttpRequestSpec, ProtocolSpec, RequestContext, TimingBreakdown,
};
pub use tcp_spec::{TcpFraming, TcpRequestSpec, TcpTestMode};
pub use traits::{
    EventRecorder, HttpGenerator, HttpTransformer, MetricsCollector, NoopEventRecorder,
    NoopPacketSource, PacketCounterSource, RateController, RequestExecutor, RequestGenerator,
    RequestScheduler, RequestTransformer,
};
pub use udp_spec::UdpRequestSpec;

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
            sent_time: now,
            dispatch_time: now,
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
        let spec = HttpRequestSpec {
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
        let config = TestConfig {
            rate: RateConfig::Static { rps: 500.0 },
            ..Default::default()
        };
        assert_eq!(config.initial_rps(), 500.0);

        let config = TestConfig {
            rate: RateConfig::Step {
                steps: vec![
                    (Duration::from_secs(0), 100.0),
                    (Duration::from_secs(5), 200.0),
                ],
            },
            ..Default::default()
        };
        assert_eq!(config.initial_rps(), 100.0);
    }

    #[test]
    fn metrics_summary_default() {
        let s = MetricsSummary::default();
        assert_eq!(s.total_requests, 0);
        assert_eq!(s.error_rate, 0.0);
    }

    // Compile test: RequestScheduler requires Send (for timer thread architecture).
    // Box<dyn RequestScheduler> must be Send.
    fn _assert_box_scheduler_is_send(s: Box<dyn RequestScheduler>) {
        fn assert_send<T: Send>(_: &T) {}
        assert_send(&s);
    }

    // Compile test: other hot-path traits remain !Send (Rc-based sharing on I/O workers).
    fn _assert_rc_generator_compiles(g: Rc<HttpGenerator>) {
        let _ = g;
    }

    #[test]
    fn node_info_serde_roundtrip() {
        let info = NodeInfo {
            id: NodeId("agent-1".into()),
            addr: "10.0.0.1:9090".into(),
            cores: 8,
            state: NodeState::Running,
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: NodeInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, info.id);
        assert_eq!(parsed.cores, 8);
        assert_eq!(parsed.state, NodeState::Running);
    }

    #[test]
    fn remote_metrics_serde_roundtrip() {
        let m = RemoteMetrics {
            node_id: NodeId("agent-1".into()),
            current_rps: 500.0,
            target_rps: 500.0,
            total_requests: 10000,
            total_errors: 5,
            error_rate: 0.0005,
            latency_p50_ms: 2.5,
            latency_p90_ms: 8.0,
            latency_p99_ms: 25.0,
            timeout_count: 0,
            in_flight_drops: 0,
            in_flight_count: 0,
            in_flight_capacity: 0,
        };
        let json = serde_json::to_string(&m).unwrap();
        let parsed: RemoteMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.total_requests, 10000);
        assert!((parsed.latency_p99_ms - 25.0).abs() < 0.01);
    }

    #[test]
    fn target_metric_external_serde_roundtrip() {
        let metric = TargetMetric::External {
            name: "load".into(),
        };
        let json = serde_json::to_string(&metric).unwrap();
        let parsed: TargetMetric = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, TargetMetric::External { name } if name == "load"));
    }
}
