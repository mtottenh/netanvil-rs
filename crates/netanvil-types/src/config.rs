use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Top-level test configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    /// Target URL(s)
    pub targets: Vec<String>,
    /// HTTP method
    pub method: String,
    /// Test duration
    pub duration: Duration,
    /// Rate configuration
    pub rate: RateConfig,
    /// Scheduler type
    pub scheduler: SchedulerConfig,
    /// Headers to add to every request
    pub headers: Vec<(String, String)>,
    /// Number of worker cores (0 = auto-detect)
    pub num_cores: usize,
    /// Connection settings
    pub connections: ConnectionConfig,
    /// How often workers send metrics snapshots to the coordinator
    pub metrics_interval: Duration,
    /// How often the coordinator runs its control loop
    pub control_interval: Duration,
    /// HTTP status codes >= this threshold count as errors in metrics.
    /// Set to 0 to disable HTTP error classification (only transport errors count).
    /// Default: 400 (all 4xx and 5xx are errors).
    pub error_status_threshold: u16,
}

impl TestConfig {
    /// Get the initial RPS from the rate config.
    pub fn initial_rps(&self) -> f64 {
        match &self.rate {
            RateConfig::Static { rps } => *rps,
            RateConfig::Step { steps } => {
                steps.first().map(|(_, rps)| *rps).unwrap_or(100.0)
            }
            RateConfig::Pid { initial_rps, .. } => *initial_rps,
        }
    }
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            targets: vec!["http://localhost:8080".to_string()],
            method: "GET".to_string(),
            duration: Duration::from_secs(10),
            rate: RateConfig::Static { rps: 100.0 },
            scheduler: SchedulerConfig::default(),
            headers: Vec::new(),
            num_cores: 0,
            connections: ConnectionConfig::default(),
            metrics_interval: Duration::from_millis(500),
            control_interval: Duration::from_millis(100),
            error_status_threshold: 400,
        }
    }
}

/// Which scheduling discipline to use for request timing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SchedulerConfig {
    /// Fixed intervals: exactly 1/rate between each request.
    ConstantRate,
    /// Poisson process: exponentially distributed inter-arrival times.
    /// Models realistic independent user arrivals.
    Poisson {
        /// Optional seed for deterministic replay.
        seed: Option<u64>,
    },
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self::ConstantRate
    }
}

/// How request rate is controlled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RateConfig {
    /// Constant request rate.
    Static { rps: f64 },
    /// Rate changes at specified time offsets. Vec of (offset_from_start, rps).
    Step { steps: Vec<(Duration, f64)> },
    /// PID controller targeting a metric.
    Pid {
        initial_rps: f64,
        target: PidTarget,
    },
}

/// PID controller target configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidTarget {
    pub metric: TargetMetric,
    /// Target value for the metric (e.g., 200ms for latency)
    pub value: f64,
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
    /// Min/max RPS bounds
    pub min_rps: f64,
    pub max_rps: f64,
}

/// Which metric the PID controller targets.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TargetMetric {
    LatencyP50,
    LatencyP90,
    LatencyP99,
    ErrorRate,
}

/// Connection pool and timeout settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    /// Maximum concurrent connections per core
    pub max_connections_per_core: usize,
    /// TCP connect timeout
    pub connect_timeout: Duration,
    /// Per-request timeout
    pub request_timeout: Duration,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            max_connections_per_core: 100,
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
        }
    }
}
