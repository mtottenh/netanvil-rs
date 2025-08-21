use std::time::Duration;

use netanvil_types::SaturationInfo;
use serde::Serialize;

/// Final test results returned after a load test completes.
///
/// Serializable to JSON for machine-readable output (`--output json`).
/// Duration fields are serialized as fractional seconds for easy consumption.
#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    pub total_requests: u64,
    pub total_errors: u64,
    #[serde(serialize_with = "ser_duration_secs")]
    pub duration: Duration,
    #[serde(serialize_with = "ser_duration_secs")]
    pub latency_p50: Duration,
    #[serde(serialize_with = "ser_duration_secs")]
    pub latency_p90: Duration,
    #[serde(serialize_with = "ser_duration_secs")]
    pub latency_p99: Duration,
    #[serde(serialize_with = "ser_duration_secs")]
    pub latency_max: Duration,
    pub request_rate: f64,
    pub error_rate: f64,
    /// Total bytes sent across all cores.
    pub total_bytes_sent: u64,
    /// Total bytes received across all cores.
    pub total_bytes_received: u64,
    /// Send throughput in megabits per second.
    pub throughput_send_mbps: f64,
    /// Receive throughput in megabits per second.
    pub throughput_recv_mbps: f64,
    /// Client/server saturation assessment over the entire test.
    pub saturation: SaturationInfo,
}

fn ser_duration_secs<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}
