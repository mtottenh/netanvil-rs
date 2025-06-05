use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Top-level test configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    /// Target URL(s)
    pub targets: Vec<String>,
    /// Test duration
    pub duration: Duration,
    /// Rate configuration
    pub rate: RateConfig,
    /// Number of worker cores (0 = auto-detect)
    pub num_cores: usize,
    /// Connection settings
    pub connections: ConnectionConfig,
    /// How often workers send metrics snapshots to the coordinator
    pub metrics_interval: Duration,
    /// How often the coordinator runs its control loop
    pub control_interval: Duration,
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
            duration: Duration::from_secs(10),
            rate: RateConfig::Static { rps: 100.0 },
            num_cores: 0,
            connections: ConnectionConfig::default(),
            metrics_interval: Duration::from_millis(500),
            control_interval: Duration::from_millis(100),
        }
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
